// SPDX-License-Identifier: MIT
//! Host-side breakpoint table + RSP step state machine.
//!
//! `StubSync` arbitrates the tool surface (`set_breakpoint`,
//! `step_instruction`, ...) onto raw RSP packets sent through the EdPro
//! USB transport. Owns the BP table mirror, the `no_ack_mode` flag, and a
//! sequence counter for tracing.
//!
//! Wire format (per `docs/02-m5-architecture.md` §4.4 / §5.4):
//! - **TX:** every framed RSP packet (and ack) is wrapped in a `USB_WR`
//!   envelope via [`proto::usb_write`]. The MCU forwards `USB_WR` payloads
//!   verbatim onto the 68k FIFO.
//! - **RX:** the cart stub writes raw RSP bytes into the FIFO; MCU mirrors
//!   them onto the host CDC stream. We read byte-by-byte.

#![allow(dead_code)] // EdProTarget will wire these methods up in M5.4+

use std::collections::HashMap;
use std::fmt;
use std::io;

use super::proto;
use super::rsp::{
    self, cmd_continue, cmd_query_halt_reason, cmd_query_start_no_ack_mode, cmd_query_supported,
    cmd_read_memory, cmd_read_registers, cmd_step, cmd_write_memory, cmd_write_registers,
    decode_packet, encode_packet, parse_hex_bytes, parse_ok, parse_qsupported_reply,
    parse_stop_reply, RspError, StopReply,
};
use super::usb::UsbTransport;

/// TRAP #1 opcode (big-endian) — patched into PSRAM at active breakpoints.
const TRAP1_OPCODE: [u8; 2] = [0x4E, 0x41];

/// Max `-` (retransmit) replies before we give up on a packet.
const MAX_RETRANSMITS: u32 = 3;

// ---- Errors ----------------------------------------------------------------

/// Errors surfaced by the stub-sync state machine.
#[derive(Debug)]
pub enum StubSyncError {
    Rsp(RspError),
    Io(io::Error),
    Transport(String),
    MaxRetransmits,
    UnexpectedReply(Vec<u8>),
}

impl fmt::Display for StubSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rsp(e) => write!(f, "rsp codec: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Transport(s) => write!(f, "transport: {s}"),
            Self::MaxRetransmits => write!(f, "stub NAKed > {MAX_RETRANSMITS} times"),
            Self::UnexpectedReply(b) => {
                write!(f, "unexpected reply: {:?}", String::from_utf8_lossy(b))
            }
        }
    }
}

