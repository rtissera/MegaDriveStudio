// SPDX-License-Identifier: MIT
//! Typed EdPro USB opcodes + thin wrappers.
//!
//! Layered on top of [`super::usb::UsbTransport`] + [`super::framing`]. Every
//! function here is hardware-free — drive them with `MockUsb` in tests.
//!
//! # Address spaces
//!
//! There are TWO independent address spaces in play and they are NOT
//! interchangeable:
//!
//! - [`PiBusAddr`] — the cart's PI-bus / MCU address space. This is what
//!   `MEM_RD` / `MEM_WR` operate on. Map (per krikzz `everdrive.h`):
//!
//!   | base         | region | size  | notes                                 |
//!   |--------------|--------|-------|---------------------------------------|
//!   | `0x000_0000` | ROM    | 16 MB | PSRAM (writable). Mirrors 68k `$0..$3FFFFF` (up to mapper). |
//!   | `0x100_0000` | SRAM   | 512 KB| Cart save SRAM.                       |
//!   | `0x108_0000` | BRAM   | 512 KB| Battery RAM.                          |
//!   | `0x180_0000` | CFG    | —     | System config; ack-gating threshold.  |
//!   | `0x181_0000` | FIFO   | —     | MCU side of `$A130D0` mailbox.        |
//!   | `0x183_0000` | MAP    | —     | Mapper regs.                          |
//!
//! - [`M68kBusAddr`] — the running 68000's address space. RAM at
//!   `$FF0000-$FFFFFF`, MMIO at `$A130xx`, etc. The host CANNOT reach
//!   this directly through `MEM_WR`; the only path is via the on-cart
//!   stub speaking RSP `m`/`M` (see `stub_sync`).
//!
//! Vector table at `$0000-$03FF` lives in PSRAM (cart ROM area), so
//! `PiBusAddr(0x24)` aliases the 68k Trace vector — that's how the host
//! installs vectors.
//!
//! # Wire format reminder (host -> cart)
//!
//! ```text
//!     '+' ~'+' OP ~OP <op-specific payload, big-endian for u16/u32>
//! ```
//!
//! Citations refer to fact-check IDs from `/tmp/mds-edpro-factcheck.md`
//! (C5/C8/C11/C13/C14/C20/C26).

// Every wrapper here is reachable only from tests until M5.x routes
// `EdProTarget` tool methods through them.
#![allow(dead_code)]

use super::framing::encode_cmd;
use super::usb::UsbTransport;

// ---------------------------------------------------------------------------
// Typed addresses
// ---------------------------------------------------------------------------

/// Cart PI-bus address (the address space `MEM_RD` / `MEM_WR` operate on).
///
/// Distinct from [`M68kBusAddr`] at the type level so call sites that
/// confused the two (per fact-check C13/C14/C26) won't compile.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PiBusAddr(pub u32);

impl PiBusAddr {
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::LowerHex for PiBusAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, f)
    }
}

/// 68000 bus address as seen by the running CPU. Reachable from the host
/// only through the on-cart stub's RSP `m`/`M` packets, NOT through
/// `MEM_WR`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct M68kBusAddr(pub u32);

impl M68kBusAddr {
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::LowerHex for M68kBusAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, f)
    }
}

/// Cart PSRAM (16 MB, mirrors 68k ROM area `$000000-$3FFFFF`).
pub const PI_ROM_BASE: PiBusAddr = PiBusAddr(0x000_0000);
/// Cart save SRAM (512 KB).
pub const PI_SRAM_BASE: PiBusAddr = PiBusAddr(0x100_0000);
/// Battery RAM (512 KB).
pub const PI_BRAM_BASE: PiBusAddr = PiBusAddr(0x108_0000);
/// CFG region — first PI address that does NOT need MEM_WR ack-gating.
pub const PI_CFG_BASE: PiBusAddr = PiBusAddr(0x180_0000);
/// MCU side of the `$A130D0` USB mailbox.
pub const PI_FIFO_BASE: PiBusAddr = PiBusAddr(0x181_0000);
/// Mapper registers.
pub const PI_MAP_BASE: PiBusAddr = PiBusAddr(0x183_0000);

