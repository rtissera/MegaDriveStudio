// SPDX-License-Identifier: MIT
//! Embed + deploy the on-cart 68k debug stub.
//!
//! The stub is built from `mds-stub-68k/` as a flat binary blob
//! (`mdsstub.bin`) linked into cart PSRAM at a fixed offset (default
//! [`STUB_LOAD_ADDR`] = `PiBusAddr(0x300000)`, the 3 MB mark — beyond
//! typical 1-2 MB user ROM images). Its data lives in 68k work RAM at
//! [`STUB_BSS_ADDR`] (= `M68kBusAddr(0xFFEE00)`, 512 bytes reserved).
//!
//! # Why PSRAM, not work RAM
//!
//! Per fact-check C13/C14/C26: `MEM_WR addr` is the cart PI-bus address
//! space, NOT the 68k bus. There is **no documented PI-bus alias for MD
//! work RAM (`$FF0000-$FFFFFF`)**. The previous design that uploaded the
//! stub to `$FF8000` via MEM_WR was simply writing into PSRAM offset
//! `0xFF8000` — i.e. somewhere inside the user's 16 MB ROM region — not
//! into work RAM at all. So the stub must live in PSRAM.
//!
//! 68k vector table (`$0000-$03FF`) IS reachable: it lives in PSRAM
//! because the 68k `$0..$3FFFFF` is mapped onto cart PSRAM. Writing
//! `PiBusAddr(0x24)` patches the Trace vector that the 68000 reads on
//! every Trace exception (fact-check C12/C27).
//!
//! # Deployment flow (caller is `EdProTarget::deploy_stub_then_handshake`)
//!
//! 1. `proto::host_reset(Soft)` — halt CPU.
//! 2. `MEM_WR` blob to `STUB_LOAD_ADDR` (PSRAM).
//! 3. `MEM_WR` 4 bytes (entry_trace) to `PiBusAddr(0x24)`.
//! 4. `MEM_WR` 4 bytes (entry_trap1) to `PiBusAddr(0x84)`.
//! 5. `proto::host_reset(Off)` — release CPU.
//! 6. Wait for first stop reply, run gdb handshake.
//!
//! Steps 2..4 are implemented here. The rest belong to
//! `EdProTarget::deploy_stub_then_handshake`.

#![allow(dead_code)] // wired in via EdProTarget::connect, partially in M5.5b

use super::proto::{self, M68kBusAddr, PiBusAddr};
use super::usb::UsbTransport;

/// Where the stub is linked to live in cart PSRAM. Must match `ORIGIN`
/// of `psram_stub` in `mds-stub-68k/mdsstub.ld`. The 68k sees this same
/// PSRAM offset at 68k address `$300000` (cart-mapped ROM area).
pub const STUB_LOAD_ADDR: PiBusAddr = PiBusAddr(0x0030_0000);

/// Where the stub's BSS lives, expressed as a 68k bus address. The stub
/// references this region by absolute pointer (no relocation), and
/// zero-initialises it on first entry. 512 bytes reserved between the
/// SGDK heap (typically below `$FFD000`) and SGDK stack (`$FFFFFE`
/// growing down) — see `mds-stub-68k/mdsstub.ld`.
///
/// The host CANNOT pre-zero this from PI-bus (see C13). The stub's first
/// entry path zero-initialises BSS itself.
pub const STUB_BSS_ADDR: M68kBusAddr = M68kBusAddr(0x00FF_EE00);

/// Length of the BSS region (matches `mdsstub.ld`).
pub const STUB_BSS_LEN: u32 = 0x200;

/// 68k Trace exception vector (vector index 9, byte offset `9*4 = 0x24`)
/// expressed as a PI-bus address. Same numeric value because 68k `$0..`
/// is mapped to PSRAM `$0..`.
pub const VEC_TRACE: PiBusAddr = PiBusAddr(0x0000_0024);

/// 68k TRAP #1 vector (vector index 33, byte offset `33*4 = 0x84`).
pub const VEC_TRAP1: PiBusAddr = PiBusAddr(0x0000_0084);

