// SPDX-License-Identifier: MIT
//! Mega Everdrive Pro target — stub.
//!
//! The full USB protocol is out of scope for this milestone; what lands here
//! is the surface enumeration so VS Code can `--target edpro` against a
//! plausible MCP server, see `target: "edpro", connected: false` in the
//! status response, and get clean `not_supported_on_target` errors from
//! every other tool.
//!
//! TODO M5.1: implement USB framing.
//!   - EdPro USB FIFO is at $A130D0 (data) / $A130D2 (status) /
//!     $A130D4 (sys status). NOT $A130E2 (that's the SSF mapper bank
//!     register, unrelated to USB). KDebug-over-USB requires explicit
//!     ROM-side writes to $A130D0; SGDK's KDebug_Alert writes $C00004
//!     (Gens KMod $9E00) which is emulator-only and a no-op on hardware.
//!   - 68k debug stub linked into the user's ROM. 68000 has no VBR, so
//!     vectors $24 (Trace) + $84 (TRAP #1) must be patched at link time
//!     to a RAM stub. Hybrid TRAP #1 software BPs + T-bit single-step.
//!     See scripts/gdb-proxy.py for the host-side TCP↔serial passthrough.
//!   - Host protocol (krikzz/mega-ed-pub + ricky26/megalink-rs):
//!     4-byte cmd framing `+ ~+ CMD ~CMD`. Opcodes: status 0x10,
//!     mem_rd 0x19, mem_wr 0x1A, usb_wr 0x22, host_rst 0x29.
//!     ACK-throttled 1024-byte chunks for ROM-area writes.
//!   - Reuse the `Target` enum so the IDE doesn't change.
//!
//! See docs/01-architecture.md and CLAUDE.md "Notes Mega Everdrive Pro".

pub mod framing;
pub mod proto;
pub mod usb;

use crate::target::{EdProConfig, TargetKind, NOT_SUPPORTED};
use usb::UsbTransport;

#[allow(dead_code)] // M5.x will wire these methods up
pub struct EdProTarget {
    cfg: EdProConfig,
    /// Always `false` until a real (or mock) USB session is installed.
    connected: bool,
    /// Active transport, if any. `None` until `connect`/`connect_mock`.
    usb: Option<Box<dyn UsbTransport + Send>>,
}

#[allow(dead_code)]
impl EdProTarget {
    pub fn new(cfg: EdProConfig) -> Self {
        Self {
            cfg,
            connected: false,
            usb: None,
        }
    }

    /// Try to open a USB session to the configured port. Stubbed.
    pub fn connect(&mut self) -> anyhow::Result<()> {
        // TODO M5.x: open serialport-rs/tokio-serial, install transport.
        anyhow::bail!(
            "{NOT_SUPPORTED}: edpro USB protocol not implemented (port {:?})",
            self.cfg.port
        );
    }

    /// Test-only: install a `MockUsb` transport and flip `connected = true`.
    /// Hardware-free path used by unit + integration tests.
    #[cfg(test)]
    pub fn connect_mock(&mut self) {
        self.usb = Some(Box::new(usb::MockUsb::new()));
        self.connected = true;
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

    // --------------------------------------------------------------
    // Tool surface — every method below still returns NOT_SUPPORTED.
    // M5.x will route each one through `self.usb` + `proto::*`.
    // The signatures are placeholders; real callers go through the
    // `Target` trait once it lands. See `tools/mod.rs::block_on_edpro`.
    // --------------------------------------------------------------

    /// TODO M5.x: route via self.usb (proto::mem_write for ROM upload).
    pub fn load_rom(&mut self, _path: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// TODO M5.x: route via self.usb (proto::host_reset).
    pub fn reset(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// TODO M5.x: route via self.usb (proto::mem_read).
    pub fn read_memory(&mut self, _addr: u32, _len: u32) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// TODO M5.x: route via self.usb (proto::mem_write, non-ROM area).
    pub fn write_memory(&mut self, _addr: u32, _data: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }

    /// TODO M5.x: route via self.usb (proto::usb_write — RSP wrapper).
    pub fn rsp_send(&mut self, _payload: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!(NOT_SUPPORTED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_disconnected() {
        let t = EdProTarget::new(EdProConfig::default());
        assert!(!t.connected());
        assert_eq!(t.kind(), TargetKind::EdPro);
        assert_eq!(t.port_str(), "/dev/everdrive");
    }

    #[test]
    fn connect_mock_flips_state() {
        let mut t = EdProTarget::new(EdProConfig::default());
        t.connect_mock();
        assert!(t.connected());
        assert!(t.usb.is_some());
    }

    #[test]
    fn tool_surface_returns_not_supported() {
        let mut t = EdProTarget::new(EdProConfig::default());
        assert!(t.connect().is_err());
        assert!(t.reset().is_err());
        assert!(t.read_memory(0, 4).is_err());
        assert!(t.write_memory(0, &[]).is_err());
        assert!(t.rsp_send(&[]).is_err());
        assert!(t.load_rom(std::path::Path::new("/tmp/x")).is_err());
    }
}
