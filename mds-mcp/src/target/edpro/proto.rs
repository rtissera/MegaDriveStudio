// SPDX-License-Identifier: MIT
//! Typed EdPro USB opcodes + thin wrappers.
//!
//! Layered on top of [`super::usb::UsbTransport`] + [`super::framing`]. Every
//! function here is hardware-free — drive them with `MockUsb` in tests.
//!
//! Wire format reminder (host -> cart):
//! ```text
//!     '+' ~'+' OP ~OP <op-specific payload, big-endian for u16/u32>
//! ```

// Every wrapper here is reachable only from tests until M5.x routes
// `EdProTarget` tool methods through them.
#![allow(dead_code)]

use super::framing::encode_cmd;
use super::usb::UsbTransport;

/// EdPro command opcodes used by M5. See `/tmp/mds-m5-research.md`.
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

/// `0xAA` enables ACK-throttling for ROM-area writes (`addr < 0x180_0000`).
const MEM_WR_ACK_ON: u8 = 0xAA;
const MEM_WR_ACK_OFF: u8 = 0x00;
/// Per krikzz convention `ed_cmd_mem_wr` reads back one byte per 1 KiB block
/// once ACK is on. The exact value isn't documented in the public sources —
/// we verify a non-zero ACK and bail on 0x00. Confirm against real hardware
/// in M5.4.
const MEM_WR_BLOCK: usize = 1024;

/// Reset modes for `HOST_RST` (0x29). Values mirror krikzz everdrive.c.
#[allow(dead_code)] // M5.x will route real reset flows
#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum ResetMode {
    Off = 0,
    Soft = 1,
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

/// Send `MEM_RD` (0x19) for `[addr, addr+len)` and read back `len` bytes.
/// One-shot — no chunking yet (M5.2 will add streaming for large reads).
pub async fn mem_read<T: UsbTransport>(
    t: &mut T,
    addr: u32,
    len: u32,
) -> anyhow::Result<Vec<u8>> {
    let mut hdr = [0u8; 4 + 4 + 4];
    hdr[..4].copy_from_slice(&encode_cmd(Opcode::MemRd as u8));
    hdr[4..8].copy_from_slice(&addr.to_be_bytes());
    hdr[8..12].copy_from_slice(&len.to_be_bytes());
    t.write_all(&hdr).await?;

    let mut out = vec![0u8; len as usize];
    if !out.is_empty() {
        t.read_exact(&mut out).await?;
    }
    Ok(out)
}

/// Send `MEM_WR` (0x1A) — chunks payload into 1 KiB slabs. ROM-area writes
/// (`addr < 0x180_0000`) ask the cart to ACK each slab so we don't overflow
/// the FIFO. Under `cfg(test)` we skip the ACK read so MockUsb tests can
/// stay terse — flip via the `skip_ack` parameter when wired into real fw.
pub async fn mem_write<T: UsbTransport>(
    t: &mut T,
    addr: u32,
    data: &[u8],
) -> anyhow::Result<()> {
    let len = data.len() as u32;
    let want_ack = addr < 0x0180_0000;
    let ack_byte = if want_ack { MEM_WR_ACK_ON } else { MEM_WR_ACK_OFF };

    let mut hdr = [0u8; 4 + 4 + 4 + 1];
    hdr[..4].copy_from_slice(&encode_cmd(Opcode::MemWr as u8));
    hdr[4..8].copy_from_slice(&addr.to_be_bytes());
    hdr[8..12].copy_from_slice(&len.to_be_bytes());
    hdr[12] = ack_byte;
    t.write_all(&hdr).await?;

    for chunk in data.chunks(MEM_WR_BLOCK) {
        t.write_all(chunk).await?;
        if want_ack && !cfg!(test) {
            let mut ack = [0u8; 1];
            t.read_exact(&mut ack).await?;
            if ack[0] == 0 {
                anyhow::bail!("mem_write: cart NAK (0x00) on chunk @ {addr:#x}");
            }
        }
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

/// Send `HOST_RST` (0x29) with the given reset mode. Soft-reset by default.
pub async fn host_reset<T: UsbTransport>(t: &mut T) -> anyhow::Result<()> {
    let mut frame = [0u8; 5];
    frame[..4].copy_from_slice(&encode_cmd(Opcode::HostRst as u8));
    frame[4] = ResetMode::Soft as u8;
    t.write_all(&frame).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;

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
        let out = mem_read(&mut m, 0x0010_0000, 4).await.unwrap();
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
        let out = mem_read(&mut m, 0, 0).await.unwrap();
        assert!(out.is_empty());
        assert_eq!(m.rx_remaining(), 0);
    }

    #[tokio::test]
    async fn mem_write_single_chunk() {
        let mut m = MockUsb::new();
        let payload = [0x11u8, 0x22, 0x33, 0x44];
        mem_write(&mut m, 0x0000_1000, &payload).await.unwrap();

        // Frame 0 = header; frame 1 = chunk.
        assert_eq!(m.tx_frames().len(), 2);
        let h = &m.tx_frames()[0];
        assert_eq!(&h[..4], &[0x2B, 0xD4, 0x1A, 0xE5]);
        assert_eq!(&h[4..8], &0x0000_1000u32.to_be_bytes());
        assert_eq!(&h[8..12], &4u32.to_be_bytes());
        assert_eq!(h[12], MEM_WR_ACK_ON, "ROM-area write must request ACK");
        assert_eq!(&m.tx_frames()[1][..], &payload);
    }

    #[tokio::test]
    async fn mem_write_multi_chunk() {
        let mut m = MockUsb::new();
        // 2.5 KiB -> three chunks of 1024/1024/512.
        let payload = vec![0xAB; 2560];
        mem_write(&mut m, 0x0000_0000, &payload).await.unwrap();

        assert_eq!(m.tx_frames().len(), 1 + 3);
        assert_eq!(m.tx_frames()[1].len(), 1024);
        assert_eq!(m.tx_frames()[2].len(), 1024);
        assert_eq!(m.tx_frames()[3].len(), 512);
    }

    #[tokio::test]
    async fn mem_write_cfg_area_no_ack() {
        let mut m = MockUsb::new();
        mem_write(&mut m, 0x0180_0000, &[0u8; 8]).await.unwrap();
        assert_eq!(m.tx_frames()[0][12], MEM_WR_ACK_OFF);
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
        host_reset(&mut m).await.unwrap();
        assert_eq!(
            m.tx_log(),
            &[0x2B, 0xD4, 0x29, 0xD6, ResetMode::Soft as u8]
        );
    }
}
