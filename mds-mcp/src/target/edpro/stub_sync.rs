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
use super::stub_blob;
use super::rsp::{
    self, cmd_continue, cmd_qmds_cram, cmd_qmds_vdp_status, cmd_qmds_vram, cmd_qmds_vsram,
    cmd_query_halt_reason, cmd_query_start_no_ack_mode, cmd_query_supported, cmd_read_memory,
    cmd_read_registers, cmd_step, cmd_write_memory, cmd_write_registers, decode_packet,
    encode_packet, parse_hex_bytes, parse_ok, parse_qsupported_reply, parse_stop_reply, RspError,
    StopReply,
};
use super::usb::UsbTransport;

/// Maximum bytes per qMdsVram request — must match `VRAM_CHUNK_MAX` in
/// `mds-stub-68k/src/stub.c`. Larger reads must be chunked by the caller.
pub const VRAM_CHUNK_MAX: u32 = 128;

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

    // -----------------------------------------------------------------
    // M5.9: pause / unpause via VBL hijack.
    //
    // Pause works by patching vector $78 (level-6 IRQ / VBL) to land in
    // the stub on the next VBL frame, which sees a paused-flag and
    // enters the RSP loop. The original VBL handler is captured by the
    // host (via MEM_RD) so the stub's "fast path" can chain to it
    // until the pause actually fires.
    //
    // See `mds-stub-68k/src/entry.s` (header layout + vbl thunk),
    // `stub_blob::pause` / `stub_blob::unpause` (host-side memory ops),
    // and `docs/02-m5-architecture.md` §5.9.
    // -----------------------------------------------------------------

    /// M5.9: arm a pause and wait for the next VBL to land us in the
    /// stub. Returns a tuple of (`StopReply` from the stub, captured
    /// `original_vbl` handler addr — caller must hand this back to
    /// [`Self::unpause`] before resuming).
    ///
    /// `blob_load_addr` is the PI-bus address the stub blob lives at
    /// (typically `stub_blob::STUB_LOAD_ADDR.0 = 0x0030_0000`). It
    /// drives the offsets to the `paused_flag` / `original_vbl` slots
    /// inside the blob header.
    ///
    /// `entry_vbl` is the absolute 68k address of the stub's VBL thunk
    /// (parsed from the blob header by `parse_header`). Caller is
    /// expected to cache this from connect-time.
    pub async fn pause(
        &mut self,
        blob_load_addr: u32,
        entry_vbl: u32,
    ) -> Result<(StopReply, u32)> {
        // Install the VBL hijack. `stub_blob::pause` does MEM_RD + 3
        // MEM_WRs and returns the captured original handler addr.
        let original_vbl = stub_blob::pause(&mut self.transport, blob_load_addr, entry_vbl)
            .await
            .map_err(|e| StubSyncError::Transport(e.to_string()))?;

        // Wait for the stub's stop reply. We DON'T expect any RSP ack
        // dance here even in ack-mode-on: the stub's stop reply is an
        // unsolicited packet (no host packet preceded it), so the host
        // just sends `+` after consuming it.
        let raw = self.read_framed_packet().await?;
        let (decoded, _consumed) = decode_packet(&raw)?;
        if !self.no_ack_mode {
            proto::usb_write(&mut self.transport, b"+").await?;
        }
        let stop = parse_stop_reply(&decoded)?;
        Ok((stop, original_vbl))
    }

    /// M5.9: undo the VBL hijack put in place by [`Self::pause`].
    /// Order: clear the paused-flag, then restore vector `$78`. The
    /// caller is responsible for issuing the RSP `c` (or equivalent)
    /// AFTER this call — by then the stub is no longer trapping any
    /// VBL at all, so the resume just lets the previously-paused CPU
    /// continue.
    pub async fn unpause(
        &mut self,
        blob_load_addr: u32,
        original_vbl: u32,
    ) -> Result<()> {
        stub_blob::unpause(&mut self.transport, blob_load_addr, original_vbl)
            .await
            .map_err(|e| StubSyncError::Transport(e.to_string()))?;
        Ok(())
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
    // M5.7: VDP-indirect reads (qMds* monitor commands).
    // -----------------------------------------------------------------
    //
    // These wrap the stub's custom `qMdsCram` / `qMdsVsram` /
    // `qMdsVdpStatus` / `qMdsVram` packets. Stub-side helpers do the
    // address-set + data-port read loop; host parses hex-encoded payloads.
    //
    // VDP state references: see `docs/02-m5-architecture.md` §5.7.

    /// Read all 128 bytes of CRAM (64 9-bit BGR colour entries).
    pub async fn read_cram(&mut self) -> Result<[u8; 128]> {
        let reply = self.send_rsp_packet(&cmd_qmds_cram()).await?;
        let bytes = decode_hex_payload(&reply)?;
        if bytes.len() != 128 {
            return Err(StubSyncError::UnexpectedReply(bytes));
        }
        let mut out = [0u8; 128];
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// Read all 80 bytes of VSRAM (40 vertical-scroll word entries).
    pub async fn read_vsram(&mut self) -> Result<[u8; 80]> {
        let reply = self.send_rsp_packet(&cmd_qmds_vsram()).await?;
        let bytes = decode_hex_payload(&reply)?;
        if bytes.len() != 80 {
            return Err(StubSyncError::UnexpectedReply(bytes));
        }
        let mut out = [0u8; 80];
        out.copy_from_slice(&bytes);
        Ok(out)
    }

    /// Read the VDP status word from `$C00004` (one word, 4 hex digits).
    pub async fn read_vdp_status(&mut self) -> Result<u16> {
        let reply = self.send_rsp_packet(&cmd_qmds_vdp_status()).await?;
        let bytes = decode_hex_payload(&reply)?;
        if bytes.len() != 2 {
            return Err(StubSyncError::UnexpectedReply(bytes));
        }
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// Read up to [`VRAM_CHUNK_MAX`] bytes of VRAM at `addr`. Caller is
    /// responsible for chunking larger reads. Stub silently truncates
    /// `len > VRAM_CHUNK_MAX` and word-aligns odd `len` upward.
    pub async fn read_vram(&mut self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let reply = self.send_rsp_packet(&cmd_qmds_vram(addr, len)).await?;
        decode_hex_payload(&reply)
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

    // ---- M5.7 VDP reads --------------------------------------------------

    /// Hex-encode a byte slice (lowercase) — local helper for canned-reply
    /// payloads in the M5.7 tests below.
    fn hex(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        out
    }

    #[tokio::test]
    async fn read_cram_decodes_128_bytes() {
        let raw: Vec<u8> = (0..128).map(|i| i as u8).collect();
        let mut s = mk(vec![rep(&hex(&raw), true)]);
        let cram = s.read_cram().await.unwrap();
        assert_eq!(&cram[..], &raw[..]);
        // Confirm the wire payload is the qMdsCram packet.
        let needle = encode_packet(b"qMdsCram");
        assert!(s.transport().tx_log().windows(needle.len()).any(|w| w == needle));
    }

    #[tokio::test]
    async fn read_cram_rejects_wrong_length() {
        let mut s = mk(vec![rep(&hex(&[0u8; 64]), true)]);
        assert!(matches!(
            s.read_cram().await.unwrap_err(),
            StubSyncError::UnexpectedReply(_)
        ));
    }

    #[tokio::test]
    async fn read_vsram_decodes_80_bytes() {
        let raw: Vec<u8> = (0..80).map(|i| (i ^ 0xA5) as u8).collect();
        let mut s = mk(vec![rep(&hex(&raw), true)]);
        let vsram = s.read_vsram().await.unwrap();
        assert_eq!(&vsram[..], &raw[..]);
    }

    #[tokio::test]
    async fn read_vdp_status_parses_word_be() {
        // VDP status = 0x3408 → 4 hex chars "3408"
        let mut s = mk(vec![rep(b"3408", true)]);
        assert_eq!(s.read_vdp_status().await.unwrap(), 0x3408);
    }

    #[tokio::test]
    async fn read_vram_returns_arbitrary_chunk() {
        // 32 bytes (one tile worth)
        let raw: Vec<u8> = (0..32).map(|i| (0x10 + i) as u8).collect();
        let mut s = mk(vec![rep(&hex(&raw), true)]);
        let got = s.read_vram(0xC000, 32).await.unwrap();
        assert_eq!(got, raw);
        let needle = encode_packet(b"qMdsVram:c000,20");
        assert!(s.transport().tx_log().windows(needle.len()).any(|w| w == needle));
    }

    #[tokio::test]
    async fn read_vram_propagates_error_reply() {
        let mut s = mk(vec![rep(b"E01", true)]);
        assert!(matches!(
            s.read_vram(0, 16).await.unwrap_err(),
            StubSyncError::UnexpectedReply(_)
        ));
    }

    // ---- M5.9: pause / unpause ------------------------------------------

    /// Build the rx queue for a successful `pause()` call.
    /// Order is: MEM_RD answer (4 bytes), then 3 MEM_WR per-chunk acks
    /// (one each for the writes to `original_vbl`, `paused_flag`,
    /// vector `$78`), then the stop reply from the cart.
    fn pause_replies(original_vbl: u32, stop_payload: &[u8]) -> Vec<Vec<u8>> {
        vec![
            original_vbl.to_be_bytes().to_vec(),
            vec![0u8],
            vec![0u8],
            vec![0u8],
            // Stop reply isn't framed with a leading `+` — the stub
            // emits it unsolicited; we'll send `+` BACK after we read it.
            super::super::rsp::encode_packet(stop_payload),
        ]
    }

    #[tokio::test]
    async fn pause_sends_mem_ops_and_reads_stop_reply() {
        let blob_load: u32 = 0x0030_0000;
        let entry_vbl: u32 = 0x0030_0A1C;
        let original_vbl: u32 = 0x0010_5678;

        let mut s = StubSync::new(MockUsb::with_replies(pause_replies(
            original_vbl,
            b"T05",
        )));
        // Start in ack-mode-on so we get coverage of the `+` reply path.
        let (stop, got_vbl) = s.pause(blob_load, entry_vbl).await.unwrap();
        assert_eq!(got_vbl, original_vbl);
        assert!(matches!(stop, StopReply::TrapAt { signal: 0x05, .. }));

        // The wire log must contain a MEM_WR to vector $78 with `entry_vbl`
        // as the payload, AND a `+` ack from the host after the stop reply.
        let log = s.transport().tx_log();
        // MEM_WR header (13 bytes) + 4-byte payload — find the vector $78 write.
        let mut found_vec78 = false;
        for w in log.windows(8) {
            if w[..4] == [0x2B, 0xD4, 0x1A, 0xE5]
                && w[4..8] == 0x78u32.to_be_bytes()
            {
                found_vec78 = true;
                break;
            }
        }
        assert!(found_vec78, "MEM_WR to vector $78 missing from tx log");
        // The host's `+` ack:
        let frames = s.transport().tx_frames();
        assert!(frames.iter().any(|f| f.as_slice() == b"+"));
    }

    #[tokio::test]
    async fn pause_no_ack_mode_skips_ack_send() {
        // After handshake, ack mode is off. Pause should NOT send `+`
        // back after the stop reply.
        let mut s = mk(vec![
            rep(b"PacketSize=190;swbreak+", true),
            rep(b"OK", true),
        ]);
        s.handshake().await.unwrap();
        assert!(s.no_ack_mode());

        let pre_frames = s.transport().tx_frames().len();

        // Now feed pause replies. The stop reply is bare (no leading `+`).
        for r in pause_replies(0xCAFE_BABE, b"T05") {
            s.transport_mut().push_reply(&r);
        }
        let (_stop, got_vbl) = s.pause(0x0030_0000, 0x0030_0A1C).await.unwrap();
        assert_eq!(got_vbl, 0xCAFE_BABE);

        let new = &s.transport().tx_frames()[pre_frames..];
        assert!(
            !new.iter().any(|f| f.as_slice() == b"+"),
            "no_ack_mode pause must not send a `+` ack"
        );
    }

    #[tokio::test]
    async fn unpause_emits_two_mem_writes_in_order() {
        let blob_load: u32 = 0x0030_0000;
        let original_vbl: u32 = 0x0010_5678;
        // 2 MEM_WR ack bytes (one per write).
        let mut s = StubSync::new(MockUsb::with_replies(vec![vec![0u8], vec![0u8]]));
        s.unpause(blob_load, original_vbl).await.unwrap();

        let frames = s.transport().tx_frames();
        let wr_headers: Vec<&[u8]> = frames
            .iter()
            .filter(|f| f.len() == 13 && f[0..4] == [0x2B, 0xD4, 0x1A, 0xE5])
            .map(|v| v.as_slice())
            .collect();
        assert_eq!(wr_headers.len(), 2);
        // Order is documented: clear flag first (offset 0x10), then $78.
        assert_eq!(
            &wr_headers[0][4..8],
            &(blob_load + 0x10).to_be_bytes()
        );
        assert_eq!(&wr_headers[1][4..8], &0x78u32.to_be_bytes());
    }

    #[tokio::test]
    async fn pause_then_unpause_round_trip() {
        // Full happy-path: pause arms the hijack, then unpause clears
        // it. The wire log has the read+3 writes + stop reply on pause,
        // and 2 writes on unpause.
        let blob_load: u32 = 0x0030_0000;
        let entry_vbl: u32 = 0x0030_0A1C;
        let original_vbl: u32 = 0x0010_5678;

        let mut all = pause_replies(original_vbl, b"T05");
        all.push(vec![0u8]); // unpause MEM_WR ack 1
        all.push(vec![0u8]); // unpause MEM_WR ack 2
        let mut s = StubSync::new(MockUsb::with_replies(all));

        let (_stop, got_vbl) = s.pause(blob_load, entry_vbl).await.unwrap();
        s.unpause(blob_load, got_vbl).await.unwrap();

        // Tx log contains BOTH a write of `entry_vbl` and a write of
        // `original_vbl` (the second one comes from unpause).
        let frames = s.transport().tx_frames();
        let payloads_4: Vec<&Vec<u8>> = frames.iter().filter(|f| f.len() == 4).collect();
        assert!(payloads_4
            .iter()
            .any(|f| f.as_slice() == entry_vbl.to_be_bytes()));
        assert!(payloads_4
            .iter()
            .any(|f| f.as_slice() == original_vbl.to_be_bytes()));
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
