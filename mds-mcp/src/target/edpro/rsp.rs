// SPDX-License-Identifier: MIT
//! GDB Remote Serial Protocol (RSP) codec — pure functions, no transport.
//!
//! Wire format reference:
//! [GDB Remote Protocol](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Remote-Protocol.html).
//!
//! Frame: `$<payload>#<csum>` where csum = (XOR of payload bytes) mod 256,
//! emitted as 2 lowercase hex chars. Payload is escaped via `}` XOR-0x20 for
//! the literal bytes `# $ } *`. RLE: `<byte>*<n>` expands to `<byte>` repeated
//! `(n - 29 + 1)` times — well, gdb's exact rule is "the byte preceding `*` is
//! repeated such that total run length = (count - 29 + 1)" giving runs of 4..
//! up to 126. We expand on decode; we never compress on encode (gdb spec
//! permits this — "encoders may but need not").
//!
//! Acks: `+` = OK, `-` = retransmit. Optional after `qStartNoAckMode`.
//!
//! This module is a pure codec — wiring to the EdPro USB transport happens
//! in M5.3 (`proto::usb_write` will wrap framed RSP packets).
//!
//! Wire-format goal: byte-compatible with the C stub on the cart
//! (`mds-stub-68k`, see `docs/02-m5-architecture.md` §5).

// Pure-function codec — every public item is intentionally unused outside
// tests until M5.3 wires `EdProTarget` through `proto::usb_write`. Without
// this `cargo clippy --all-targets -D warnings` flags the whole module.
#![allow(dead_code)]

use std::fmt;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the RSP codec.
#[derive(Debug, PartialEq, Eq)]
pub enum RspError {
    /// No `$` start byte found anywhere in the input.
    NoPacketStart,
    /// `$` found but no terminating `#xx`.
    UnterminatedPacket,
    /// Checksum byte didn't match the recomputed XOR.
    BadChecksum { got: u8, expected: u8 },
    /// `}` escape was at end of payload or otherwise malformed.
    BadEscape,
    /// Hex decode failure (odd length, non-hex char...).
    BadHex,
    /// Reply payload didn't match the parser's expected shape.
    UnexpectedReply(String),
}

impl fmt::Display for RspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPacketStart => write!(f, "no '$' packet start in input"),
            Self::UnterminatedPacket => write!(f, "packet missing '#xx' terminator"),
            Self::BadChecksum { got, expected } => {
                write!(f, "bad checksum: got 0x{got:02x}, expected 0x{expected:02x}")
            }
            Self::BadEscape => write!(f, "malformed '}}' escape sequence"),
            Self::BadHex => write!(f, "malformed hex string"),
            Self::UnexpectedReply(s) => write!(f, "unexpected reply: {s}"),
        }
    }
}

impl std::error::Error for RspError {}

// ---------------------------------------------------------------------------
// Acks
// ---------------------------------------------------------------------------

/// Single-byte ack/nack. Optional after `qStartNoAckMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckByte {
    Ok,
    Retransmit,
}

/// Encode an ack byte.
pub fn encode_ack(a: AckByte) -> u8 {
    match a {
        AckByte::Ok => b'+',
        AckByte::Retransmit => b'-',
    }
}

/// Try to recognize a single byte as an ack/nack. `None` if it's neither.
pub fn try_parse_ack(b: u8) -> Option<AckByte> {
    match b {
        b'+' => Some(AckByte::Ok),
        b'-' => Some(AckByte::Retransmit),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Lowercase hex digit table (used in golden-string tests too).
const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex_byte(out: &mut Vec<u8>, b: u8) {
    out.push(HEX[(b >> 4) as usize]);
    out.push(HEX[(b & 0x0F) as usize]);
}

fn push_hex_u32(out: &mut Vec<u8>, v: u32) {
    // Lowercase, big-endian, no leading zeros — matches gdb-style addresses.
    if v == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 8];
    let mut i = 0;
    let mut started = false;
    for shift in (0..8).rev() {
        let nib = ((v >> (shift * 4)) & 0xF) as u8;
        if nib != 0 || started {
            buf[i] = HEX[nib as usize];
            i += 1;
            started = true;
        }
    }
    out.extend_from_slice(&buf[..i]);
}

fn hex_nibble(c: u8) -> Result<u8, RspError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(RspError::BadHex),
    }
}