/// 68k level-6 IRQ autovector — the VBL exception (vector index 30, byte
/// offset `30*4 = 0x78`). M5.9 hijacks this to inject a pause without
/// hardware support: the host patches it to point at the stub's VBL
/// thunk, which probes a "paused" flag and either chains to the user's
/// VBL handler (fast path) or enters the RSP loop (slow path).
pub const VEC_VBL: PiBusAddr = PiBusAddr(0x0000_0078);

/// Magic at offset 0 of the header: 'MDST', big-endian.
pub const HEADER_MAGIC: u32 = 0x4D44_5354;

/// Header byte offsets (must match `mds-stub-68k/src/entry.s` and
/// `docs/02-m5-architecture.md` §5.4).
pub const HEADER_OFF_MAGIC: u32 = 0x00;
pub const HEADER_OFF_ENTRY_TRACE: u32 = 0x04;
pub const HEADER_OFF_ENTRY_TRAP1: u32 = 0x08;
pub const HEADER_OFF_ENTRY_VBL: u32 = 0x0C;
pub const HEADER_OFF_PAUSED_FLAG: u32 = 0x10;
pub const HEADER_OFF_ORIGINAL_VBL: u32 = 0x14;
/// Total header size (M5.9 grew it from 16 → 24 bytes).
pub const HEADER_SIZE: usize = 0x18;

/// Embedded blob, baked into the binary at compile time.
pub const STUB_BLOB: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../mds-stub-68k/mdsstub.bin"));

/// Minimum sane blob size: 24-byte header + non-trivial code.
const MIN_BLOB_SIZE: usize = 32;

/// Maximum sane blob size: 16 KiB (the linker `MEMORY` window for the
/// PSRAM stub region).
const MAX_BLOB_SIZE: usize = 16 * 1024;

/// Errors specific to stub deployment.
#[derive(Debug)]
pub enum DeployError {
    /// Blob is empty / truncated / impossibly large.
    BadBlobSize(usize),
    /// Header magic didn't match `'MDST'` — wrong file or build mismatch.
    BadMagic(u32),
    /// Header `paused_flag` (offset 0x10) or `original_vbl` (0x14) wasn't
    /// zero in the freshly-built blob — this indicates a stale build or a
    /// header revision we don't understand.
    UnknownHeaderRevision { paused_flag: u32, original_vbl: u32 },
    /// Wrapped USB transport error.
    Transport(anyhow::Error),
}

impl std::fmt::Display for DeployError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadBlobSize(n) => {
                write!(f, "stub blob size {n} bytes outside [{MIN_BLOB_SIZE}, {MAX_BLOB_SIZE}]")
            }
            Self::BadMagic(m) => write!(
                f,
                "stub blob bad magic 0x{m:08X} (want 0x{HEADER_MAGIC:08X} 'MDST')"
            ),
            Self::UnknownHeaderRevision { paused_flag, original_vbl } => {
                write!(
                    f,
                    "stub blob header init slots non-zero: paused_flag=0x{paused_flag:08X} original_vbl=0x{original_vbl:08X} (expected both 0); rebuild?"
                )
            }
            Self::Transport(e) => write!(f, "transport: {e}"),
        }
    }
}

impl std::error::Error for DeployError {}

impl From<anyhow::Error> for DeployError {
    fn from(e: anyhow::Error) -> Self {
        Self::Transport(e)
    }
}

/// Parsed 24-byte blob header. Entry-point fields are 68k bus addresses
/// (the running CPU jumps to them on the relevant exception).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StubHeader {
    pub entry_trace: u32,
    pub entry_trap1: u32,
    /// M5.9 VBL hijack entry — host installs at vector `$78` to arm a
    /// pause from a running CPU.
    pub entry_vbl: u32,
}

