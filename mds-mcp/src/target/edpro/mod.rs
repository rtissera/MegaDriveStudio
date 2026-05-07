// SPDX-License-Identifier: MIT
//! Mega Everdrive Pro target.
//!
//! M5.5 wires `EdProTarget` tool methods to the `StubSync` host state
//! machine. Hardware-free: callers drive the target through `connect_mock`
//! + a `MockUsb` transport with canned RSP-encoded replies.
//!
//! Higher-up `tools/mod.rs` still short-circuits every emulator-only tool
//! through `block_on_edpro`, so the public MCP surface is unchanged for
//! M5.5. The methods on this struct are the wiring layer that future M5.6+
//! work will plug into the tool dispatcher.
//!
//! Tool mapping (see `docs/02-m5-architecture.md` §7):
//! - `read_memory` / `write_memory` → RSP `m` / `M` via `StubSync`
//! - `set_breakpoint` / `clear_breakpoint` / `list_breakpoints` → BP table
//! - `step_instruction` → RSP `s`
//! - `resume` / `continue_run` → RSP `c` (fire-and-forget; no stop wait)
//! - `get_68k_registers` → RSP `g`, decoded big-endian into 18 longs
//! - `get_vdp_registers` → `m C00004,18` (raw 24 bytes; VDP shadow regs)
//! - `get_palettes` → RSP `qMdsCram` monitor cmd (M5.7) → 128 raw CRAM bytes
//! - `dump_tile` → RSP `qMdsVram:<index*32>,20` monitor cmd (M5.7) → 32 bytes
//! - `pause` / `screenshot` / `save_state` / `load_state` /
//!   `step_frame` / `get_z80_registers` / `get_sprites` /
//!   `load_rom` / `unload_rom` / `mega_input_*` →
//!   `not_supported_on_target` (see TODOs M5.6/M5.8).

// Tool surface methods on `EdProTarget` aren't wired into `tools/mod.rs`
// yet (the dispatcher still short-circuits via `block_on_edpro`). M5.6+
// removes the gate; until then, suppress dead-code warnings on the
// otherwise-unused public methods so `clippy -D warnings` is clean.
#![allow(dead_code)]

pub mod framing;
pub mod proto;
pub mod rsp;
pub mod stub_blob;
pub mod stub_sync;
pub mod usb;

use async_trait::async_trait;

#[allow(unused_imports)] // re-exported for M5.3 callers
pub use rsp::{decode_packet, encode_packet, AckByte, RspError, StopReply};
#[allow(unused_imports)] // re-exported for M5.4+ callers (tools/mod.rs)
pub use stub_sync::{BreakpointTable, StubSync, StubSyncError};

use crate::target::{EdProConfig, TargetKind, NOT_SUPPORTED};
use usb::UsbTransport;

/// VDP registers MMIO base — control port. The on-cart stub reads these
/// via the same `m` RSP packet (since it runs on the 68k and the 24 shadow
/// regs are mirrored to `$C00004` per SGDK convention).
#[allow(dead_code)] // referenced once we wire VDP into `tools/mod.rs`
const VDP_REGS_BASE: u32 = 0x00C0_0004;

// Blanket impl so `StubSync<Box<dyn UsbTransport + Send>>` works.
// `usb.rs` declares `UsbTransport` for concrete types only; this is the
// trait-object delegate. Kept local (mod.rs) per M5.5 scope rule.
#[async_trait]
impl<T: UsbTransport + ?Sized + Send> UsbTransport for Box<T> {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        (**self).read_exact(buf).await
    }
    async fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        (**self).write_all(buf).await
    }
    async fn flush(&mut self) -> anyhow::Result<()> {
        (**self).flush().await
    }
}

/// Decoded 68k register block — matches `crate::emulator::decode::M68kRegisters`
/// shape so callers can serialize identically across targets. The gdb m68k
/// stub returns 18 big-endian longs: D0..D7, A0..A7, PS(=SR padded), PC.
/// Some stubs (incl. mds-stub-68k extended mode) emit USP/SSP as longs
/// 19/20; we accept those when present, default zero otherwise.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EdProM68kRegisters {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u16,
    pub usp: u32,
    pub ssp: u32,
}