/// Decode a lowercase/uppercase hex byte stream into raw bytes.
pub fn parse_hex_bytes(payload: &[u8]) -> Result<Vec<u8>, RspError> {
    if !payload.len().is_multiple_of(2) {
        return Err(RspError::BadHex);
    }
    let mut out = Vec::with_capacity(payload.len() / 2);
    for pair in payload.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Packet framing
// ---------------------------------------------------------------------------

/// XOR checksum of payload bytes (mod 256). Per gdb spec: applied to the
/// already-escaped payload bytes, *not* the raw user data.
fn xor_checksum(payload: &[u8]) -> u8 {
    let mut c = 0u8;
    for b in payload {
        c ^= *b;
    }
    c
}

/// Apply `}` XOR-0x20 escape to the literal bytes `# $ } *`.
fn escape_payload(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    for &b in payload {
        match b {
            b'#' | b'$' | b'}' | b'*' => {
                out.push(b'}');
                out.push(b ^ 0x20);
            }
            _ => out.push(b),
        }
    }
    out
}

/// Build a framed RSP packet around `payload`. Applies `}` escapes to the
/// payload before computing the checksum (per gdb spec).
pub fn encode_packet(payload: &[u8]) -> Vec<u8> {
    let escaped = escape_payload(payload);
    let csum = xor_checksum(&escaped);
    let mut out = Vec::with_capacity(escaped.len() + 4);
    out.push(b'$');
    out.extend_from_slice(&escaped);
    out.push(b'#');
    push_hex_byte(&mut out, csum);
    out
}

/// Decode a framed RSP packet from `input`. Skips leading non-`$` bytes
/// (acks `+`/`-`, junk). Reverses `}` escapes and expands `*` RLE runs.
///
/// Returns `(decoded_payload, total_bytes_consumed_from_input)`.
pub fn decode_packet(input: &[u8]) -> Result<(Vec<u8>, usize), RspError> {
    // Find '$'.
    let start = input
        .iter()
        .position(|&b| b == b'$')
        .ok_or(RspError::NoPacketStart)?;
    // Find '#' after start.
    let hash_rel = input[start + 1..]
        .iter()
        .position(|&b| b == b'#')
        .ok_or(RspError::UnterminatedPacket)?;
    let hash = start + 1 + hash_rel;
    if hash + 2 >= input.len() {
        return Err(RspError::UnterminatedPacket);
    }
    let raw_payload = &input[start + 1..hash];
    let csum_chars = &input[hash + 1..hash + 3];
    let expected = xor_checksum(raw_payload);
    let got = (hex_nibble(csum_chars[0])? << 4) | hex_nibble(csum_chars[1])?;
    if got != expected {
        return Err(RspError::BadChecksum { got, expected });
    }

    // Reverse escapes + expand RLE.
    let mut out: Vec<u8> = Vec::with_capacity(raw_payload.len());
    let mut i = 0;
    while i < raw_payload.len() {
        let b = raw_payload[i];
        if b == b'}' {
            i += 1;
            if i >= raw_payload.len() {
                return Err(RspError::BadEscape);
            }
            out.push(raw_payload[i] ^ 0x20);
            i += 1;
        } else if b == b'*' {
            // RLE: needs a preceding byte and a count byte.
            if out.is_empty() {
                return Err(RspError::BadEscape);
            }
            i += 1;
            if i >= raw_payload.len() {
                return Err(RspError::BadEscape);
            }
            let count_byte = raw_payload[i];
            // gdb: count = byte - 29; appended to the previous byte that
            // many extra times. So "X*\x20" (0x20 = 32) means 32-29 = 3
            // additional X => total run of 4 X's.
            if count_byte < 29 {
                return Err(RspError::BadEscape);
            }
            let extra = (count_byte - 29) as usize;
            let last = *out.last().unwrap();
            for _ in 0..extra {
                out.push(last);
            }
            i += 1;
        } else {
            out.push(b);
            i += 1;
        }
    }

    Ok((out, hash + 3))
}

// ---------------------------------------------------------------------------
// Command builders (return raw payloads — caller frames via encode_packet)
// ---------------------------------------------------------------------------

/// `qSupported:feat1+;feat2+`
pub fn cmd_query_supported(features: &[&str]) -> Vec<u8> {
    let mut out = Vec::from(b"qSupported".as_slice());
    if !features.is_empty() {
        out.push(b':');
        for (i, f) in features.iter().enumerate() {
            if i > 0 {
                out.push(b';');
            }
            out.extend_from_slice(f.as_bytes());
        }
    }
    out
}

/// `g` — read all general registers.
pub fn cmd_read_registers() -> Vec<u8> {
    b"g".to_vec()
}

/// `G<hex>` — write all general registers.
pub fn cmd_write_registers(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + data.len() * 2);
    out.push(b'G');
    for &b in data {
        push_hex_byte(&mut out, b);
    }
    out
}