/// Validate `blob` and parse its 24-byte header. Pure function — no I/O.
pub fn parse_header(blob: &[u8]) -> Result<StubHeader, DeployError> {
    if blob.len() < MIN_BLOB_SIZE || blob.len() > MAX_BLOB_SIZE {
        return Err(DeployError::BadBlobSize(blob.len()));
    }
    if blob.len() < HEADER_SIZE {
        return Err(DeployError::BadBlobSize(blob.len()));
    }
    let magic = be_u32(&blob[0..4]);
    if magic != HEADER_MAGIC {
        return Err(DeployError::BadMagic(magic));
    }
    let entry_trace = be_u32(&blob[4..8]);
    let entry_trap1 = be_u32(&blob[8..12]);
    let entry_vbl = be_u32(&blob[12..16]);
    let paused_flag = be_u32(&blob[16..20]);
    let original_vbl = be_u32(&blob[20..24]);
    if paused_flag != 0 || original_vbl != 0 {
        return Err(DeployError::UnknownHeaderRevision { paused_flag, original_vbl });
    }
    Ok(StubHeader {
        entry_trace,
        entry_trap1,
        entry_vbl,
    })
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Upload `blob` to [`STUB_LOAD_ADDR`] and patch vectors `$24` / `$84`.
///
/// Caller is responsible for halting the CPU before calling this and for
/// releasing it afterwards. Per fact-check C20: krikzz convention is to
/// HOST_RST(Soft) first, MEM_WR, then HOST_RST(Off); concurrent MEM_WR
/// while the CPU runs is undocumented and unverified.
pub async fn deploy<T: UsbTransport>(t: &mut T, blob: &[u8]) -> Result<StubHeader, DeployError> {
    let hdr = parse_header(blob)?;

    // 1. Upload the whole blob to cart PSRAM. mem_write chunks to 1 KiB
    //    internally and reads one ack byte before each chunk because
    //    STUB_LOAD_ADDR is in the ack-gated region (PI-bus < 0x180_0000).
    proto::mem_write(t, STUB_LOAD_ADDR, blob)
        .await
        .map_err(DeployError::Transport)?;

    // 2. Patch vector $24 with entry_trace (4 bytes BE).
    proto::mem_write(t, VEC_TRACE, &hdr.entry_trace.to_be_bytes())
        .await
        .map_err(DeployError::Transport)?;

    // 3. Patch vector $84 with entry_trap1 (4 bytes BE).
    proto::mem_write(t, VEC_TRAP1, &hdr.entry_trap1.to_be_bytes())
        .await
        .map_err(DeployError::Transport)?;

    Ok(hdr)
}

/// M5.9: arm a pause-while-running by hijacking the 68k VBL vector.
///
/// Sequence (host-side; see `docs/02-m5-architecture.md` §5.9):
///
/// 1. `MEM_RD PiBus($78)` → snapshot the user's existing VBL handler.
/// 2. `MEM_WR PiBus(blob_load_addr + 0x14)` ← that snapshot, so the
///    stub's fast path can chain to it on every VBL until the flag fires.
/// 3. `MEM_WR PiBus(blob_load_addr + 0x10)` ← `[0,0,0,1]` — set the
///    paused-request flag.
/// 4. `MEM_WR PiBus($78)` ← `entry_vbl` from the parsed header — patch
///    the VBL vector to land in our thunk on the next IRQ.
///
/// Returns the captured original VBL handler addr, which the caller
/// MUST stash and pass to [`unpause`] for the resume path.
///
/// **Pre-conditions**:
/// - The user's ROM must have VBL ints enabled (SGDK does so by default
///   in `SYS_doVBlankProcess` flow). If VBL is masked at the SR level,
///   the pause flag will never be observed and pause becomes a no-op
///   until the user's code re-enables it.
/// - The CPU does NOT need to be halted: this is the entire reason the
///   path exists. We touch four PSRAM longs via PI-bus MEM_RD/MEM_WR;
///   per fact-check C20, concurrent MEM_WR while running is
///   undocumented but PSRAM-targeted writes are safe in practice (the
///   ED Pro MCU arbitrates the PI-bus while the 68k continues to run).
pub async fn pause<T: UsbTransport>(
    t: &mut T,
    blob_load_addr: u32,
    entry_vbl: u32,
) -> Result<u32, DeployError> {
    // 1. Snapshot the existing $78 handler.
    let snap = proto::mem_read(t, VEC_VBL, 4)
        .await
        .map_err(DeployError::Transport)?;
    if snap.len() != 4 {
        return Err(DeployError::Transport(anyhow::anyhow!(
            "MEM_RD VEC_VBL returned {} bytes (want 4)",
            snap.len()
        )));
    }
    let original_vbl = u32::from_be_bytes([snap[0], snap[1], snap[2], snap[3]]);

    // 2. Stash original_vbl into header offset 0x14 so the fast path
    //    can chain to it.
    proto::mem_write(
        t,
        PiBusAddr(blob_load_addr + HEADER_OFF_ORIGINAL_VBL),
        &original_vbl.to_be_bytes(),
    )
    .await
    .map_err(DeployError::Transport)?;

    // 3. Set paused_flag = 1 (header offset 0x10).
    proto::mem_write(
        t,
        PiBusAddr(blob_load_addr + HEADER_OFF_PAUSED_FLAG),
        &1u32.to_be_bytes(),
    )
    .await
    .map_err(DeployError::Transport)?;

    // 4. Patch vector $78 → entry_vbl. After this point, the very next
    //    VBL fires our thunk; it sees paused_flag != 0 and enters RSP.
    proto::mem_write(t, VEC_VBL, &entry_vbl.to_be_bytes())
        .await
        .map_err(DeployError::Transport)?;

    Ok(original_vbl)
}

/// M5.9: unwind the pause hijack. Order matters here — we clear the
/// `paused_flag` BEFORE restoring vector `$78` so that any racing VBL
/// (between the two writes) lands in our stub but takes the fast-path
/// chain to the original handler instead of re-entering the RSP loop.
///
/// 1. `MEM_WR PiBus(blob_load_addr + 0x10)` ← `[0,0,0,0]` (clear flag)
/// 2. `MEM_WR PiBus($78)` ← `original_vbl` (restore the user's handler)
///
/// Doesn't tell the stub to resume — that's a separate RSP `c` packet
/// the caller (`StubSync::unpause` / `EdProTarget::resume`) sends. The
/// order is therefore: `unpause()` (this fn) first, then RSP `c`. By
/// the time the `c` reaches the stub, vector `$78` is already restored
/// and a subsequent VBL will not trip the stub again.
pub async fn unpause<T: UsbTransport>(
    t: &mut T,
    blob_load_addr: u32,
    original_vbl: u32,
) -> Result<(), DeployError> {
    proto::mem_write(
        t,
        PiBusAddr(blob_load_addr + HEADER_OFF_PAUSED_FLAG),
        &0u32.to_be_bytes(),
    )
    .await
    .map_err(DeployError::Transport)?;
    proto::mem_write(t, VEC_VBL, &original_vbl.to_be_bytes())
        .await
        .map_err(DeployError::Transport)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;

    /// Pre-load `n` per-chunk OK ack bytes (for `mem_write` ack-mode).
    fn ack_bytes(n: usize) -> Vec<u8> {
        vec![0u8; n]
    }

    /// Hand-build a minimal valid header + filler so size > MIN. The
    /// 24-byte header (M5.9) is: magic, entry_trace, entry_trap1,
    /// entry_vbl, paused_flag (=0), original_vbl (=0).
    fn synth_blob(entry_trace: u32, entry_trap1: u32) -> Vec<u8> {
        synth_blob_full(entry_trace, entry_trap1, 0x0030_0A1C, 0, 0)
    }

    /// As [`synth_blob`] but lets the test set the paused/original_vbl
    /// init slots, e.g. to verify `parse_header` rejects non-zero init.
    fn synth_blob_full(
        entry_trace: u32,
        entry_trap1: u32,
        entry_vbl: u32,
        paused_flag: u32,
        original_vbl: u32,
    ) -> Vec<u8> {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&HEADER_MAGIC.to_be_bytes());
        v.extend_from_slice(&entry_trace.to_be_bytes());
        v.extend_from_slice(&entry_trap1.to_be_bytes());
        v.extend_from_slice(&entry_vbl.to_be_bytes());
        v.extend_from_slice(&paused_flag.to_be_bytes());
        v.extend_from_slice(&original_vbl.to_be_bytes());
        v.resize(64, 0xCD);
        v
    }

    #[test]
    fn embedded_blob_has_valid_header() {
        // The build.rs makes this binary alongside cargo build; if the
        // include_bytes! payload is wrong, this is the canary.
        let hdr = parse_header(STUB_BLOB).expect("embedded blob must parse");
        // Entry points must lie inside the PSRAM stub window
        // [STUB_LOAD_ADDR, STUB_LOAD_ADDR + MAX_BLOB_SIZE).
        let load = STUB_LOAD_ADDR.0;
        assert!(
            hdr.entry_trace >= load && hdr.entry_trace < load + MAX_BLOB_SIZE as u32,
            "entry_trace 0x{:08X} not in [0x{:08X}, +0x{:X})",
            hdr.entry_trace,
            load,
            MAX_BLOB_SIZE,
        );
        assert!(
            hdr.entry_trap1 >= load && hdr.entry_trap1 < load + MAX_BLOB_SIZE as u32,
        );
        assert!(
            hdr.entry_vbl >= load && hdr.entry_vbl < load + MAX_BLOB_SIZE as u32,
            "entry_vbl 0x{:08X} not in PSRAM window",
            hdr.entry_vbl
        );
        assert_ne!(hdr.entry_trace, hdr.entry_trap1);
        assert_ne!(hdr.entry_trace, hdr.entry_vbl);
        assert_ne!(hdr.entry_trap1, hdr.entry_vbl);
    }

    #[test]
    fn parse_header_synth_ok() {
        let blob = synth_blob(0x0030_0C4C, 0x0030_0C68);
        let hdr = parse_header(&blob).unwrap();
        assert_eq!(hdr.entry_trace, 0x0030_0C4C);
        assert_eq!(hdr.entry_trap1, 0x0030_0C68);
        // synth_blob defaults entry_vbl to 0x0030_0A1C (current build's value).
        assert_eq!(hdr.entry_vbl, 0x0030_0A1C);
    }

    #[test]
    fn parse_header_rejects_bad_magic() {
        let mut blob = synth_blob(0, 0);
        blob[0] = 0xFF;
        match parse_header(&blob) {
            Err(DeployError::BadMagic(_)) => {}
            other => panic!("expected BadMagic, got {other:?}"),
        }
    }

    #[test]
    fn parse_header_rejects_short_blob() {
        let blob = vec![0u8; 8];
        match parse_header(&blob) {
            Err(DeployError::BadBlobSize(8)) => {}
            other => panic!("expected BadBlobSize(8), got {other:?}"),
        }
    }

    #[test]
    fn parse_header_rejects_oversized_blob() {
        let blob = vec![0u8; MAX_BLOB_SIZE + 1];
        match parse_header(&blob) {
            Err(DeployError::BadBlobSize(_)) => {}
            other => panic!("expected BadBlobSize, got {other:?}"),
        }
    }

    #[test]
    fn parse_header_rejects_nonzero_init_slots() {
        // paused_flag != 0
        let blob = synth_blob_full(1, 2, 3, 0xDEAD_BEEF, 0);
        match parse_header(&blob) {
            Err(DeployError::UnknownHeaderRevision { paused_flag: 0xDEAD_BEEF, original_vbl: 0 }) => {}
            other => panic!("expected UnknownHeaderRevision (paused), got {other:?}"),
        }
        // original_vbl != 0
        let blob = synth_blob_full(1, 2, 3, 0, 0xCAFE_BABE);
        match parse_header(&blob) {
            Err(DeployError::UnknownHeaderRevision { paused_flag: 0, original_vbl: 0xCAFE_BABE }) => {}
            other => panic!("expected UnknownHeaderRevision (original_vbl), got {other:?}"),
        }
    }

    #[test]
    fn parse_header_extracts_entry_vbl() {
        let blob = synth_blob_full(0x0030_0010, 0x0030_0020, 0x0030_0030, 0, 0);
        let hdr = parse_header(&blob).unwrap();
        assert_eq!(hdr.entry_trace, 0x0030_0010);
        assert_eq!(hdr.entry_trap1, 0x0030_0020);
        assert_eq!(hdr.entry_vbl, 0x0030_0030);
    }

    #[tokio::test]
    async fn deploy_emits_blob_then_vector_patches() {
        let blob = synth_blob(0x0030_0C4C, 0x0030_0C68);
        // 64 bytes -> 1 chunk for the blob upload (1 ack), then 4 bytes
        // for each vector patch (1 ack each). Total 3 acks.
        let mut m = MockUsb::with_replies(vec![ack_bytes(3)]);
        let hdr = deploy(&mut m, &blob).await.unwrap();
        assert_eq!(hdr.entry_trace, 0x0030_0C4C);
        assert_eq!(hdr.entry_trap1, 0x0030_0C68);

        // Three MEM_WR commands: blob upload, vector $24, vector $84.
        let frames = m.tx_frames();
        // Frame 0: blob header (cmd + addr + len + ack mode byte)
        assert_eq!(&frames[0][..4], &[0x2B, 0xD4, 0x1A, 0xE5]);
        assert_eq!(&frames[0][4..8], &STUB_LOAD_ADDR.0.to_be_bytes());
        assert_eq!(&frames[0][8..12], &(blob.len() as u32).to_be_bytes());

        // Walk MEM_WR headers in tx order.
        let headers: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.len() == 13 && f[0..4] == [0x2B, 0xD4, 0x1A, 0xE5])
            .map(|v| v.as_slice())
            .collect();
        assert_eq!(headers.len(), 3);
        assert_eq!(&headers[0][4..8], &STUB_LOAD_ADDR.0.to_be_bytes());
        assert_eq!(&headers[1][4..8], &VEC_TRACE.0.to_be_bytes());
        assert_eq!(&headers[2][4..8], &VEC_TRAP1.0.to_be_bytes());
        // Vector patches are 4 bytes each.
        assert_eq!(&headers[1][8..12], &4u32.to_be_bytes());
        assert_eq!(&headers[2][8..12], &4u32.to_be_bytes());
    }

    // ---- M5.9: pause / unpause helpers --------------------------------

    #[tokio::test]
    async fn pause_emits_read_then_three_writes() {
        // The host sequence is: MEM_RD $78 → MEM_WR origvbl → MEM_WR
        // flag → MEM_WR vector $78. MEM_RD has no per-chunk ack;
        // MEM_WR for header writes goes to PSRAM (ack-mode).
        let original_vbl: u32 = 0x0010_5678;
        let blob_load: u32 = 0x0030_0000;
        let entry_vbl: u32 = 0x0030_0A1C;

        // Replies: 4 bytes for the MEM_RD answer, then one ack byte for
        // EACH of the three subsequent MEM_WRs (4 bytes each, single chunk).
        let mut m = MockUsb::with_replies(vec![
            original_vbl.to_be_bytes().to_vec(),
            vec![0u8],
            vec![0u8],
            vec![0u8],
        ]);
        let got = pause(&mut m, blob_load, entry_vbl).await.unwrap();
        assert_eq!(got, original_vbl);

        // Walk the MEM_RD / MEM_WR headers in tx order. MEM_WR header
        // is 13 bytes (4 cmd + 4 addr + 4 len + 1 mode); MEM_RD is 12.
        let frames = m.tx_frames();
        // Frame 0: MEM_RD $78, len 4.
        assert_eq!(&frames[0][..4], &[0x2B, 0xD4, 0x19, 0xE6]);
        assert_eq!(&frames[0][4..8], &VEC_VBL.0.to_be_bytes());
        assert_eq!(&frames[0][8..12], &4u32.to_be_bytes());

        // Subsequent MEM_WR headers, one per write.
        let wr_headers: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.len() == 13 && f[0..4] == [0x2B, 0xD4, 0x1A, 0xE5])
            .map(|v| v.as_slice())
            .collect();
        assert_eq!(wr_headers.len(), 3);
        // Order: original_vbl slot, paused_flag slot, vector $78.
        assert_eq!(
            &wr_headers[0][4..8],
            &(blob_load + HEADER_OFF_ORIGINAL_VBL).to_be_bytes()
        );
        assert_eq!(
            &wr_headers[1][4..8],
            &(blob_load + HEADER_OFF_PAUSED_FLAG).to_be_bytes()
        );
        assert_eq!(&wr_headers[2][4..8], &VEC_VBL.0.to_be_bytes());

        // The flag write should be `[0,0,0,1]`.
        let flag_chunk = &frames
            .iter()
            .filter(|f| f.len() == 4 && f.as_slice() == [0u8, 0, 0, 1])
            .count();
        assert_eq!(*flag_chunk, 1, "expected exactly one [0,0,0,1] payload");

        // The vector $78 write should be `entry_vbl` BE.
        let vbl_chunk_present = frames.iter().any(|f| {
            f.len() == 4 && f.as_slice() == entry_vbl.to_be_bytes()
        });
        assert!(vbl_chunk_present, "vector $78 write must contain entry_vbl");
    }

    #[tokio::test]
    async fn unpause_clears_flag_then_restores_vector() {
        let original_vbl: u32 = 0x0010_5678;
        let blob_load: u32 = 0x0030_0000;
        // Two MEM_WRs to PSRAM-region addrs, both ack-gated; one ack byte each.
        let mut m = MockUsb::with_replies(vec![vec![0u8], vec![0u8]]);
        unpause(&mut m, blob_load, original_vbl).await.unwrap();

        let frames = m.tx_frames();
        let wr_headers: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.len() == 13 && f[0..4] == [0x2B, 0xD4, 0x1A, 0xE5])
            .map(|v| v.as_slice())
            .collect();
        assert_eq!(wr_headers.len(), 2);
        // Clear flag FIRST, then restore vector $78. Order matters: see doc.
        assert_eq!(
            &wr_headers[0][4..8],
            &(blob_load + HEADER_OFF_PAUSED_FLAG).to_be_bytes()
        );
        assert_eq!(&wr_headers[1][4..8], &VEC_VBL.0.to_be_bytes());

        // Flag chunk = [0,0,0,0]; vector chunk = original_vbl BE.
        assert!(frames.iter().any(|f| f.as_slice() == [0u8, 0, 0, 0]));
        assert!(frames
            .iter()
            .any(|f| f.as_slice() == original_vbl.to_be_bytes()));
    }

    #[tokio::test]
    async fn pause_propagates_short_mem_rd() {
        // MEM_RD returns < 4 bytes → DeployError. Mock yields only 2 bytes
        // and `read_exact` will error out before pause() can interpret.
        // Confirm the helper surfaces a Transport error.
        let mut m = MockUsb::with_replies(vec![vec![0u8, 0u8]]);
        match pause(&mut m, 0x0030_0000, 0x0030_0A1C).await {
            Err(DeployError::Transport(_)) => {}
            other => panic!("expected Transport error, got {other:?}"),
        }
    }

    // Sanity-check the placement constants at compile time. Per
    // fact-check C13: PI-bus < 0x100_0000 = ROM/PSRAM region; the load
    // address must sit beyond typical 1-2 MB user ROM images. BSS must
    // be inside 68k work RAM ($FF0000-$FFFFFF).
    const _ASSERT_LOAD_IN_PSRAM: () = {
        assert!(STUB_LOAD_ADDR.0 < 0x100_0000);
        assert!(STUB_LOAD_ADDR.0 >= 0x20_0000);
    };
    const _ASSERT_BSS_IN_WORK_RAM: () = {
        assert!(STUB_BSS_ADDR.0 >= 0x00FF_0000);
        assert!(STUB_BSS_ADDR.0 + STUB_BSS_LEN <= 0x0100_0000);
    };
}
