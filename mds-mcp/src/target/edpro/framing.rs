// SPDX-License-Identifier: MIT
//! 4-byte EdPro command framing.
//!
//! Every host->cart command starts with `+ ~+ CMD ~CMD`:
//!
//! ```text
//!     0x2B 0xD4 OP   ~OP
//! ```
//!
//! `0xD4 = !0x2B`. The MCU drops corrupted frames silently. Source:
//! krikzz/mega-ed-pub `edio-mega/everdrive.c` (`ed_cmd_tx`).

// `decode_cmd` + `FramingError` are exercised only by tests until M5.x
// adds reverse-direction frame validation (cart-side stub echoes).
#![allow(dead_code)]

/// Encode the 4-byte preamble + opcode + inverted opcode.
pub fn encode_cmd(opcode: u8) -> [u8; 4] {
    [0x2B, 0xD4, opcode, !opcode]
}

/// Errors returned when validating a 4-byte command frame on the wire.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum FramingError {
    #[error("bad sync bytes (expected 0x2B 0xD4)")]
    BadSync,
    #[error("opcode checksum mismatch")]
    BadChecksum,
}

/// Decode + validate the 4-byte preamble. Returns the opcode on success.
pub fn decode_cmd(buf: &[u8; 4]) -> Result<u8, FramingError> {
    if buf[0] != 0x2B || buf[1] != 0xD4 {
        return Err(FramingError::BadSync);
    }
    if buf[3] != !buf[2] {
        return Err(FramingError::BadChecksum);
    }
    Ok(buf[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_known_opcodes() {
        assert_eq!(encode_cmd(0x10), [0x2B, 0xD4, 0x10, 0xEF]);
        assert_eq!(encode_cmd(0x19), [0x2B, 0xD4, 0x19, 0xE6]);
        assert_eq!(encode_cmd(0x1A), [0x2B, 0xD4, 0x1A, 0xE5]);
        assert_eq!(encode_cmd(0x22), [0x2B, 0xD4, 0x22, 0xDD]);
        assert_eq!(encode_cmd(0x29), [0x2B, 0xD4, 0x29, 0xD6]);
    }

    #[test]
    fn roundtrip_all_opcodes() {
        for op in 0u8..=255 {
            let f = encode_cmd(op);
            assert_eq!(decode_cmd(&f).unwrap(), op);
        }
    }

    #[test]
    fn rejects_bad_sync() {
        assert_eq!(decode_cmd(&[0x00, 0xD4, 0x10, 0xEF]), Err(FramingError::BadSync));
        assert_eq!(decode_cmd(&[0x2B, 0x00, 0x10, 0xEF]), Err(FramingError::BadSync));
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(
            decode_cmd(&[0x2B, 0xD4, 0x10, 0x00]),
            Err(FramingError::BadChecksum)
        );
    }
}
