// SPDX-License-Identifier: MIT
//! Embed + deploy the on-cart 68k debug stub.
//!
//! The stub is built from `mds-stub-68k/` as a flat binary blob
//! (`mdsstub.bin`) linked at a fixed work-RAM address (default
//! `$FF8000`). At debug-attach time we:
//!
//! 1. (caller) halt the CPU via `proto::host_reset`
//! 2. `MEM_WR` the entire blob to `STUB_LOAD_ADDR`
//! 3. parse the 16-byte header to discover the two exception entry
//!    points (Trace and TRAP #1)
//! 4. `MEM_WR` 4 bytes (entry_trace) to vector `$00000024`
//! 5. `MEM_WR` 4 bytes (entry_trap1) to vector `$00000084`
//! 6. (caller) release the CPU
//! 7. (caller) wait for first stop reply, then run the gdb handshake
//!
//! Steps 2..5 are implemented here. The rest belong to
//! `EdProTarget::connect`.
//!
//! See `mds-stub-68k/README.md` for the deployment contract and
//! `docs/02-m5-architecture.md` §5.6 for the full sequence.

#![allow(dead_code)] // wired in via EdProTarget::connect, partially in M5.5b

use super::proto;
use super::usb::UsbTransport;

/// Where the stub is linked to live. Must match `LOAD_ADDR` in
/// `mds-stub-68k/Makefile` and `ORIGIN` in `mds-stub-68k/mdsstub.ld`.
pub const STUB_LOAD_ADDR: u32 = 0x00FF_8000;

/// 68k Trace exception vector (vector index 9, byte offset 9*4 = 0x24).
pub const VEC_TRACE: u32 = 0x0000_0024;

/// 68k TRAP #1 vector (vector index 33, byte offset 33*4 = 0x84).
pub const VEC_TRAP1: u32 = 0x0000_0084;

/// Magic at offset 0 of the header: 'MDST', big-endian.
pub const HEADER_MAGIC: u32 = 0x4D44_5354;

/// Embedded blob, baked into the binary at compile time.
///
/// CARGO_MANIFEST_DIR points at `mds-mcp/`, the binary lives one level up
/// under `mds-stub-68k/mdsstub.bin`. Building the blob is the responsibility
/// of `cargo` callers — see `mds-mcp/build.rs` (added in this milestone)
/// which shells out to `make` in `mds-stub-68k/`.
pub const STUB_BLOB: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../mds-stub-68k/mdsstub.bin"));

/// Minimum sane blob size: 16-byte header + non-trivial code. Anything
/// shorter is a build-system bug.
const MIN_BLOB_SIZE: usize = 32;

/// Maximum sane blob size: 16 KiB (the linker `MEMORY` window). Larger
/// would risk colliding with SGDK heap/stack at the load address.
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

/// Parsed 16-byte blob header.
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

/// Upload `blob` to `STUB_LOAD_ADDR` and patch vectors `$24` / `$84`.
///
/// Caller is responsible for halting the CPU before calling this and for
/// releasing it afterwards. We do **not** issue `HOST_RST` from inside —
/// that's the connect-flow's job and conflates "just deploy the blob" with
/// "drive the full reset cycle".
pub async fn deploy<T: UsbTransport>(t: &mut T, blob: &[u8]) -> Result<StubHeader, DeployError> {
    let hdr = parse_header(blob)?;

    // 1. Upload the whole blob. mem_write chunks to 1 KiB internally and
    //    asks the cart to ACK each chunk because the load address is in
    //    ROM-mapped PSRAM (`addr < 0x180_0000`).
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

    /// Hand-build a minimal valid header + filler so size > MIN.
    fn synth_blob(entry_trace: u32, entry_trap1: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(64);
        v.extend_from_slice(&HEADER_MAGIC.to_be_bytes());
        v.extend_from_slice(&entry_trace.to_be_bytes());
        v.extend_from_slice(&entry_trap1.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.resize(64, 0xAA);
        v
    }

    #[test]
    fn embedded_blob_has_valid_header() {
        // The build.rs makes this binary alongside cargo build; if the
        // include_bytes! payload is wrong, this is the canary.
        let hdr = parse_header(STUB_BLOB).expect("embedded blob must parse");
        // Entry points must be inside the load region.
        assert!(
            hdr.entry_trace >= STUB_LOAD_ADDR
                && hdr.entry_trace < STUB_LOAD_ADDR + MAX_BLOB_SIZE as u32,
            "entry_trace 0x{:08X} not in [0x{:08X}, +0x{:X})",
            hdr.entry_trace,
            STUB_LOAD_ADDR,
            MAX_BLOB_SIZE,
        );
        assert!(
            hdr.entry_trap1 >= STUB_LOAD_ADDR
                && hdr.entry_trap1 < STUB_LOAD_ADDR + MAX_BLOB_SIZE as u32,
        );
        assert_ne!(hdr.entry_trace, hdr.entry_trap1);
    }

    #[test]
    fn parse_header_synth_ok() {
        let blob = synth_blob(0x00FF_8C4C, 0x00FF_8C68);
        let hdr = parse_header(&blob).unwrap();
        assert_eq!(hdr.entry_trace, 0x00FF_8C4C);
        assert_eq!(hdr.entry_trap1, 0x00FF_8C68);
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
        let blob = synth_blob(0x00FF_8C4C, 0x00FF_8C68);
        let mut m = MockUsb::new();
        let hdr = deploy(&mut m, &blob).await.unwrap();
        assert_eq!(hdr.entry_trace, 0x00FF_8C4C);
        assert_eq!(hdr.entry_trap1, 0x00FF_8C68);

        // Three MEM_WR commands: blob upload, vector $24, vector $84.
        // Each one is a (header frame + payload frames) pair via the
        // mem_write impl. We assert by looking at the per-frame log.
        let frames = m.tx_frames();
        // Frame 0: blob header (cmd + addr + len + ack)
        assert_eq!(&frames[0][..4], &[0x2B, 0xD4, 0x1A, 0xE5]);
        assert_eq!(&frames[0][4..8], &STUB_LOAD_ADDR.to_be_bytes());
        assert_eq!(&frames[0][8..12], &(blob.len() as u32).to_be_bytes());

        // After the blob upload (1 header + N data chunks), the next
        // header frame must target vector $24 then $84.
        let mut headers: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.len() == 13 && f[0..4] == [0x2B, 0xD4, 0x1A, 0xE5])
            .map(|v| v.as_slice())
            .collect();
        // Sort by appearance order is preserved by the filter.
        assert_eq!(headers.len(), 3);
        assert_eq!(&headers[0][4..8], &STUB_LOAD_ADDR.to_be_bytes());
        assert_eq!(&headers[1][4..8], &VEC_TRACE.to_be_bytes());
        assert_eq!(&headers[2][4..8], &VEC_TRAP1.to_be_bytes());
        // Vector patches are 4 bytes each.
        assert_eq!(&headers[1][8..12], &4u32.to_be_bytes());
        assert_eq!(&headers[2][8..12], &4u32.to_be_bytes());
        // Suppress unused-mut warning on stable.
        let _ = &mut headers;
    }
}