/// `p<n>` — read a single register.
pub fn cmd_read_one_register(n: u32) -> Vec<u8> {
    let mut out = Vec::from(b"p".as_slice());
    push_hex_u32(&mut out, n);
    out
}

/// `P<n>=<hex>` — write a single register.
pub fn cmd_write_one_register(n: u32, value: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(b"P".as_slice());
    push_hex_u32(&mut out, n);
    out.push(b'=');
    for &b in value {
        push_hex_byte(&mut out, b);
    }
    out
}

/// `m<addr>,<len>` — read memory.
pub fn cmd_read_memory(addr: u32, len: u32) -> Vec<u8> {
    let mut out = Vec::from(b"m".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, len);
    out
}

/// `M<addr>,<len>:<hex>` — write memory.
pub fn cmd_write_memory(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(b"M".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, data.len() as u32);
    out.push(b':');
    for &b in data {
        push_hex_byte(&mut out, b);
    }
    out
}

/// `c[addr]` — continue, optionally from `addr`.
pub fn cmd_continue(addr: Option<u32>) -> Vec<u8> {
    let mut out = Vec::from(b"c".as_slice());
    if let Some(a) = addr {
        push_hex_u32(&mut out, a);
    }
    out
}

/// `s[addr]` — single-step, optionally from `addr`.
pub fn cmd_step(addr: Option<u32>) -> Vec<u8> {
    let mut out = Vec::from(b"s".as_slice());
    if let Some(a) = addr {
        push_hex_u32(&mut out, a);
    }
    out
}

/// `Z0,<addr>,<kind>` — set software breakpoint.
pub fn cmd_set_sw_breakpoint(addr: u32, kind: u32) -> Vec<u8> {
    let mut out = Vec::from(b"Z0,".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, kind);
    out
}

/// `z0,<addr>,<kind>` — clear software breakpoint.
pub fn cmd_clear_sw_breakpoint(addr: u32, kind: u32) -> Vec<u8> {
    let mut out = Vec::from(b"z0,".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, kind);
    out
}

/// `?` — query halt reason.
pub fn cmd_query_halt_reason() -> Vec<u8> {
    b"?".to_vec()
}

/// `vCont;<actions>` — continue with explicit actions.
pub fn cmd_vcont(actions: &str) -> Vec<u8> {
    let mut out = Vec::from(b"vCont;".as_slice());
    out.extend_from_slice(actions.as_bytes());
    out
}

/// `QStartNoAckMode` — disable acks after handshake.
pub fn cmd_query_start_no_ack_mode() -> Vec<u8> {
    b"QStartNoAckMode".to_vec()
}