/// Decode 18 big-endian longs (with optional 19th/20th) from an RSP `g` reply.
fn decode_g_reply_be(buf: &[u8]) -> anyhow::Result<EdProM68kRegisters> {
    // gdb m68k stub: D0..D7, A0..A7, PS(u32, low 16 = SR), PC. = 18 longs.
    if buf.len() < 18 * 4 {
        anyhow::bail!("g reply too short: {} bytes", buf.len());
    }
    let mut r = EdProM68kRegisters::default();
    let mut off = 0usize;
    fn be_u32(buf: &[u8], o: &mut usize) -> u32 {
        let v = u32::from_be_bytes([buf[*o], buf[*o + 1], buf[*o + 2], buf[*o + 3]]);
        *o += 4;
        v
    }
    for i in 0..8 {
        r.d[i] = be_u32(buf, &mut off);
    }
    for i in 0..8 {
        r.a[i] = be_u32(buf, &mut off);
    }
    let ps = be_u32(buf, &mut off);
    r.sr = ps as u16;
    r.pc = be_u32(buf, &mut off);
    // USP/SSP optional — emitted by some stubs as longs 19/20 (1-indexed).
    // Base set is 18 longs (D0..7=8, A0..7=8, PS=1, PC=1 = 18 → 72 bytes).
    if buf.len() >= 19 * 4 {
        r.usp = be_u32(buf, &mut off);
    }
    if buf.len() >= 20 * 4 {
        r.ssp = be_u32(buf, &mut off);
    }
    Ok(r)
}

/// Public, hardware-targeted MCP backend. Holds the active `StubSync` once
/// connected; every tool method below short-circuits to a `not_connected`
/// error before touching it.
pub struct EdProTarget {
    cfg: EdProConfig,
    sync: Option<StubSync<Box<dyn UsbTransport + Send>>>,
    connected: bool,
    /// Features parsed from `qSupported` during the handshake. Cached so
    /// `get_status` can surface them without round-tripping again.
    features: Vec<(String, String)>,
}

impl EdProTarget {
    pub fn new(cfg: EdProConfig) -> Self {
        Self {
            cfg,
            sync: None,
            connected: false,
            features: Vec::new(),
        }
    }

    pub fn kind(&self) -> TargetKind {
        TargetKind::EdPro
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn port_str(&self) -> String {
        self.cfg.port.to_string_lossy().into_owned()
    }

    /// Cached feature list from `qSupported`.
    pub fn features(&self) -> &[(String, String)] {
        &self.features
    }

    /// Real hardware connect — opens the configured serial port, halts the
    /// CPU, deploys the on-cart 68k debug stub via `MEM_WR`, releases the
    /// CPU, and runs the gdb handshake.
    ///
    /// **Not yet wired.** The serial-port transport (M5.7) is the only
    /// missing piece: once it lands, this becomes:
    ///
    /// ```ignore
    /// let mut t: Box<dyn UsbTransport + Send> = Box::new(SerialUsb::open(&self.cfg.port)?);
    /// proto::host_reset(&mut t, ResetMode::Soft).await?;  // step 1: halt CPU
    /// stub_blob::deploy(&mut t, stub_blob::STUB_BLOB).await?; // steps 2..5
    /// // step 6: optional pre-set BPs (host responsibility, e.g. main())
    /// proto::host_reset(&mut t, ResetMode::Off).await?;   // step 7: release CPU
    /// self.connect_mock(t).await                          // step 8: handshake on first stop
    /// ```
    ///
    /// Until the SerialUsb transport exists, callers in tests use
    /// [`Self::connect_mock`] (handshake only) or the test
    /// [`Self::deploy_stub_then_handshake`] which exercises the full
    /// upload+vector-patch flow against a `MockUsb`.
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        // TODO M5.5b: open serial transport, then call
        // `Self::deploy_stub_then_handshake` with the live `Box<dyn
        // UsbTransport + Send>`. The deployment logic itself is fully
        // covered by `stub_blob::deploy` + `connect_mock` integration tests
        // — the gating concern is opening `/dev/ttyACM*` reliably.
        anyhow::bail!(
            "{NOT_SUPPORTED}: edpro USB transport not yet implemented (port {:?}); use connect_mock in tests",
            self.cfg.port
        );
    }

    /// Test / future-real-USB helper: take an already-open transport, run
    /// the full attach flow per fact-check C20 (krikzz halt-first pattern):
    ///   1. `HOST_RST(Soft)` — halt CPU
    ///   2. `MEM_WR` stub blob to PSRAM load address
    ///   3. `MEM_WR` vector $24 / $84 in PSRAM
    ///   4. `HOST_RST(Off)`  — release CPU
    ///   5. gdb handshake on first stop reply
    ///
    /// Real-USB callers will pass the live `transport` straight through —
    /// the handshake reads from it as it would in production.
    pub async fn deploy_stub_then_handshake(
        &mut self,
        mut transport: Box<dyn UsbTransport + Send>,
    ) -> anyhow::Result<()> {
        // We pass `&mut transport` (a `&mut Box<dyn UsbTransport + Send>`);
        // the blanket impl `UsbTransport for Box<T>` makes the Box itself
        // a valid (Sized) `T: UsbTransport`, so the proto helpers accept it
        // without `?Sized` plumbing. Same trick for the stub_blob deploy.
        proto::host_reset(&mut transport, proto::ResetMode::Soft)
            .await
            .map_err(|e| anyhow::anyhow!("host_reset (halt) failed: {e}"))?;
        stub_blob::deploy(&mut transport, stub_blob::STUB_BLOB)
            .await
            .map_err(|e| anyhow::anyhow!("stub deploy failed: {e}"))?;
        proto::host_reset(&mut transport, proto::ResetMode::Off)
            .await
            .map_err(|e| anyhow::anyhow!("host_reset (release) failed: {e}"))?;
        self.connect_mock(transport).await
    }