// ---------------------------------------------------------------------------
// Opcodes
// ---------------------------------------------------------------------------

/// EdPro command opcodes used by M5. See `/tmp/mds-edpro-factcheck.md` C6-C10.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Status = 0x10,
    MemRd = 0x19,
    MemWr = 0x1A,
    UsbWr = 0x22,
    HostRst = 0x29,
}

impl TryFrom<u8> for Opcode {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        Ok(match v {
            0x10 => Self::Status,
            0x19 => Self::MemRd,
            0x1A => Self::MemWr,
            0x22 => Self::UsbWr,
            0x29 => Self::HostRst,
            _ => return Err(v),
        })
    }
}

/// Reply to a `STATUS` (0x10) command. Cart sends `[0xA5, code]`. Old
/// firmwares may use a different key byte; M5.2 will decode the bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusReply {
    /// Always `0xA5` on healthy firmwares.
    pub key: u8,
    /// TODO M5.2: decode bits (idle / error / mode flags).
    pub code: u8,
}

const STATUS_KEY: u8 = 0xA5;

/// MEM_WR ack-mode header byte. Per fact-check C11: `0xAA` is the ONE-TIME
/// mode byte sent in the MEM_WR header to request per-chunk ack-gating;
/// it is NOT the per-chunk ack value. Mode is enabled when the target
/// PI-bus address is below [`PI_CFG_BASE`] (ROM/SRAM/BRAM region).
const MEM_WR_ACK_ON: u8 = 0xAA;
const MEM_WR_ACK_OFF: u8 = 0x00;
/// Per krikzz `ed_cmd_mem_wr` / megalink-rs `tx_ack`: 1 KiB chunk size for
/// ack-gated transfers.
const MEM_WR_BLOCK: usize = 1024;

/// Per-chunk ack value the cart sends BEFORE each 1 KiB chunk under
/// ack-mode. `0x00` = OK; any non-zero value is an error code from the
/// cart MCU. Per fact-check C11 / megalink-rs `tx_ack`.
const MEM_WR_PER_CHUNK_OK: u8 = 0x00;

/// Reset modes for `HOST_RST` (0x29). Values mirror krikzz everdrive.c.
#[allow(dead_code)] // M5.x will route real reset flows
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    /// Release reset — let the CPU run.
    Off = 0,
    /// Soft reset — halts CPU at reset vector. Krikzz's `load_game`
    /// pattern uses this before `MEM_WR` then `Off` after.
    Soft = 1,
    /// Hard reset.
    Hard = 2,
}

/// Send `STATUS` (0x10) and read back `[key, code]`.
pub async fn send_status<T: UsbTransport>(t: &mut T) -> anyhow::Result<StatusReply> {
    t.write_all(&encode_cmd(Opcode::Status as u8)).await?;
    let mut buf = [0u8; 2];
    t.read_exact(&mut buf).await?;
    if buf[0] != STATUS_KEY {
        anyhow::bail!("status: bad key byte 0x{:02X} (want 0xA5)", buf[0]);
    }
    Ok(StatusReply {
        key: buf[0],
        code: buf[1],
    })
}

/// Send `MEM_RD` (0x19) for the PI-bus range `[addr, addr+len)` and read
/// back `len` bytes. One-shot — no chunking yet (M5.2 will add streaming
/// for large reads).
///
/// `addr` is a [`PiBusAddr`]. To read 68k memory, route through the on-cart
/// stub's RSP `m` packet instead — `MEM_RD` cannot reach 68k work RAM
/// (fact-check C13).
pub async fn mem_read<T: UsbTransport>(
    t: &mut T,
    addr: PiBusAddr,
    len: u32,
) -> anyhow::Result<Vec<u8>> {
    let mut hdr = [0u8; 4 + 4 + 4];
    hdr[..4].copy_from_slice(&encode_cmd(Opcode::MemRd as u8));
    hdr[4..8].copy_from_slice(&addr.0.to_be_bytes());
    hdr[8..12].copy_from_slice(&len.to_be_bytes());
    t.write_all(&hdr).await?;

    let mut out = vec![0u8; len as usize];
    if !out.is_empty() {
        t.read_exact(&mut out).await?;
    }
    Ok(out)
}