// ---------------------------------------------------------------------------
// M5.7: custom VDP queries (handled by mds-stub-68k's `qMds*` dispatcher).
//
// These are *not* part of the GDB RSP standard — they're our private monitor
// commands (the `qMds<Name>` namespace) for reading VDP-indirect memory
// (CRAM/VSRAM/VRAM) and the VDP status word, which are not directly
// memory-mapped on the 68k bus and so can't be served by a plain `m` packet.
//
// Reply format: hex-encoded raw bytes (CRAM/VSRAM/VRAM) or 4 hex digits
// (VDP status). Both decoded by the host via `parse_hex_bytes`.
// ---------------------------------------------------------------------------

/// `qMdsCram` — read all 128 bytes of CRAM (64 9-bit BGR colour entries).
pub fn cmd_qmds_cram() -> Vec<u8> {
    b"qMdsCram".to_vec()
}

/// `qMdsVsram` — read all 80 bytes of VSRAM (40 vertical-scroll entries).
pub fn cmd_qmds_vsram() -> Vec<u8> {
    b"qMdsVsram".to_vec()
}

/// `qMdsVdpStatus` — read the VDP status word (4 hex digits, big-endian).
pub fn cmd_qmds_vdp_status() -> Vec<u8> {
    b"qMdsVdpStatus".to_vec()
}

/// `qMdsVram:<addr>,<len>` — read up to 128 bytes from VRAM at `addr`.
/// Stub silently truncates `len` to 128; host caller chunks larger reads.
pub fn cmd_qmds_vram(addr: u32, len: u32) -> Vec<u8> {
    let mut out = Vec::from(b"qMdsVram:".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, len);
    out
}

// ---------------------------------------------------------------------------
// Reply parsers
// ---------------------------------------------------------------------------

/// Stop reply — `S<sig>` (simple) or `T<sig>k:v;...` (rich).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReply {
    Sig(u8),
    TrapAt {
        signal: u8,
        regs: Vec<(u32, Vec<u8>)>,
        reason: Option<String>,
    },
}

/// Parse a stop reply payload (`S05`, `T05thread:01;`, etc.).
pub fn parse_stop_reply(payload: &[u8]) -> Result<StopReply, RspError> {
    if payload.is_empty() {
        return Err(RspError::UnexpectedReply("empty stop reply".into()));
    }
    match payload[0] {
        b'S' => {
            if payload.len() != 3 {
                return Err(RspError::UnexpectedReply(format!(
                    "S reply length {}",
                    payload.len()
                )));
            }
            let sig = (hex_nibble(payload[1])? << 4) | hex_nibble(payload[2])?;
            Ok(StopReply::Sig(sig))
        }
        b'T' => {
            if payload.len() < 3 {
                return Err(RspError::UnexpectedReply("T reply too short".into()));
            }
            let sig = (hex_nibble(payload[1])? << 4) | hex_nibble(payload[2])?;
            let mut regs: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut reason: Option<String> = None;
            // Remainder is a `;`-separated list of `key:value` pairs. Trailing
            // `;` is allowed (often present in gdb traffic).
            let rest = &payload[3..];
            for part in rest.split(|&b| b == b';') {
                if part.is_empty() {
                    continue;
                }
                let colon = part
                    .iter()
                    .position(|&b| b == b':')
                    .ok_or_else(|| RspError::UnexpectedReply("T reply: missing ':'".into()))?;
                let key = &part[..colon];
                let val = &part[colon + 1..];
                // If the key is all hex, it's a register number. Otherwise
                // (`thread`, `core`, `watch`, `swbreak`...) it's a stop reason.
                if !key.is_empty() && key.iter().all(|&b| b.is_ascii_hexdigit()) {
                    let mut n: u32 = 0;
                    for &c in key {
                        n = (n << 4) | u32::from(hex_nibble(c)?);
                    }
                    regs.push((n, parse_hex_bytes(val)?));
                } else {
                    let kstr = std::str::from_utf8(key)
                        .map_err(|_| RspError::UnexpectedReply("T reply: non-utf8 key".into()))?;
                    let vstr = std::str::from_utf8(val)
                        .map_err(|_| RspError::UnexpectedReply("T reply: non-utf8 val".into()))?;
                    reason = Some(format!("{kstr}:{vstr}"));
                }
            }
            Ok(StopReply::TrapAt {
                signal: sig,
                regs,
                reason,
            })
        }
        _ => Err(RspError::UnexpectedReply(format!(
            "stop reply: unknown leading byte 0x{:02x}",
            payload[0]
        ))),
    }
}

