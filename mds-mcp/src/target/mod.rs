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
///
/// `port` defaults to `None`; callers must supply an explicit path
/// (`/dev/ttyACM0`, `/dev/cu.usbmodem*`, `COM3`, ...) before
/// [`crate::target::edpro::EdProTarget::connect`] will succeed. We
/// deliberately do **not** guess a default: the EdPro shares its CDC class
/// id with anything else plugged in (Arduinos, modems...) and silently
/// opening the wrong device is hostile.
#[derive(Debug, Clone)]
pub struct EdProConfig {
    pub port: Option<String>,
    /// USB serial baud rate. CDC-class devices (which the EdPro is) ignore
    /// the baud value, but we mirror what `ricky26/megalink-rs` opens with
    /// (9600) for consistency. See `target/edpro/serial.rs` comment.
    #[allow(dead_code)]
    pub baud: u32,
    /// If `port == None` and this is `true`, `connect()` may try the
    /// platform-default path (`/dev/ttyACM0` on Linux). Not implemented
    /// yet: M5.5b ships with port-required semantics only. Reserved for
    /// the IDE auto-attach UX work in M5.6+.
    #[allow(dead_code)]
    pub auto_detect_port: bool,
    /// Path to a debug ELF (built with `make debug` — `-g` keeps `.symtab`).
    /// At `connect()` time the EdPro target parses this file and extracts
    /// the work-RAM addresses of SGDK's VDP-state shadow globals
    /// (`regValues`, `bga_addr`, `bgb_addr`, `slist_addr`, `window_addr`,
    /// `hscroll`, optionally `palette_cache`). Without it,
    /// `mega_get_vdp_registers` and `mega_get_sprites` surface a clear
    /// "ELF symbols not loaded" error — the VDP register file is
    /// write-only on hardware so we can't recover those addresses any
    /// other way.
    pub elf_path: Option<PathBuf>,
}

impl EdProConfig {
    /// Default baud rate matching `ricky26/megalink-rs` so anyone reusing
    /// the same cart over the same cable sees identical wire behaviour.
    pub const DEFAULT_BAUD: u32 = 9600;
}

impl Default for EdProConfig {
    fn default() -> Self {
        Self {
            port: None,
            baud: Self::DEFAULT_BAUD,
            auto_detect_port: false,
            elf_path: None,
        }
    }
}