/// Send `MEM_WR` (0x1A) — chunks payload into 1 KiB slabs. Per krikzz
/// `ed_cmd_mem_wr` / megalink-rs `tx_ack`:
///
/// 1. Header includes a one-time mode byte: `0xAA` for `addr < PI_CFG_BASE`
///    (PSRAM/SRAM/BRAM, the ack-gated region), `0x00` otherwise.
/// 2. Under ack-mode, BEFORE each 1 KiB chunk, the cart sends one byte:
///    `0x00 = OK`, any non-zero value is an error.
/// 3. The host then transmits the 1 KiB chunk.
///
/// `addr` is a [`PiBusAddr`]. There is no PI-bus alias for MD work RAM
/// (`$FF0000-$FFFFFF`); attempts to push state to the running 68k via
/// MEM_WR will land in PSRAM at the matching offset, NOT in work RAM
/// (fact-check C13/C14/C26).
///
/// **Caveat (fact-check C20):** the krikzz pattern is `HOST_RST(Soft)` →
/// `MEM_WR` → `HOST_RST(Off)`. Issuing `MEM_WR` while the 68k is running
/// is undocumented and unverified — always halt first.
pub async fn mem_write<T: UsbTransport>(
    t: &mut T,
    addr: PiBusAddr,
    data: &[u8],
) -> anyhow::Result<()> {
    let len = data.len() as u32;
    let want_ack = addr.0 < PI_CFG_BASE.0;
    let ack_byte = if want_ack { MEM_WR_ACK_ON } else { MEM_WR_ACK_OFF };

    let mut hdr = [0u8; 4 + 4 + 4 + 1];
    hdr[..4].copy_from_slice(&encode_cmd(Opcode::MemWr as u8));
    hdr[4..8].copy_from_slice(&addr.0.to_be_bytes());
    hdr[8..12].copy_from_slice(&len.to_be_bytes());
    hdr[12] = ack_byte;
    t.write_all(&hdr).await?;

    for chunk in data.chunks(MEM_WR_BLOCK) {
        if want_ack {
            // ACK arrives BEFORE the chunk is transmitted (per
            // megalink-rs `tx_ack`).
            let mut ack = [0u8; 1];
            t.read_exact(&mut ack).await?;
            if ack[0] != MEM_WR_PER_CHUNK_OK {
                anyhow::bail!(
                    "mem_write: cart NAK 0x{:02X} on chunk @ pi:{:#x}",
                    ack[0],
                    addr.0
                );
            }
        }
        t.write_all(chunk).await?;
    }
    Ok(())
}

/// Send `USB_WR` (0x22) — wraps `payload` with a u16 BE length and pushes
/// to the host's CDC stream. M5.3 will route RSP packets through here.
pub async fn usb_write<T: UsbTransport>(t: &mut T, payload: &[u8]) -> anyhow::Result<()> {
    if payload.len() > u16::MAX as usize {
        anyhow::bail!("usb_write: payload too large ({} bytes)", payload.len());
    }
    let mut hdr = [0u8; 4 + 2];
    hdr[..4].copy_from_slice(&encode_cmd(Opcode::UsbWr as u8));
    hdr[4..6].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    t.write_all(&hdr).await?;
    if !payload.is_empty() {
        t.write_all(payload).await?;
    }
    Ok(())
}

