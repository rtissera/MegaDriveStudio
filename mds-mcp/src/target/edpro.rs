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
//!   - KDebug stream via SSF mapper $A130E2 (writes from the running ROM
//!     show up on the USB endpoint as ASCII frames).
//!   - 68k debug stub linked into the user's ROM, exposing register dump,
//!     single-step (T-bit trace exception), and exec breakpoints by
//!     opcode-patching with TRAP. See scripts/gdb-proxy.py for the host
//!     side of an RSP-over-USB framing the stub can serve.
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
