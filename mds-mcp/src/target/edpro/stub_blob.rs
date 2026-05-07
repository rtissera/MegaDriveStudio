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

/// Magic at offset 0 of the header: 'MDST', big-endian.
pub const HEADER_MAGIC: u32 = 0x4D44_5354;

/// Embedded blob, baked into the binary at compile time.
pub const STUB_BLOB: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../mds-stub-68k/mdsstub.bin"));

/// Minimum sane blob size: 16-byte header + non-trivial code.
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
    /// Reserved field at offset 0x0C wasn't zero — newer header version we
    /// don't know how to parse.
    UnknownHeaderRevision(u32),
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
            Self::UnknownHeaderRevision(v) => {
                write!(f, "stub blob header reserved field = 0x{v:08X} (expected 0); rebuild?")
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

/// Parsed 16-byte blob header. Entry-point fields are 68k bus addresses
/// (the running CPU jumps to them on the relevant exception).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StubHeader {
    pub entry_trace: u32,
    pub entry_trap1: u32,
}

/// Validate `blob` and parse its 16-byte header. Pure function — no I/O.
pub fn parse_header(blob: &[u8]) -> Result<StubHeader, DeployError> {
    if blob.len() < MIN_BLOB_SIZE || blob.len() > MAX_BLOB_SIZE {
        return Err(DeployError::BadBlobSize(blob.len()));
    }
    let magic = be_u32(&blob[0..4]);
    if magic != HEADER_MAGIC {
        return Err(DeployError::BadMagic(magic));
    }
    let entry_trace = be_u32(&blob[4..8]);
    let entry_trap1 = be_u32(&blob[8..12]);
    let reserved = be_u32(&blob[12..16]);
    if reserved != 0 {
        return Err(DeployError::UnknownHeaderRevision(reserved));
    }
    Ok(StubHeader {
        entry_trace,
        entry_trap1,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;

    /// Pre-load `n` per-chunk OK ack bytes (for `mem_write` ack-mode).
    fn ack_bytes(n: usize) -> Vec<u8> {
        vec![0u8; n]
    }

    /// Hand-build a minimal valid header + filler so size > MIN.
    fn synth_blob(entry_trace: u32, entry_trap1: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&HEADER_MAGIC.to_be_bytes());
        v.extend_from_slice(&entry_trace.to_be_bytes());
        v.extend_from_slice(&entry_trap1.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
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
        assert_ne!(hdr.entry_trace, hdr.entry_trap1);
    }

    #[test]
    fn parse_header_synth_ok() {
        let blob = synth_blob(0x0030_0C4C, 0x0030_0C68);
        let hdr = parse_header(&blob).unwrap();
        assert_eq!(hdr.entry_trace, 0x0030_0C4C);
        assert_eq!(hdr.entry_trap1, 0x0030_0C68);
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
    fn parse_header_rejects_nonzero_reserved() {
        let mut blob = synth_blob(1, 2);
        blob[12..16].copy_from_slice(&0xDEAD_BEEF_u32.to_be_bytes());
        match parse_header(&blob) {
            Err(DeployError::UnknownHeaderRevision(0xDEAD_BEEF)) => {}
            other => panic!("expected UnknownHeaderRevision, got {other:?}"),
        }
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
