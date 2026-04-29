// SPDX-License-Identifier: MIT
//! Target abstraction (M5 prep).
//!
//! Megadrive Studio talks to one of two backends through the same MCP tool
//! surface:
//!
//! - `Emulator` — in-process libretro core (clownmdemu). Currently the only
//!   fully-implemented target.
//! - `EdPro`    — Mega Everdrive Pro hardware over USB (`/dev/everdrive`).
//!   Stubbed in this milestone — the trait is wired but every tool except
//!   `mega_get_status` returns `not_supported_on_target`. Full USB protocol
//!   lands in M5.1.
//!
//! The `Target` trait is intentionally narrower than the emulator's full
//! command surface: hardware can't take screenshots or save_state, so those
//! return `Ok(None)` defaults that tool handlers translate into
//! `not_supported_on_target` errors.

use std::path::PathBuf;

pub mod edpro;
pub mod emulator;

/// Hardware/emulator target kind. Surfaces in `mega_get_status` and gates
/// emulator-only tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Emulator,
    EdPro,
}

impl TargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Emulator => "emulator",
            Self::EdPro => "edpro",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "emulator" | "emu" => Self::Emulator,
            "edpro" | "ed-pro" | "everdrive" => Self::EdPro,
            _ => return None,
        })
    }
}

/// Reason returned by emulator-only tools when running against a hardware
/// target. The MCP layer wraps this in a structured response so callers can
/// branch on `not_supported_on_target` rather than parsing a free-form string.
pub const NOT_SUPPORTED: &str = "not_supported_on_target";

/// Configuration for the EdPro stub. Surfaced by the CLI and consumed by
/// `mds_main` so the stub knows which serial port to *eventually* open.
#[derive(Debug, Clone)]
pub struct EdProConfig {
    pub port: PathBuf,
    /// USB serial baud rate. Hard-coded for the EdPro Pro firmware (921600
    /// 8N1). Read by the M5.1 transport once the USB protocol lands.
    #[allow(dead_code)]
    pub baud: u32,
}

impl Default for EdProConfig {
    fn default() -> Self {
        Self {
            port: PathBuf::from("/dev/everdrive"),
            baud: 921_600,
        }
    }
}