/// Parse `OK`. Anything else is `UnexpectedReply`.
pub fn parse_ok(payload: &[u8]) -> Result<(), RspError> {
    if payload == b"OK" {
        Ok(())
    } else {
        Err(RspError::UnexpectedReply(format!(
            "expected OK, got {:?}",
            String::from_utf8_lossy(payload)
        )))
    }
}

/// Parse `E<xx>` — returns the error code.
pub fn parse_error(payload: &[u8]) -> Result<u8, RspError> {
    if payload.len() != 3 || payload[0] != b'E' {
        return Err(RspError::UnexpectedReply(format!(
            "expected E<xx>, got {:?}",
            String::from_utf8_lossy(payload)
        )));
    }
    Ok((hex_nibble(payload[1])? << 4) | hex_nibble(payload[2])?)
}

/// Parse a `qSupported` reply: `;`-separated list of `feat=val` or `feat[+/-/?]`.
pub fn parse_qsupported_reply(payload: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in payload.split(|&b| b == b';') {
        if part.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(part);
        if let Some(eq) = s.find('=') {
            out.push((s[..eq].to_string(), s[eq + 1..].to_string()));
        } else if let Some(last) = s.chars().last() {
            if matches!(last, '+' | '-' | '?') {
                let n = s.len() - last.len_utf8();
                out.push((s[..n].to_string(), last.to_string()));
            } else {
                out.push((s.to_string(), String::new()));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- encode_packet -----------------------------------------------------

    #[test]
    fn encode_empty_payload() {
        // XOR of zero bytes = 0 -> "$#00"
        assert_eq!(encode_packet(b""), b"$#00".to_vec());
    }

    #[test]
    fn encode_ascii_payload_ok() {
        // payload "OK" -> 'O'^'K' = 0x4F ^ 0x4B = 0x04
        assert_eq!(encode_packet(b"OK"), b"$OK#04".to_vec());
    }

    #[test]
    fn encode_g_command() {
        // 'g' = 0x67
        assert_eq!(encode_packet(b"g"), b"$g#67".to_vec());
    }

    #[test]
    fn encode_escapes_hash() {
        // payload single '#' (0x23): escape -> '}' (0x7D), 0x23^0x20=0x03
        // checksum = 0x7D ^ 0x03 = 0x7E
        assert_eq!(encode_packet(b"#"), b"$}\x03#7e".to_vec());
    }

    #[test]
    fn encode_escapes_dollar() {
        // '$' = 0x24, escaped to '}', 0x04 -> csum 0x7D^0x04 = 0x79
        assert_eq!(encode_packet(b"$"), b"$}\x04#79".to_vec());
    }

    #[test]
    fn encode_escapes_brace() {
        // '}' = 0x7D, escaped to '}', 0x5D -> csum 0x7D^0x5D = 0x20
        assert_eq!(encode_packet(b"}"), b"$}\x5d#20".to_vec());
    }

    #[test]
    fn encode_escapes_star() {
        // '*' = 0x2A, escaped to '}', 0x0A -> csum 0x7D^0x0A = 0x77
        assert_eq!(encode_packet(b"*"), b"$}\x0a#77".to_vec());
    }

    #[test]
    fn encode_checksum_handcomputed() {
        // payload "abc" -> 'a'^'b'^'c' = 0x60
        assert_eq!(encode_packet(b"abc"), b"$abc#60".to_vec());
    }

    // --- decode_packet -----------------------------------------------------

    #[test]
    fn decode_well_formed() {
        let (p, n) = decode_packet(b"$OK#04").unwrap();
        assert_eq!(p, b"OK");
        assert_eq!(n, 6);
    }

    #[test]
    fn decode_skips_leading_garbage() {
        let (p, n) = decode_packet(b"junk$OK#04tail").unwrap();
        assert_eq!(p, b"OK");
        assert_eq!(n, 10); // 4 garbage + 6 packet
    }

    #[test]
    fn decode_skips_leading_ack() {
        let (p, n) = decode_packet(b"+$OK#04").unwrap();
        assert_eq!(p, b"OK");
        assert_eq!(n, 7);
    }

    #[test]
    fn decode_no_dollar() {
        assert_eq!(decode_packet(b"junk"), Err(RspError::NoPacketStart));
    }

    #[test]
    fn decode_no_hash() {
        assert_eq!(decode_packet(b"$abcdef"), Err(RspError::UnterminatedPacket));
    }

    #[test]
    fn decode_truncated_csum() {
        assert_eq!(decode_packet(b"$abc#6"), Err(RspError::UnterminatedPacket));
    }

    #[test]
    fn decode_bad_csum() {
        let r = decode_packet(b"$OK#00");
        match r {
            Err(RspError::BadChecksum { got, expected }) => {
                assert_eq!(got, 0x00);
                assert_eq!(expected, 0x04);
            }
            other => panic!("expected BadChecksum, got {other:?}"),
        }
    }

    #[test]
    fn decode_escape_hash() {
        // Encoded: "}\x03" => decodes to "#"
        let frame = encode_packet(b"#");
        let (p, _) = decode_packet(&frame).unwrap();
        assert_eq!(p, b"#");
    }

    #[test]
    fn decode_dangling_escape() {
        // Frame "$}#5d" — '}' at end of payload (csum of just '}' = 0x7D = "5d" lowercase? let's check: 0x7D = "7d")
        // Actually we want a valid checksum frame whose payload is just "}".
        // payload = b"}" -> csum = 0x7D -> "$}#7d"
        // decode should fail with BadEscape (escape byte missing).
        assert_eq!(decode_packet(b"$}#7d"), Err(RspError::BadEscape));
    }

    #[test]
    fn decode_rle_expansion() {
        // payload "X*\x20" -> count = 0x20 - 29 = 3 extra X's -> "XXXX"
        // csum: 'X' ^ '*' ^ 0x20 = 0x58 ^ 0x2A ^ 0x20 = 0x52 -> "52"
        let (p, _) = decode_packet(b"$X*\x20#52").unwrap();
        assert_eq!(p, b"XXXX");
    }

    #[test]
    fn decode_rle_bad_count() {
        // count byte = 28 < 29 -> BadEscape
        // payload "X*\x1c" csum 0x58 ^ 0x2A ^ 0x1C = 0x6E -> "6e"
        assert_eq!(decode_packet(b"$X*\x1c#6e"), Err(RspError::BadEscape));
    }

    // --- Acks --------------------------------------------------------------

    #[test]
    fn ack_encode() {
        assert_eq!(encode_ack(AckByte::Ok), b'+');
        assert_eq!(encode_ack(AckByte::Retransmit), b'-');
    }

    #[test]
    fn ack_parse() {
        assert_eq!(try_parse_ack(b'+'), Some(AckByte::Ok));
        assert_eq!(try_parse_ack(b'-'), Some(AckByte::Retransmit));
        assert_eq!(try_parse_ack(b'X'), None);
        assert_eq!(try_parse_ack(b'$'), None);
    }

    // --- Command builders --------------------------------------------------

    #[test]
    fn cmd_qsupported_no_features() {
        assert_eq!(cmd_query_supported(&[]), b"qSupported".to_vec());
    }

    #[test]
    fn cmd_qsupported_with_features() {
        assert_eq!(
            cmd_query_supported(&["swbreak+", "hwbreak+"]),
            b"qSupported:swbreak+;hwbreak+".to_vec()
        );
    }

    #[test]
    fn cmd_read_registers_payload() {
        assert_eq!(cmd_read_registers(), b"g".to_vec());
    }

    #[test]
    fn cmd_write_registers_payload() {
        assert_eq!(
            cmd_write_registers(&[0xDE, 0xAD, 0xBE, 0xEF]),
            b"Gdeadbeef".to_vec()
        );
    }

    #[test]
    fn cmd_read_one_register_payload() {
        assert_eq!(cmd_read_one_register(0), b"p0".to_vec());
        assert_eq!(cmd_read_one_register(0x11), b"p11".to_vec());
        assert_eq!(cmd_read_one_register(0xABCD), b"pabcd".to_vec());
    }

    #[test]
    fn cmd_write_one_register_payload() {
        assert_eq!(
            cmd_write_one_register(7, &[0x12, 0x34]),
            b"P7=1234".to_vec()
        );
    }

    #[test]
    fn cmd_read_memory_payload() {
        assert_eq!(cmd_read_memory(0xFF0000, 4), b"mff0000,4".to_vec());
    }

    #[test]
    fn cmd_write_memory_payload() {
        assert_eq!(
            cmd_write_memory(0xFF8000, &[0xAA, 0xBB]),
            b"Mff8000,2:aabb".to_vec()
        );
    }

    #[test]
    fn cmd_continue_payload() {
        assert_eq!(cmd_continue(None), b"c".to_vec());
        assert_eq!(cmd_continue(Some(0x200)), b"c200".to_vec());
    }

    #[test]
    fn cmd_step_payload() {
        assert_eq!(cmd_step(None), b"s".to_vec());
        assert_eq!(cmd_step(Some(0x1000)), b"s1000".to_vec());
    }

    #[test]
    fn cmd_breakpoint_payloads() {
        assert_eq!(cmd_set_sw_breakpoint(0x200, 2), b"Z0,200,2".to_vec());
        assert_eq!(cmd_clear_sw_breakpoint(0x200, 2), b"z0,200,2".to_vec());
    }

    #[test]
    fn cmd_misc_payloads() {
        assert_eq!(cmd_query_halt_reason(), b"?".to_vec());
        assert_eq!(cmd_vcont("c:1"), b"vCont;c:1".to_vec());
        assert_eq!(cmd_query_start_no_ack_mode(), b"QStartNoAckMode".to_vec());
    }

    // --- M5.7 qMds* VDP queries -------------------------------------------

    #[test]
    fn cmd_qmds_cram_payload() {
        assert_eq!(cmd_qmds_cram(), b"qMdsCram".to_vec());
    }

    #[test]
    fn cmd_qmds_vsram_payload() {
        assert_eq!(cmd_qmds_vsram(), b"qMdsVsram".to_vec());
    }

    #[test]
    fn cmd_qmds_vdp_status_payload() {
        assert_eq!(cmd_qmds_vdp_status(), b"qMdsVdpStatus".to_vec());
    }

    #[test]
    fn cmd_qmds_vram_payload() {
        // Lowercase, no-leading-zeros hex (matches existing builders).
        assert_eq!(cmd_qmds_vram(0, 32), b"qMdsVram:0,20".to_vec());
        assert_eq!(cmd_qmds_vram(0xC000, 64), b"qMdsVram:c000,40".to_vec());
        assert_eq!(cmd_qmds_vram(0x1234, 0x80), b"qMdsVram:1234,80".to_vec());
    }

    #[test]
    fn cmd_qmds_vram_roundtrips_through_packet() {
        let frame = encode_packet(&cmd_qmds_vram(0xC000, 0x40));
        let (decoded, _) = decode_packet(&frame).unwrap();
        assert_eq!(decoded, b"qMdsVram:c000,40");
    }

    // --- Reply parsers -----------------------------------------------------

    #[test]
    fn parse_ok_ok() {
        assert!(parse_ok(b"OK").is_ok());
    }

    #[test]
    fn parse_ok_rejects_else() {
        assert!(parse_ok(b"oK").is_err());
        assert!(parse_ok(b"").is_err());
    }

    #[test]
    fn parse_error_e01() {
        assert_eq!(parse_error(b"E01").unwrap(), 1);
        assert_eq!(parse_error(b"Eff").unwrap(), 0xFF);
    }

    #[test]
    fn parse_error_rejects_else() {
        assert!(parse_error(b"OK").is_err());
        assert!(parse_error(b"E0").is_err());
    }

    #[test]
    fn parse_stop_simple() {
        assert_eq!(parse_stop_reply(b"S05").unwrap(), StopReply::Sig(0x05));
    }

    #[test]
    fn parse_stop_rich() {
        let r = parse_stop_reply(b"T05thread:01;").unwrap();
        match r {
            StopReply::TrapAt {
                signal,
                regs,
                reason,
            } => {
                assert_eq!(signal, 0x05);
                assert!(regs.is_empty());
                assert_eq!(reason.as_deref(), Some("thread:01"));
            }
            _ => panic!("expected TrapAt"),
        }
    }

    #[test]
    fn parse_stop_rich_with_reg() {
        // T05 0a:deadbeef;thread:01;
        let r = parse_stop_reply(b"T050a:deadbeef;thread:01;").unwrap();
        match r {
            StopReply::TrapAt {
                signal,
                regs,
                reason,
            } => {
                assert_eq!(signal, 0x05);
                assert_eq!(regs, vec![(0x0a, vec![0xDE, 0xAD, 0xBE, 0xEF])]);
                assert_eq!(reason.as_deref(), Some("thread:01"));
            }
            _ => panic!("expected TrapAt"),
        }
    }

    #[test]
    fn parse_stop_malformed() {
        assert!(parse_stop_reply(b"").is_err());
        assert!(parse_stop_reply(b"X05").is_err());
        assert!(parse_stop_reply(b"S0").is_err());
    }

    #[test]
    fn parse_qsupported_mixed() {
        let v = parse_qsupported_reply(b"PacketSize=400;swbreak+;hwbreak-;qXfer:features:read-");
        // The last entry contains '=' inside the value — `find('=')` stops
        // at the first one, so we get key="qXfer:features:read-" only when
        // there's no '='. Here "qXfer:features:read-" has no '=' so it
        // becomes (qXfer:features:read, "-").
        assert!(v.contains(&("PacketSize".into(), "400".into())));
        assert!(v.contains(&("swbreak".into(), "+".into())));
        assert!(v.contains(&("hwbreak".into(), "-".into())));
        assert!(v.contains(&("qXfer:features:read".into(), "-".into())));
    }

    // --- Hex helpers -------------------------------------------------------

    #[test]
    fn parse_hex_bytes_basic() {
        assert_eq!(parse_hex_bytes(b"deadbeef").unwrap(), vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(parse_hex_bytes(b"").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn parse_hex_bytes_rejects_odd() {
        assert!(parse_hex_bytes(b"abc").is_err());
    }

    #[test]
    fn parse_hex_bytes_rejects_nonhex() {
        assert!(parse_hex_bytes(b"zz").is_err());
    }

    // --- Roundtrip ---------------------------------------------------------

    #[test]
    fn roundtrip_plain() {
        for p in [
            &b""[..],
            &b"OK"[..],
            &b"hello world"[..],
            &b"qSupported:swbreak+;hwbreak+"[..],
        ] {
            let frame = encode_packet(p);
            let (decoded, n) = decode_packet(&frame).unwrap();
            assert_eq!(decoded, p);
            assert_eq!(n, frame.len());
        }
    }

    #[test]
    fn roundtrip_with_escapes() {
        for p in [
            &b"#"[..],
            &b"$"[..],
            &b"}"[..],
            &b"*"[..],
            &b"#$}*"[..],
            &b"a#b$c}d*e"[..],
        ] {
            let frame = encode_packet(p);
            let (decoded, _n) = decode_packet(&frame).unwrap();
            assert_eq!(decoded, p, "roundtrip failed for {:?}", p);
        }
    }

    #[test]
    fn roundtrip_command_through_packet() {
        // Build a typical mem-read command and round-trip it.
        let payload = cmd_read_memory(0xFF8000, 16);
        let frame = encode_packet(&payload);
        let (decoded, _) = decode_packet(&frame).unwrap();
        assert_eq!(decoded, payload);
        assert_eq!(decoded, b"mff8000,10");
    }
}