/// Send `HOST_RST` (0x29) with the given reset mode. Per fact-check C18/C19:
/// `Soft` halts the CPU at the reset vector; `Off` releases it.
pub async fn host_reset<T: UsbTransport>(t: &mut T, mode: ResetMode) -> anyhow::Result<()> {
    let mut frame = [0u8; 5];
    frame[..4].copy_from_slice(&encode_cmd(Opcode::HostRst as u8));
    frame[4] = mode as u8;
    t.write_all(&frame).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;

    /// Pre-load `n` per-chunk OK ack bytes into MockUsb's rx queue. The
    /// new `mem_write` reads one ack BEFORE each chunk under ack-mode.
    fn ack_bytes(n: usize) -> Vec<u8> {
        vec![MEM_WR_PER_CHUNK_OK; n]
    }

    #[test]
    fn opcode_tryfrom_roundtrip() {
        for op in [
            Opcode::Status,
            Opcode::MemRd,
            Opcode::MemWr,
            Opcode::UsbWr,
            Opcode::HostRst,
        ] {
            assert_eq!(Opcode::try_from(op as u8).unwrap(), op);
        }
        assert_eq!(Opcode::try_from(0xFF), Err(0xFF));
    }

    #[tokio::test]
    async fn status_roundtrip() {
        let mut m = MockUsb::with_replies(vec![vec![0xA5, 0x00]]);
        let r = send_status(&mut m).await.unwrap();
        assert_eq!(r, StatusReply { key: 0xA5, code: 0 });
        assert_eq!(m.tx_log(), &[0x2B, 0xD4, 0x10, 0xEF]);
    }

    #[tokio::test]
    async fn status_rejects_bad_key() {
        let mut m = MockUsb::with_replies(vec![vec![0x00, 0x00]]);
        assert!(send_status(&mut m).await.is_err());
    }

    #[tokio::test]
    async fn mem_read_short() {
        let mut m = MockUsb::with_replies(vec![vec![0xDE, 0xAD, 0xBE, 0xEF]]);
        let out = mem_read(&mut m, PiBusAddr(0x0010_0000), 4).await.unwrap();
        assert_eq!(out, vec![0xDE, 0xAD, 0xBE, 0xEF]);

        let expected = {
            let mut v = Vec::new();
            v.extend_from_slice(&[0x2B, 0xD4, 0x19, 0xE6]);
            v.extend_from_slice(&0x0010_0000u32.to_be_bytes());
            v.extend_from_slice(&4u32.to_be_bytes());
            v
        };
        assert_eq!(m.tx_log(), expected.as_slice());
    }

    #[tokio::test]
    async fn mem_read_zero_len_does_not_read() {
        let mut m = MockUsb::new();
        let out = mem_read(&mut m, PiBusAddr(0), 0).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(m.rx_remaining(), 0);
    }

    #[tokio::test]
    async fn mem_write_single_chunk_reads_one_ack_before_chunk() {
        // Ack-mode (PSRAM target). Cart sends 0x00 OK byte before the
        // single chunk transmission.
        let mut m = MockUsb::with_replies(vec![ack_bytes(1)]);
        let payload = [0x11u8, 0x22, 0x33, 0x44];
        mem_write(&mut m, PiBusAddr(0x0000_1000), &payload).await.unwrap();

        // Frame 0 = header; frame 1 = chunk.
        assert_eq!(m.tx_frames().len(), 2);
        let h = &m.tx_frames()[0];
        assert_eq!(&h[..4], &[0x2B, 0xD4, 0x1A, 0xE5]);
        assert_eq!(&h[4..8], &0x0000_1000u32.to_be_bytes());
        assert_eq!(&h[8..12], &4u32.to_be_bytes());
        assert_eq!(h[12], MEM_WR_ACK_ON, "ROM-area write must request ACK");
        assert_eq!(&m.tx_frames()[1][..], &payload);
        // Cart side ack got consumed.
        assert_eq!(m.rx_remaining(), 0);
    }

    #[tokio::test]
    async fn mem_write_mode_byte_sent_once_in_header_only() {
        // Verify the 0xAA mode byte appears EXACTLY ONCE — in the header,
        // not interleaved between chunks.
        let mut m = MockUsb::with_replies(vec![ack_bytes(3)]);
        let payload = vec![0xCDu8; 3 * MEM_WR_BLOCK];
        mem_write(&mut m, PiBusAddr(0), &payload).await.unwrap();

        let occurrences =
            m.tx_log().iter().filter(|&&b| b == MEM_WR_ACK_ON).count();
        // 0xAA might appear inside chunk data too (we use 0xCD here so it
        // doesn't), so this guarantees it's only the header byte.
        assert_eq!(
            occurrences, 1,
            "0xAA mode byte must appear only once (in the MEM_WR header)"
        );
        // And it's at offset 12 (right after the 12-byte header prefix).
        assert_eq!(m.tx_frames()[0][12], MEM_WR_ACK_ON);
    }

    #[tokio::test]
    async fn mem_write_multi_chunk_reads_one_ack_per_chunk() {
        // 2.5 KiB -> three chunks of 1024/1024/512. The cart sends one
        // OK byte BEFORE each chunk.
        let mut m = MockUsb::with_replies(vec![ack_bytes(3)]);
        let payload = vec![0xABu8; 2560];
        mem_write(&mut m, PiBusAddr(0x0000_0000), &payload).await.unwrap();

        assert_eq!(m.tx_frames().len(), 1 + 3);
        assert_eq!(m.tx_frames()[1].len(), 1024);
        assert_eq!(m.tx_frames()[2].len(), 1024);
        assert_eq!(m.tx_frames()[3].len(), 512);
        assert_eq!(m.rx_remaining(), 0, "all 3 acks consumed");
    }

    #[tokio::test]
    async fn mem_write_cfg_area_no_ack_no_read() {
        // CFG-region writes are unthrottled: no per-chunk ack reads.
        // MockUsb has zero replies — if mem_write tried to read, it
        // would underflow.
        let mut m = MockUsb::new();
        mem_write(&mut m, PI_CFG_BASE, &[0u8; 8]).await.unwrap();
        assert_eq!(m.tx_frames()[0][12], MEM_WR_ACK_OFF);
    }

    #[tokio::test]
    async fn mem_write_errors_on_nonzero_ack() {
        // Cart sends 0xEE (some error code) before the chunk → bail.
        let mut m = MockUsb::with_replies(vec![vec![0xEE]]);
        let err = mem_write(&mut m, PiBusAddr(0), &[0u8; 4]).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("NAK"), "msg: {msg}");
        assert!(msg.contains("0xEE"), "msg: {msg}");
    }

    #[tokio::test]
    async fn usb_write_lengths_payload() {
        let mut m = MockUsb::new();
        usb_write(&mut m, b"hi").await.unwrap();
        assert_eq!(m.tx_frames().len(), 2);
        let h = &m.tx_frames()[0];
        assert_eq!(&h[..4], &[0x2B, 0xD4, 0x22, 0xDD]);
        assert_eq!(&h[4..6], &2u16.to_be_bytes());
        assert_eq!(&m.tx_frames()[1][..], b"hi");
    }

    #[tokio::test]
    async fn usb_write_empty_payload_skips_second_frame() {
        let mut m = MockUsb::new();
        usb_write(&mut m, &[]).await.unwrap();
        assert_eq!(m.tx_frames().len(), 1);
    }

    #[tokio::test]
    async fn host_reset_emits_soft() {
        let mut m = MockUsb::new();
        host_reset(&mut m, ResetMode::Soft).await.unwrap();
        assert_eq!(
            m.tx_log(),
            &[0x2B, 0xD4, 0x29, 0xD6, ResetMode::Soft as u8]
        );
    }

    #[tokio::test]
    async fn host_reset_off_releases_cpu() {
        let mut m = MockUsb::new();
        host_reset(&mut m, ResetMode::Off).await.unwrap();
        assert_eq!(
            m.tx_log(),
            &[0x2B, 0xD4, 0x29, 0xD6, ResetMode::Off as u8]
        );
    }

    #[test]
    fn pi_bus_constants_match_factcheck() {
        // Per fact-check §1 C13/C14: PI-bus map.
        assert_eq!(PI_ROM_BASE.0, 0x000_0000);
        assert_eq!(PI_SRAM_BASE.0, 0x100_0000);
        assert_eq!(PI_BRAM_BASE.0, 0x108_0000);
        assert_eq!(PI_CFG_BASE.0, 0x180_0000);
        assert_eq!(PI_FIFO_BASE.0, 0x181_0000);
        assert_eq!(PI_MAP_BASE.0, 0x183_0000);
    }
}
