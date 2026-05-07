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

use crate::target::{EdProConfig, TargetKind, NOT_SUPPORTED};

#[allow(dead_code)] // M5.1 will wire these methods up
pub struct EdProTarget {
    cfg: EdProConfig,
    /// Always `false` until M5.1 wires up a real USB session.
    connected: bool,
}

#[allow(dead_code)]
impl EdProTarget {
    pub fn new(cfg: EdProConfig) -> Self {
        Self {
            cfg,
            connected: false,
        }
    }

    /// Try to open a USB session to the configured port. Stubbed.
    #[allow(dead_code)]
    pub fn connect(&mut self) -> anyhow::Result<()> {
        anyhow::bail!(
            "{NOT_SUPPORTED}: edpro USB protocol not implemented (port {:?})",
            self.cfg.port
        );
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
}