impl std::error::Error for StubSyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rsp(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<RspError> for StubSyncError {
    fn from(e: RspError) -> Self {
        Self::Rsp(e)
    }
}

impl From<io::Error> for StubSyncError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<anyhow::Error> for StubSyncError {
    fn from(e: anyhow::Error) -> Self {
        Self::Transport(e.to_string())
    }
}

/// Result alias used throughout the module.
pub type Result<T> = std::result::Result<T, StubSyncError>;

// ---- Breakpoint table ------------------------------------------------------

/// Host-side mirror of the cart's BP table. Maps a 24-bit 68k address to the
/// 2-byte word that lived there before we patched it with TRAP #1.
#[derive(Debug, Default)]
pub struct BreakpointTable {
    bps: HashMap<u32, [u8; 2]>,
}

impl BreakpointTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert / overwrite the saved opcode for `addr`.
    pub fn insert(&mut self, addr: u32, original: [u8; 2]) {
        self.bps.insert(addr, original);
    }

    /// Return `Some(original)` if `addr` is currently set as a breakpoint.
    pub fn get(&self, addr: u32) -> Option<[u8; 2]> {
        self.bps.get(&addr).copied()
    }

    /// Forget about `addr`. Returns the saved opcode if it was present.
    pub fn remove(&mut self, addr: u32) -> Option<[u8; 2]> {
        self.bps.remove(&addr)
    }

    /// Number of active breakpoints.
    pub fn len(&self) -> usize {
        self.bps.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.bps.is_empty()
    }

    /// Sorted address list. Useful for `mega_list_breakpoints`.
    pub fn addresses(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self.bps.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// True iff `addr` is registered.
    pub fn contains(&self, addr: u32) -> bool {
        self.bps.contains_key(&addr)
    }
}

// ---- StubSync --------------------------------------------------------------

/// Host-side orchestrator for the EdPro 68k debug stub.
pub struct StubSync<T: UsbTransport> {
    transport: T,
    bps: BreakpointTable,
    no_ack_mode: bool,
    seq: u64,
}

impl<T: UsbTransport> StubSync<T> {
    /// New session. Starts in ack-mode (the gdb default until
    /// `QStartNoAckMode` is acknowledged).
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            bps: BreakpointTable::new(),
            no_ack_mode: false,
            seq: 0,
        }
    }

    /// Whether ack-mode has been disabled via `QStartNoAckMode`.
    pub fn no_ack_mode(&self) -> bool {
        self.no_ack_mode
    }

    /// Borrow the transport (test introspection).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Mutable borrow of the transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Sorted active-breakpoint addresses.
    pub fn list_breakpoints(&self) -> Vec<u32> {
        self.bps.addresses()
    }

    // -----------------------------------------------------------------
    // Public RSP-level API
    // -----------------------------------------------------------------

    /// Run the gdb handshake: `qSupported` then `QStartNoAckMode`. Flips the
    /// internal `no_ack_mode` flag if the stub answered `OK` to the latter.
    /// Returns the parsed feature list from the `qSupported` reply.
    pub async fn handshake(&mut self) -> Result<Vec<(String, String)>> {
        let payload = cmd_query_supported(&["swbreak+", "hwbreak+"]);
        let reply = self.send_rsp_packet(&payload).await?;
        let features = parse_qsupported_reply(&reply);

        let payload = cmd_query_start_no_ack_mode();
        let reply = self.send_rsp_packet(&payload).await?;
        if parse_ok(&reply).is_ok() {
            // Once the stub has acknowledged we stop emitting acks AND stop
            // expecting them on inbound packets. Everything from the next
            // packet on is bare framed RSP bytes.
            self.no_ack_mode = true;
        }
        Ok(features)
    }

    /// `g` — pull all general registers as a raw byte buffer. The caller
    /// (m68k tool layer) decodes the 18-long order.
    pub async fn read_registers(&mut self) -> Result<Vec<u8>> {
        let reply = self.send_rsp_packet(&cmd_read_registers()).await?;
        decode_hex_payload(&reply)
    }

    /// `G<hex>` — write the full register block.
    pub async fn write_registers(&mut self, data: &[u8]) -> Result<()> {
        let reply = self.send_rsp_packet(&cmd_write_registers(data)).await?;
        parse_ok(&reply).map_err(StubSyncError::from)
    }

    /// `m<addr>,<len>` — read `len` bytes starting at `addr` (host endianness
    /// is preserved by gdb, no swap).
    pub async fn read_memory(&mut self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let reply = self.send_rsp_packet(&cmd_read_memory(addr, len)).await?;
        decode_hex_payload(&reply)
    }

    /// `M<addr>,<len>:<hex>` — write `data` starting at `addr`.
    pub async fn write_memory(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let reply = self.send_rsp_packet(&cmd_write_memory(addr, data)).await?;
        parse_ok(&reply).map_err(StubSyncError::from)
    }

    /// Set a software breakpoint at `addr`: `m addr,2` -> save -> `M addr,2:4e41`.
    /// Idempotent: re-setting an already-registered address is a no-op.
    pub async fn set_breakpoint(&mut self, addr: u32) -> Result<()> {
        if self.bps.contains(addr) {
            return Ok(());
        }
        let orig = self.read_memory(addr, 2).await?;
        if orig.len() != 2 {
            return Err(StubSyncError::UnexpectedReply(orig));
        }
        let mut saved = [0u8; 2];
        saved.copy_from_slice(&orig);
        self.write_memory(addr, &TRAP1_OPCODE).await?;
        self.bps.insert(addr, saved);
        Ok(())
    }

    /// Clear a software breakpoint at `addr`. Restores the saved word and
    /// drops the table entry. No-op if no BP was registered at `addr`.
    pub async fn clear_breakpoint(&mut self, addr: u32) -> Result<()> {
        let Some(saved) = self.bps.get(addr) else {
            return Ok(());
        };
        self.write_memory(addr, &saved).await?;
        self.bps.remove(addr);
        Ok(())
    }

    /// `s` — single-step one instruction (the stub sets the T-bit and lets
    /// the CPU run; the trace exception bounces us right back).
    pub async fn step_instruction(&mut self) -> Result<StopReply> {
        let reply = self.send_rsp_packet(&cmd_step(None)).await?;
        Ok(parse_stop_reply(&reply)?)
    }

    /// `c` — resume execution until the next BP or trap.
    pub async fn continue_(&mut self) -> Result<StopReply> {
        let reply = self.send_rsp_packet(&cmd_continue(None)).await?;
        Ok(parse_stop_reply(&reply)?)
    }

    /// `?` — query why the target is halted right now.
    pub async fn query_halt_reason(&mut self) -> Result<StopReply> {
        let reply = self.send_rsp_packet(&cmd_query_halt_reason()).await?;
        Ok(parse_stop_reply(&reply)?)
    }

    /// After a BP hit at `addr`: restore the original opcode so the CPU can
    /// re-execute the instruction on resume — but **keep** the table entry
    /// (the user still wants to break here next time). The caller is
    /// expected to step + re-arm before continuing (TODO M5.4 helper).
    pub async fn on_breakpoint_hit(&mut self, addr: u32) -> Result<()> {
        let saved = self
            .bps
            .get(addr)
            .ok_or_else(|| StubSyncError::UnexpectedReply(format!("no bp at {addr:#x}").into()))?;
        self.write_memory(addr, &saved).await?;
        // Intentionally do NOT remove from the table — see doc comment.
        Ok(())
    }

    // -----------------------------------------------------------------
    // Internal: send_rsp_packet
    // -----------------------------------------------------------------

    /// Encode `payload`, push through `proto::usb_write`, read back reply
    /// (with optional ack handling, up to [`MAX_RETRANSMITS`] retries).
    /// Returns the decoded (de-escaped, RLE-expanded) reply payload.
    async fn send_rsp_packet(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        let frame = encode_packet(payload);
        self.seq = self.seq.wrapping_add(1);

        // Attempt loop: 1 send + up to MAX_RETRANSMITS retransmits.
        let mut attempts: u32 = 0;
        let reply_bytes = loop {
            proto::usb_write(&mut self.transport, &frame).await?;

            if !self.no_ack_mode {
                // Read 1 byte: must be '+' or '-'. Other bytes => out of sync.
                let mut ack = [0u8; 1];
                self.transport.read_exact(&mut ack).await?;
                match rsp::try_parse_ack(ack[0]) {
                    Some(rsp::AckByte::Ok) => { /* fall through to read reply */ }
                    Some(rsp::AckByte::Retransmit) => {
                        attempts += 1;
                        if attempts > MAX_RETRANSMITS {
                            return Err(StubSyncError::MaxRetransmits);
                        }
                        continue;
                    }
                    None => {
                        return Err(StubSyncError::UnexpectedReply(vec![ack[0]]));
                    }
                }
            }

            // Accumulate raw bytes between '$' and '#xx'; decode_packet
            // will un-escape and expand any RLE.
            let raw = self.read_framed_packet().await?;
            break raw;
        };

        // Decode + send '+' back if ack-mode is on.
        let (decoded, _consumed) = decode_packet(&reply_bytes)?;

        if !self.no_ack_mode {
            // Single-byte ack frame, also wrapped in USB_WR (the MCU only
            // forwards USB_WR payloads — see §4.4 of the design doc).
            proto::usb_write(&mut self.transport, b"+").await?;
        }

        Ok(decoded)
    }

    /// Read raw bytes from the transport until we have a full
    /// `$...#xx` packet. Returns the bytes including the leading `$` and
    /// trailing 2-byte hex checksum.
    async fn read_framed_packet(&mut self) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(32);
        let mut byte = [0u8; 1];

        // Skip junk until '$' (gdb reference does the same).
        loop {
            self.transport.read_exact(&mut byte).await?;
            if byte[0] == b'$' {
                buf.push(b'$');
                break;
            }
        }

        // Read until we hit '#'.
        loop {
            self.transport.read_exact(&mut byte).await?;
            buf.push(byte[0]);
            if byte[0] == b'#' {
                break;
            }
        }

        // Two checksum hex chars.
        let mut csum = [0u8; 2];
        self.transport.read_exact(&mut csum).await?;
        buf.extend_from_slice(&csum);

        Ok(buf)
    }
}