    /// Test helper: install an arbitrary `UsbTransport` (typically `MockUsb`
    /// pre-loaded with canned replies) and run the gdb handshake. Flips
    /// `connected = true` on success.
    pub async fn connect_mock(
        &mut self,
        transport: Box<dyn UsbTransport + Send>,
    ) -> anyhow::Result<()> {
        let mut sync = StubSync::new(transport);
        let features = sync
            .handshake()
            .await
            .map_err(|e| anyhow::anyhow!("handshake failed: {e}"))?;
        self.features = features;
        self.sync = Some(sync);
        self.connected = true;
        Ok(())
    }

    /// Test helper: install a transport without running the handshake.
    /// Useful for verifying disconnected-vs-raw-transport tx logs.
    #[cfg(test)]
    pub fn connect_mock_no_handshake(&mut self, transport: Box<dyn UsbTransport + Send>) {
        self.sync = Some(StubSync::new(transport));
        self.connected = true;
    }

    fn sync_mut(&mut self) -> anyhow::Result<&mut StubSync<Box<dyn UsbTransport + Send>>> {
        self.sync
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("{NOT_SUPPORTED}: edpro target not connected"))
    }

    fn sync_ref(&self) -> Option<&StubSync<Box<dyn UsbTransport + Send>>> {
        self.sync.as_ref()
    }

    // -------------------------------------------------------------------
    // Tool surface (matches docs/02-m5-architecture.md §7).
    // -------------------------------------------------------------------

    /// `mega_load_rom` — would need MEM_WR + HOST_RST upload via
    /// `proto::mem_write`. Out of scope for M5.5 (cart already has the ROM
    /// flashed; later milestones may add live upload).
    pub async fn load_rom(&mut self, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// `mega_unload_rom` — N/A on hardware (ROM is on the cart). M5.6 may
    /// reuse this as `cmd_host_reset(off)`.
    pub async fn unload_rom(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// `mega_pause` — needs IRQ7 injection on hardware (the stub can't
    /// stop a running CPU without an external pulse). Tracked as §10 open
    /// question. Returns `not_supported_on_target` until the mechanism is
    /// designed.
    pub async fn pause(&mut self) -> anyhow::Result<()> {
        anyhow::bail!("{NOT_SUPPORTED}: hardware pause needs IRQ injection (TODO M5.x)");
    }

    /// `mega_resume` — RSP `c`, fire-and-forget. We do **not** wait for
    /// the next stop reply (per M5.4 stub guidance: only stop-reply or
    /// timeout, never `OK`). The caller polls/notifies separately.
    pub async fn resume(&mut self) -> anyhow::Result<()> {
        let s = self.sync_mut()?;
        // Send the framed `c` and bail without reading; the stop-reply
        // pump will surface the trap asynchronously in M5.6.
        let frame = rsp::encode_packet(&rsp::cmd_continue(None));
        proto::usb_write(s.transport_mut(), &frame)
            .await
            .map_err(|e| anyhow::anyhow!("usb_write failed: {e}"))?;
        Ok(())
    }

    /// `mega_continue` — same wire op as `resume`, kept distinct so future
    /// callers can attach BP-restoration logic post-halt.
    pub async fn continue_run(&mut self) -> anyhow::Result<()> {
        self.resume().await
    }

    /// `mega_step_frame` — frames are an emulator concept; hardware can't
    /// single-frame. Permanent `not_supported_on_target`.
    pub async fn step_frame(&mut self, _n: u32) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// `mega_step_instruction` — RSP `s`, awaits the trace-trap stop reply.
    pub async fn step_instruction(&mut self) -> anyhow::Result<StopReply> {
        let s = self.sync_mut()?;
        s.step_instruction()
            .await
            .map_err(|e| anyhow::anyhow!("step_instruction failed: {e}"))
    }

    /// `mega_read_memory` — RSP `m addr,len`. Returns raw bytes.
    pub async fn read_memory(&mut self, addr: u32, len: u32) -> anyhow::Result<Vec<u8>> {
        let s = self.sync_mut()?;
        s.read_memory(addr, len)
            .await
            .map_err(|e| anyhow::anyhow!("read_memory failed: {e}"))
    }

    /// `mega_write_memory` — RSP `M addr,len:hex`. Caller is responsible
    /// for ensuring `addr` is in writable memory (RAM/PSRAM, not ROM).
    pub async fn write_memory(&mut self, addr: u32, data: &[u8]) -> anyhow::Result<()> {
        let s = self.sync_mut()?;
        s.write_memory(addr, data)
            .await
            .map_err(|e| anyhow::anyhow!("write_memory failed: {e}"))
    }

    /// `mega_get_vdp_registers` — read 24 raw bytes from the VDP control
    /// port. The on-cart stub services the `m` RSP packet via a 68k MMIO
    /// read since it runs on the same bus. Tool layer (M5.6) decodes.
    pub async fn get_vdp_registers(&mut self) -> anyhow::Result<Vec<u8>> {
        let s = self.sync_mut()?;
        s.read_memory(VDP_REGS_BASE, 24)
            .await
            .map_err(|e| anyhow::anyhow!("get_vdp_registers failed: {e}"))
    }

    /// `mega_get_palettes` — read all 128 bytes of CRAM via the stub's
    /// `qMdsCram` monitor command (VDP address-set + 64 word-reads from
    /// the data port). The 9-bit BGR-per-entry decode (CRAM word ->
    /// 24-bit RGB) is the caller's job; we surface raw bytes here so the
    /// tool layer can choose its preferred encoding.
    pub async fn get_palettes(&mut self) -> anyhow::Result<Vec<u8>> {
        let s = self.sync_mut()?;
        let cram = s
            .read_cram()
            .await
            .map_err(|e| anyhow::anyhow!("read_cram failed: {e}"))?;
        Ok(cram.to_vec())
    }

    /// `mega_get_sprites` — needs the VDP "Sprite Attribute Table" base
    /// address, which lives in VDP register 5. Those registers are
    /// **write-only** on hardware (no readback path), so we cannot derive
    /// the SAT base from the stub alone.
    ///
    /// TODO M5.8: maintain a host-side VDP register shadow (populated at
    /// `connect()` time from a known SGDK init or from a user-ROM mailbox
    /// at a fixed work-RAM address) and use it here to issue
    /// `read_vram(sat_base, sat_len)`. Until then, this surfaces an
    /// explicit error so the IDE can render a hint instead of silently
    /// returning bogus bytes.
    pub async fn get_sprites(&mut self) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!(
            "{NOT_SUPPORTED}: sprite-attr-table address unknown on hw target until VDP REG_05 shadow is reachable (TODO M5.8)"
        );
    }

    /// `mega_dump_tile` — read 32 bytes (one 4bpp 8x8 tile) from VRAM at
    /// `index * 32`. Routed through the stub's `qMdsVram` monitor command
    /// which performs the VDP address-set + 16 word-reads.
    pub async fn dump_tile(&mut self, index: u32) -> anyhow::Result<Vec<u8>> {
        let s = self.sync_mut()?;
        // VRAM addresses wrap at 64 KiB; allow any 16-bit-aligned tile index.
        let addr = index.wrapping_mul(32);
        s.read_vram(addr, 32)
            .await
            .map_err(|e| anyhow::anyhow!("dump_tile failed: {e}"))
    }

    /// `mega_get_68k_registers` — RSP `g`, decoded as 17/18 big-endian
    /// longs into `EdProM68kRegisters`.
    pub async fn get_68k_registers(&mut self) -> anyhow::Result<EdProM68kRegisters> {
        let s = self.sync_mut()?;
        let raw = s
            .read_registers()
            .await
            .map_err(|e| anyhow::anyhow!("read_registers failed: {e}"))?;
        decode_g_reply_be(&raw)
    }

    /// `mega_get_z80_registers` — Z80 isn't paused by the 68k stub; later
    /// milestone could pause + readback via Z80-bus-request handler.
    pub async fn get_z80_registers(&mut self) -> anyhow::Result<()> {
        anyhow::bail!("{NOT_SUPPORTED}: Z80 not exposed by 68k stub");
    }

    /// `mega_set_breakpoint` — RSP `m`+`M(0x4e41)` patching via `StubSync`.
    pub async fn set_breakpoint(&mut self, addr: u32) -> anyhow::Result<()> {
        let s = self.sync_mut()?;
        s.set_breakpoint(addr)
            .await
            .map_err(|e| anyhow::anyhow!("set_breakpoint failed: {e}"))
    }

    /// `mega_clear_breakpoint` — restore saved opcode + drop table entry.
    pub async fn clear_breakpoint(&mut self, addr: u32) -> anyhow::Result<()> {
        let s = self.sync_mut()?;
        s.clear_breakpoint(addr)
            .await
            .map_err(|e| anyhow::anyhow!("clear_breakpoint failed: {e}"))
    }

    /// `mega_list_breakpoints` — host-side mirror, no wire op.
    pub fn list_breakpoints(&self) -> Vec<u32> {
        self.sync_ref()
            .map(|s| s.list_breakpoints())
            .unwrap_or_default()
    }

    /// `mega_screenshot` — synthesising a frame on hardware requires
    /// reading CRAM (palettes, M5.7 ✓), VRAM tile data (M5.7 ✓), and the
    /// VDP register state (plane A/B base, scroll, sprite-table base,
    /// window split, display mode). VDP regs `$00..$17` are
    /// **write-only** on hardware — there's no MMIO read path. M5.8 will
    /// populate a host-side reg shadow before this can flip on.
    pub async fn screenshot(&mut self) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!(
            "{NOT_SUPPORTED}: screenshot needs VDP reg shadow for plane A/B addrs + scroll regs; deferred to M5.8"
        );
    }

    /// `mega_save_state` — would need full 68k + VRAM dump. Out of scope.
    pub async fn save_state(&mut self, _slot: u32) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// `mega_load_state` — out of scope for hardware.
    pub async fn load_state(&mut self, _slot: u32) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// `mega_get_status` — synchronous (no I/O). Reports liveness +
    /// breakpoint count.
    pub fn get_status(&self) -> serde_json::Value {
        serde_json::json!({
            "target": "edpro",
            "connected": self.connected,
            "port": self.cfg.port.to_string_lossy(),
            "bp_count": self.list_breakpoints().len(),
        })
    }

    // mega_input_* — joypad input on real hardware would require either
    // host-side IRQ injection (cf. `pause`) or a controller-proxy command;
    // out of scope for M5.5.
    pub async fn input_set_state(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }
    pub async fn input_press(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }
    pub async fn input_release(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }
    pub async fn input_get_state(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::edpro::usb::MockUsb;
    use std::sync::{Arc, Mutex};

    fn rep(payload: &[u8], ack: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if ack {
            v.push(b'+');
        }
        v.extend_from_slice(&rsp::encode_packet(payload));
        v
    }

    /// Standard handshake replies: qSupported features + OK to noack.
    fn handshake_replies() -> Vec<Vec<u8>> {
        vec![
            rep(b"PacketSize=400;swbreak+;hwbreak+", true),
            rep(b"OK", true),
        ]
    }

    /// Transport wrapper that mirrors writes into a shared `Arc<Mutex<Vec<u8>>>`
    /// so tests can assert on tx contents after the transport has been
    /// boxed-and-moved into `EdProTarget::sync`.
    struct SpyUsb {
        inner: MockUsb,
        tx_log: Arc<Mutex<Vec<u8>>>,
    }

    #[async_trait]
    impl UsbTransport for SpyUsb {
        async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
            self.inner.read_exact(buf).await
        }
        async fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()> {
            self.tx_log.lock().unwrap().extend_from_slice(buf);
            self.inner.write_all(buf).await
        }
        async fn flush(&mut self) -> anyhow::Result<()> {
            self.inner.flush().await
        }
    }

    /// Build a spy transport pre-loaded with the handshake replies plus
    /// `extra`. Returns the boxed transport + the shared tx-log handle.
    fn make_mock(
        extra: Vec<Vec<u8>>,
    ) -> (Box<dyn UsbTransport + Send>, Arc<Mutex<Vec<u8>>>) {
        let mut all = handshake_replies();
        all.extend(extra);
        let tx_log = Arc::new(Mutex::new(Vec::<u8>::new()));
        let spy = SpyUsb {
            inner: MockUsb::with_replies(all),
            tx_log: tx_log.clone(),
        };
        (Box::new(spy), tx_log)
    }

    fn tx_contains(log: &Arc<Mutex<Vec<u8>>>, needle: &[u8]) -> bool {
        let g = log.lock().unwrap();
        g.windows(needle.len()).any(|w| w == needle)
    }

    // --- connectivity ----------------------------------------------------

    #[test]
    fn new_is_disconnected() {
        let t = EdProTarget::new(EdProConfig::default());
        assert!(!t.connected());
        assert_eq!(t.kind(), TargetKind::EdPro);
        let st = t.get_status();
        assert_eq!(st["target"], "edpro");
        assert_eq!(st["connected"], false);
        assert_eq!(st["bp_count"], 0);
    }

    #[tokio::test]
    async fn connect_real_returns_not_supported() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let e = t.connect().await.unwrap_err().to_string();
        assert!(e.contains(NOT_SUPPORTED), "got: {e}");
    }

    #[tokio::test]
    async fn connect_mock_runs_handshake_and_caches_features() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        assert!(t.connected());
        assert!(t
            .features()
            .iter()
            .any(|(k, v)| k == "swbreak" && v == "+"));
        assert!(t
            .features()
            .iter()
            .any(|(k, _)| k == "PacketSize"));
        assert_eq!(t.get_status()["connected"], true);
    }

    // --- read/write memory ----------------------------------------------

    #[tokio::test]
    async fn read_memory_returns_canned_bytes() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![rep(b"deadbeef", false)]);
        t.connect_mock(m).await.unwrap();
        let bytes = t.read_memory(0x00FF_8000, 4).await.unwrap();
        assert_eq!(bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[tokio::test]
    async fn write_memory_emits_m_packet() {
        // After handshake, no_ack mode is on, so the M reply has no '+'.
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![rep(b"OK", false)]);
        t.connect_mock(m).await.unwrap();
        t.write_memory(0x00FF_0000, &[0xAB, 0xCD]).await.unwrap();
        let needle = rsp::encode_packet(b"Mff0000,2:abcd");
        assert!(tx_contains(&log, &needle), "M packet not found in tx log");
    }

    // --- breakpoints -----------------------------------------------------

    #[tokio::test]
    async fn set_breakpoint_does_read_save_patch_and_lists() {
        let mut t = EdProTarget::new(EdProConfig::default());
        // m -> 0x1234, M -> OK
        let (m, log) = make_mock(vec![rep(b"1234", false), rep(b"OK", false)]);
        t.connect_mock(m).await.unwrap();
        t.set_breakpoint(0x200).await.unwrap();
        assert_eq!(t.list_breakpoints(), vec![0x200]);
        let needle = rsp::encode_packet(b"M200,2:4e41");
        assert!(tx_contains(&log, &needle), "TRAP #1 patch packet not found");
    }

    #[tokio::test]
    async fn clear_breakpoint_restores_saved_word() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![
            rep(b"abcd", false), // m for set
            rep(b"OK", false),   // M for set
            rep(b"OK", false),   // M for clear
        ]);
        t.connect_mock(m).await.unwrap();
        t.set_breakpoint(0x300).await.unwrap();
        t.clear_breakpoint(0x300).await.unwrap();
        assert!(t.list_breakpoints().is_empty());
        let needle = rsp::encode_packet(b"M300,2:abcd");
        assert!(tx_contains(&log, &needle));
    }

    // --- step / continue ------------------------------------------------

    #[tokio::test]
    async fn step_instruction_returns_stop_reply() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![rep(b"S05", false)]);
        t.connect_mock(m).await.unwrap();
        let sr = t.step_instruction().await.unwrap();
        assert_eq!(sr, StopReply::Sig(0x05));
    }

    #[tokio::test]
    async fn resume_does_not_wait_for_stop_reply() {
        // Crucially: only feed handshake replies. If `resume` tried to
        // read a stop reply it would underflow MockUsb and error out.
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        t.resume().await.unwrap();
        let needle = rsp::encode_packet(b"c");
        assert!(tx_contains(&log, &needle), "c packet missing from tx log");
    }

    #[tokio::test]
    async fn continue_run_aliases_resume() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        t.continue_run().await.unwrap();
        let needle = rsp::encode_packet(b"c");
        assert!(tx_contains(&log, &needle));
    }

    // --- registers -------------------------------------------------------

    #[tokio::test]
    async fn get_68k_registers_decodes_big_endian_block() {
        // 17 longs big-endian: D0..D7=0..7, A0..A7=0x10..0x17,
        // PS=0x2700 (SR=0x2700), PC=0x00FF_0000.
        let mut hex = String::new();
        for i in 0..8u32 {
            hex.push_str(&format!("{:08x}", i));
        }
        for i in 0..8u32 {
            hex.push_str(&format!("{:08x}", 0x10 + i));
        }
        hex.push_str("00002700"); // PS
        hex.push_str("00ff0000"); // PC

        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![rep(hex.as_bytes(), false)]);
        t.connect_mock(m).await.unwrap();
        let r = t.get_68k_registers().await.unwrap();
        assert_eq!(r.d, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(r.a, [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);
        assert_eq!(r.sr, 0x2700);
        assert_eq!(r.pc, 0x00FF_0000);
    }

    // --- get_status ------------------------------------------------------

    #[tokio::test]
    async fn get_status_reports_bp_count() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![
            rep(b"1111", false),
            rep(b"OK", false),
            rep(b"2222", false),
            rep(b"OK", false),
            rep(b"OK", false), // clear
        ]);
        t.connect_mock(m).await.unwrap();
        assert_eq!(t.get_status()["bp_count"], 0);
        t.set_breakpoint(0x100).await.unwrap();
        t.set_breakpoint(0x200).await.unwrap();
        assert_eq!(t.get_status()["bp_count"], 2);
        t.clear_breakpoint(0x100).await.unwrap();
        assert_eq!(t.get_status()["bp_count"], 1);
    }

    // --- pause / unsupported tools --------------------------------------

    #[tokio::test]
    async fn pause_returns_hardware_only_unsupported() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        let e = t.pause().await.unwrap_err().to_string();
        assert!(e.contains(NOT_SUPPORTED), "got: {e}");
        assert!(e.contains("IRQ"), "should mention IRQ injection: {e}");
    }

    #[tokio::test]
    async fn permanently_unsupported_tools_return_not_supported() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        // M5.7 wired `get_palettes` + `dump_tile` to qMds* monitor cmds —
        // those are tested in their own dedicated tests below.
        // `get_sprites` + `screenshot` remain NOT_SUPPORTED until M5.8 lands
        // the VDP register shadow.
        for e in [
            t.load_rom(std::path::Path::new("/tmp/x")).await.unwrap_err().to_string(),
            t.unload_rom().await.unwrap_err().to_string(),
            t.step_frame(1).await.unwrap_err().to_string(),
            t.get_sprites().await.unwrap_err().to_string(),
            t.get_z80_registers().await.unwrap_err().to_string(),
            t.screenshot().await.unwrap_err().to_string(),
            t.save_state(0).await.unwrap_err().to_string(),
            t.load_state(0).await.unwrap_err().to_string(),
            t.input_set_state().await.unwrap_err().to_string(),
            t.input_press().await.unwrap_err().to_string(),
            t.input_release().await.unwrap_err().to_string(),
            t.input_get_state().await.unwrap_err().to_string(),
        ] {
            assert!(e.contains(NOT_SUPPORTED), "expected NOT_SUPPORTED, got: {e}");
        }
    }

    // --- M5.7 wired tools -------------------------------------------------

    #[tokio::test]
    async fn get_palettes_returns_128_cram_bytes() {
        let raw: Vec<u8> = (0..128).map(|i| i as u8).collect();
        let hex_pl: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![rep(hex_pl.as_bytes(), false)]);
        t.connect_mock(m).await.unwrap();
        let pal = t.get_palettes().await.unwrap();
        assert_eq!(pal.len(), 128);
        assert_eq!(pal[0], 0);
        assert_eq!(pal[127], 127);
        // qMdsCram packet appears on the wire.
        let needle = rsp::encode_packet(b"qMdsCram");
        assert!(tx_contains(&log, &needle), "qMdsCram missing from tx log");
    }

    #[tokio::test]
    async fn dump_tile_reads_32_bytes_at_index_times_32() {
        let raw: Vec<u8> = (0..32).map(|i| 0xA0 ^ i as u8).collect();
        let hex_pl: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![rep(hex_pl.as_bytes(), false)]);
        t.connect_mock(m).await.unwrap();
        // tile index 0x10 -> VRAM addr 0x200, len 0x20.
        let tile = t.dump_tile(0x10).await.unwrap();
        assert_eq!(tile, raw);
        let needle = rsp::encode_packet(b"qMdsVram:200,20");
        assert!(tx_contains(&log, &needle), "qMdsVram packet missing");
    }

    #[tokio::test]
    async fn get_palettes_propagates_disconnected() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let e = t.get_palettes().await.unwrap_err().to_string();
        assert!(
            e.contains(NOT_SUPPORTED) && e.contains("not connected"),
            "expected not-connected error, got: {e}"
        );
    }

    #[tokio::test]
    async fn dump_tile_propagates_disconnected() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let e = t.dump_tile(0).await.unwrap_err().to_string();
        assert!(
            e.contains(NOT_SUPPORTED) && e.contains("not connected"),
            "expected not-connected error, got: {e}"
        );
    }

    #[tokio::test]
    async fn get_sprites_blocked_on_m58_with_explicit_msg() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        let e = t.get_sprites().await.unwrap_err().to_string();
        assert!(e.contains(NOT_SUPPORTED));
        assert!(e.contains("REG_05") || e.contains("M5.8"), "msg should reference REG_05/M5.8: {e}");
    }

    #[tokio::test]
    async fn screenshot_blocked_on_m58_with_explicit_msg() {
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, _log) = make_mock(vec![]);
        t.connect_mock(m).await.unwrap();
        let e = t.screenshot().await.unwrap_err().to_string();
        assert!(e.contains(NOT_SUPPORTED));
        assert!(e.contains("M5.8"), "msg should reference M5.8: {e}");
    }

    // --- disconnected --------------------------------------------------

    #[tokio::test]
    async fn disconnected_methods_error_with_not_connected() {
        let mut t = EdProTarget::new(EdProConfig::default());
        // Real-impl methods that go through sync_mut() must surface
        // not_connected when sync is None.
        for e in [
            t.read_memory(0, 4).await.unwrap_err().to_string(),
            t.write_memory(0, &[0]).await.unwrap_err().to_string(),
            t.set_breakpoint(0x100).await.unwrap_err().to_string(),
            t.clear_breakpoint(0x100).await.unwrap_err().to_string(),
            t.step_instruction().await.unwrap_err().to_string(),
            t.resume().await.unwrap_err().to_string(),
            t.continue_run().await.unwrap_err().to_string(),
            t.get_68k_registers().await.unwrap_err().to_string(),
            t.get_vdp_registers().await.unwrap_err().to_string(),
            t.get_palettes().await.unwrap_err().to_string(),
            t.dump_tile(0).await.unwrap_err().to_string(),
        ] {
            assert!(
                e.contains(NOT_SUPPORTED) && e.contains("not connected"),
                "expected not_connected, got: {e}"
            );
        }
        // list_breakpoints is sync + non-erroring; just empty.
        assert!(t.list_breakpoints().is_empty());
    }

    // --- connect_mock_no_handshake -------------------------------------

    #[tokio::test]
    async fn connect_mock_no_handshake_skips_features() {
        let mut t = EdProTarget::new(EdProConfig::default());
        t.connect_mock_no_handshake(Box::new(MockUsb::new()));
        assert!(t.connected());
        assert!(t.features().is_empty());
    }

    // --- deploy_stub_then_handshake ------------------------------------

    #[tokio::test]
    async fn deploy_stub_then_handshake_emits_full_attach_sequence() {
        // Mock pre-loads MEM_WR per-chunk ack bytes (one before each
        // 1 KiB chunk in ack-mode) plus the handshake replies. The blob
        // is N bytes -> ceil(N/1024) acks. Plus 1 ack each for the two
        // 4-byte vector patches.
        let blob_len = stub_blob::STUB_BLOB.len();
        let blob_chunks = blob_len.div_ceil(1024);
        let total_acks = blob_chunks + 2;
        let ack_replies: Vec<Vec<u8>> = vec![vec![0u8; total_acks]];

        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = {
            // Build a mock that has acks FIRST (consumed by mem_write) then
            // the handshake replies (consumed by handshake).
            let mut all = ack_replies;
            all.extend(handshake_replies());
            let tx_log = Arc::new(Mutex::new(Vec::<u8>::new()));
            let spy = SpyUsb {
                inner: MockUsb::with_replies(all),
                tx_log: tx_log.clone(),
            };
            (Box::new(spy) as Box<dyn UsbTransport + Send>, tx_log)
        };
        t.deploy_stub_then_handshake(m).await.unwrap();
        assert!(t.connected());

        // Check the wire log for the deployment sequence:
        //   [HOST_RST soft] [MEM_WR @ load_addr ...] [MEM_WR @ $24 ...]
        //   [MEM_WR @ $84 ...] [HOST_RST off] [qSupported] [QStartNoAckMode]
        let host_rst_soft = [0x2B, 0xD4, 0x29, 0xD6, 0x01];
        let host_rst_off = [0x2B, 0xD4, 0x29, 0xD6, 0x00];
        let mem_wr_at_24 = {
            let mut v = vec![0x2B, 0xD4, 0x1A, 0xE5];
            v.extend_from_slice(&stub_blob::VEC_TRACE.0.to_be_bytes());
            v.extend_from_slice(&4u32.to_be_bytes());
            v
        };
        let mem_wr_at_84 = {
            let mut v = vec![0x2B, 0xD4, 0x1A, 0xE5];
            v.extend_from_slice(&stub_blob::VEC_TRAP1.0.to_be_bytes());
            v.extend_from_slice(&4u32.to_be_bytes());
            v
        };
        assert!(tx_contains(&log, &host_rst_soft), "HOST_RST(Soft) not in tx log");
        assert!(tx_contains(&log, &host_rst_off), "HOST_RST(Off) not in tx log");
        assert!(tx_contains(&log, &mem_wr_at_24), "MEM_WR @ $24 not in tx log");
        assert!(tx_contains(&log, &mem_wr_at_84), "MEM_WR @ $84 not in tx log");
        assert!(tx_contains(&log, b"qSupported"), "qSupported missing");
    }

    // --- get_vdp_registers ---------------------------------------------

    #[tokio::test]
    async fn get_vdp_registers_reads_24_bytes_from_c00004() {
        // Build a 24-byte hex blob (48 hex chars).
        let mut hex = String::new();
        for i in 0..24u8 {
            hex.push_str(&format!("{i:02x}"));
        }
        let mut t = EdProTarget::new(EdProConfig::default());
        let (m, log) = make_mock(vec![rep(hex.as_bytes(), false)]);
        t.connect_mock(m).await.unwrap();
        let regs = t.get_vdp_registers().await.unwrap();
        assert_eq!(regs.len(), 24);
        assert_eq!(regs[0], 0);
        assert_eq!(regs[23], 23);
        // And the `m` packet referenced VDP_REGS_BASE.
        let needle = rsp::encode_packet(b"mc00004,18");
        assert!(tx_contains(&log, &needle));
    }
}