// ---- Helpers ---------------------------------------------------------------

fn decode_hex_payload(payload: &[u8]) -> Result<Vec<u8>> {
    // The `m`/`g` reply is just a hex blob unless the stub returned `E<xx>`.
    if payload.len() == 3 && payload[0] == b'E' {
        return Err(StubSyncError::UnexpectedReply(payload.to_vec()));
    }
    parse_hex_bytes(payload).map_err(StubSyncError::from)
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;

    /// Single framed RSP reply, optionally `+`-prefixed for ack-mode tests.
    fn rep(payload: &[u8], ack: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if ack {
            v.push(b'+');
        }
        v.extend_from_slice(&encode_packet(payload));
        v
    }

    fn mk(parts: Vec<Vec<u8>>) -> StubSync<MockUsb> {
        StubSync::new(MockUsb::with_replies(parts))
    }

    // ---- BreakpointTable -------------------------------------------------

    #[test]
    fn bp_table_insert_get_remove() {
        let mut t = BreakpointTable::new();
        assert!(t.is_empty());
        t.insert(0x200, [0xAB, 0xCD]);
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(0x200), Some([0xAB, 0xCD]));
        assert!(t.contains(0x200));
        assert_eq!(t.remove(0x200), Some([0xAB, 0xCD]));
        assert!(t.is_empty());
    }

    #[test]
    fn bp_table_addresses_sorted() {
        let mut t = BreakpointTable::new();
        t.insert(0x300, [0; 2]);
        t.insert(0x100, [0; 2]);
        t.insert(0x200, [0; 2]);
        assert_eq!(t.addresses(), vec![0x100, 0x200, 0x300]);
    }

    // ---- handshake -------------------------------------------------------

    #[tokio::test]
    async fn handshake_parses_qsupported_and_flips_noack() {
        let mut s = mk(vec![
            rep(b"PacketSize=400;swbreak+;hwbreak+", true),
            rep(b"OK", true),
        ]);
        let f = s.handshake().await.unwrap();
        assert!(f.contains(&("PacketSize".into(), "400".into())));
        assert!(f.contains(&("swbreak".into(), "+".into())));
        assert!(s.no_ack_mode());
    }

    #[tokio::test]
    async fn handshake_keeps_ack_mode_on_non_ok() {
        let mut s = mk(vec![rep(b"", true), rep(b"", true)]);
        s.handshake().await.unwrap();
        assert!(!s.no_ack_mode());
    }

    // ---- read/write memory ----------------------------------------------

    #[tokio::test]
    async fn read_memory_decodes_hex() {
        let mut s = mk(vec![rep(b"deadbeef", true)]);
        assert_eq!(
            s.read_memory(0xFF8000, 4).await.unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
    }

    #[tokio::test]
    async fn read_memory_propagates_e01() {
        let mut s = mk(vec![rep(b"E01", true)]);
        match s.read_memory(0, 4).await.unwrap_err() {
            StubSyncError::UnexpectedReply(p) => assert_eq!(p, b"E01"),
            other => panic!("wrong err: {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_memory_expects_ok() {
        let mut s = mk(vec![rep(b"OK", true)]);
        s.write_memory(0xFF0000, &[1, 2, 3]).await.unwrap();
    }

    #[tokio::test]
    async fn write_memory_rejects_non_ok() {
        let mut s = mk(vec![rep(b"E22", true)]);
        assert!(s.write_memory(0, &[0]).await.is_err());
    }

    // ---- set_breakpoint --------------------------------------------------

    #[tokio::test]
    async fn set_breakpoint_reads_then_patches() {
        let mut s = mk(vec![rep(b"1234", true), rep(b"OK", true)]);
        s.set_breakpoint(0x200).await.unwrap();
        assert!(s.list_breakpoints().contains(&0x200));
        assert_eq!(s.bps.get(0x200), Some([0x12, 0x34]));
        let needle = encode_packet(b"M200,2:4e41");
        let concat: Vec<u8> = s.transport().tx_log().to_vec();
        assert!(concat.windows(needle.len()).any(|w| w == needle));
    }

    #[tokio::test]
    async fn set_breakpoint_idempotent() {
        // Only one set's worth of replies — second call must do zero I/O.
        let mut s = mk(vec![rep(b"abcd", true), rep(b"OK", true)]);
        s.set_breakpoint(0x300).await.unwrap();
        s.set_breakpoint(0x300).await.unwrap();
        assert_eq!(s.list_breakpoints(), vec![0x300]);
    }

    // ---- clear_breakpoint ------------------------------------------------

    #[tokio::test]
    async fn clear_breakpoint_restores_word() {
        let mut s = mk(vec![rep(b"abcd", true), rep(b"OK", true), rep(b"OK", true)]);
        s.set_breakpoint(0x200).await.unwrap();
        s.clear_breakpoint(0x200).await.unwrap();
        assert!(s.list_breakpoints().is_empty());
        let needle = encode_packet(b"M200,2:abcd");
        let concat = s.transport().tx_log().to_vec();
        assert!(concat.windows(needle.len()).any(|w| w == needle));
    }

    #[tokio::test]
    async fn clear_breakpoint_missing_is_noop() {
        let mut s = StubSync::new(MockUsb::new());
        s.clear_breakpoint(0x999).await.unwrap();
        assert_eq!(s.transport().tx_frames().len(), 0);
    }

    // ---- list_breakpoints sorted ----------------------------------------

    #[tokio::test]
    async fn list_breakpoints_returns_sorted() {
        let mut s = mk(vec![
            rep(b"1234", true),
            rep(b"OK", true),
            rep(b"5678", true),
            rep(b"OK", true),
        ]);
        s.set_breakpoint(0x300).await.unwrap();
        s.set_breakpoint(0x100).await.unwrap();
        assert_eq!(s.list_breakpoints(), vec![0x100, 0x300]);
    }

    // ---- step / continue / halt -----------------------------------------

    #[tokio::test]
    async fn step_returns_stop_reply() {
        let mut s = mk(vec![rep(b"S05", true)]);
        assert_eq!(s.step_instruction().await.unwrap(), StopReply::Sig(0x05));
    }

    #[tokio::test]
    async fn continue_returns_stop_reply() {
        let mut s = mk(vec![rep(b"S05", true)]);
        assert_eq!(s.continue_().await.unwrap(), StopReply::Sig(0x05));
    }

    #[tokio::test]
    async fn query_halt_reason_parses_t_reply() {
        let mut s = mk(vec![rep(b"T05thread:01;", true)]);
        match s.query_halt_reason().await.unwrap() {
            StopReply::TrapAt { signal, .. } => assert_eq!(signal, 0x05),
            _ => panic!("expected TrapAt"),
        }
    }

    // ---- on_breakpoint_hit ----------------------------------------------

    #[tokio::test]
    async fn on_breakpoint_hit_restores_keeps_entry() {
        let mut s = mk(vec![
            rep(b"1234", true), // m for set
            rep(b"OK", true),   // M for set
            rep(b"OK", true),   // M for restore
        ]);
        s.set_breakpoint(0x400).await.unwrap();
        s.on_breakpoint_hit(0x400).await.unwrap();
        assert_eq!(s.list_breakpoints(), vec![0x400]);
        assert_eq!(s.bps.get(0x400), Some([0x12, 0x34]));
    }

    #[tokio::test]
    async fn on_breakpoint_hit_unknown_addr_errors() {
        let mut s = StubSync::new(MockUsb::new());
        assert!(s.on_breakpoint_hit(0xDEAD).await.is_err());
    }

    // ---- ack-mode wire handling -----------------------------------------

    #[tokio::test]
    async fn ack_mode_sends_plus_back_after_reply() {
        let mut s = mk(vec![rep(b"OK", true)]);
        s.write_memory(0, &[0]).await.unwrap();
        // Look for a 1-byte tx frame == "+".
        assert!(s.transport().tx_frames().iter().any(|f| f.as_slice() == b"+"));
    }

    #[tokio::test]
    async fn no_ack_mode_skips_ack_traffic() {
        let mut s = mk(vec![
            rep(b"PacketSize=400", true),
            rep(b"OK", true),
            rep(b"deadbeef", false), // no leading ack
        ]);
        s.handshake().await.unwrap();
        assert!(s.no_ack_mode());
        let pre = s.transport().tx_frames().len();
        assert_eq!(
            s.read_memory(0xFF0000, 4).await.unwrap(),
            vec![0xDE, 0xAD, 0xBE, 0xEF]
        );
        let new = &s.transport().tx_frames()[pre..];
        assert!(!new.iter().any(|f| f.as_slice() == b"+"));
    }

    // ---- retransmit ------------------------------------------------------

    fn nak_then_ok(n: usize, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut v: Vec<Vec<u8>> = (0..n).map(|_| vec![b'-']).collect();
        v.push(rep(payload, true));
        v
    }

    #[tokio::test]
    async fn retransmits_recover_within_limit() {
        let mut s = StubSync::new(MockUsb::with_replies(nak_then_ok(2, b"OK")));
        s.write_memory(0, &[0]).await.unwrap();
    }

    #[tokio::test]
    async fn retransmits_succeed_at_exactly_three() {
        let mut s = StubSync::new(MockUsb::with_replies(nak_then_ok(3, b"OK")));
        s.write_memory(0, &[0]).await.unwrap();
    }

    #[tokio::test]
    async fn retransmits_fail_on_fourth_nak() {
        let mut s = StubSync::new(MockUsb::with_replies(vec![
            vec![b'-'],
            vec![b'-'],
            vec![b'-'],
            vec![b'-'],
            rep(b"OK", true),
        ]));
        assert!(matches!(
            s.write_memory(0, &[0]).await.unwrap_err(),
            StubSyncError::MaxRetransmits
        ));
    }

    #[tokio::test]
    async fn unexpected_ack_byte_errors() {
        let mut s = mk(vec![vec![b'X']]);
        assert!(matches!(
            s.write_memory(0, &[0]).await.unwrap_err(),
            StubSyncError::UnexpectedReply(_)
        ));
    }

    // ---- registers -------------------------------------------------------

    #[tokio::test]
    async fn read_registers_decodes_full_block() {
        let mut hex = String::new();
        for i in 0..72u8 {
            hex.push_str(&format!("{i:02x}"));
        }
        let mut s = mk(vec![rep(hex.as_bytes(), true)]);
        let r = s.read_registers().await.unwrap();
        assert_eq!(r.len(), 72);
        assert_eq!(r[71], 71);
    }

    #[tokio::test]
    async fn write_registers_round_trips() {
        let mut s = mk(vec![rep(b"OK", true)]);
        s.write_registers(&[0xDE, 0xAD, 0xBE, 0xEF]).await.unwrap();
        let needle = encode_packet(b"Gdeadbeef");
        let concat = s.transport().tx_log().to_vec();
        assert!(concat.windows(needle.len()).any(|w| w == needle));
    }

    // ---- Display impls ---------------------------------------------------

    #[test]
    fn display_max_retransmits() {
        assert!(format!("{}", StubSyncError::MaxRetransmits).contains("NAKed"));
    }

    #[test]
    fn display_rsp_wrap() {
        assert!(format!("{}", StubSyncError::Rsp(RspError::NoPacketStart)).contains("rsp codec"));
    }
}
