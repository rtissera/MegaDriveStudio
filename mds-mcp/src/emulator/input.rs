// SPDX-License-Identifier: MIT
//! Joypad input plumbing. Each port owns an `AtomicU16` whose bits map to
//! libretro `RETRO_DEVICE_ID_JOYPAD_*` ids. The MCP layer mutates bits via
//! lock-free atomics; the libra `input_state` callback (registered by the
//! runner) reads bits back for the polled `(port, id)` tuple.
//!
//! Mega-Drive 6-button mapping:
//! ```text
//!   B      = id 0   → MD A
//!   Y      = id 1   → MD X
//!   SELECT = id 2   → MD MODE
//!   START  = id 3   → MD START
//!   UP     = id 4
//!   DOWN   = id 5
//!   LEFT   = id 6
//!   RIGHT  = id 7
//!   A      = id 8   → MD C
//!   X      = id 9   → MD Y
//!   L      = id 10  → MD Z
//!   R      = id 11  → MD ... (unused on MD; we expose it as Z alias)
//! ```
//! Note: the libretro convention used by clownmdemu is `B=A`, `A=C`, `Y=X`,
//! `X=Y`, `L=Z`, `SELECT=MODE`. We honour that here and let
//! `Button::libretro_id` translate.
#![allow(dead_code)]

use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

/// Mega-Drive button enum. Stable for the public tool surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Up,
    Down,
    Left,
    Right,
    A,
    B,
    C,
    Start,
    X,
    Y,
    Z,
    Mode,
}

impl Button {
    /// Stable bit position inside `PortState::buttons`.
    pub fn bit(self) -> u16 {
        match self {
            Button::Up => 1 << 0,
            Button::Down => 1 << 1,
            Button::Left => 1 << 2,
            Button::Right => 1 << 3,
            Button::A => 1 << 4,
            Button::B => 1 << 5,
            Button::C => 1 << 6,
            Button::Start => 1 << 7,
            Button::X => 1 << 8,
            Button::Y => 1 << 9,
            Button::Z => 1 << 10,
            Button::Mode => 1 << 11,
        }
    }

    /// Map the libretro JOYPAD id polled by the core into our internal button.
    /// Returns None for ids outside the 6-pad range we track.
    pub fn from_libretro_id(id: u32) -> Option<Self> {
        match id {
            0 => Some(Button::A),    // RETRO_DEVICE_ID_JOYPAD_B
            1 => Some(Button::X),    // RETRO_DEVICE_ID_JOYPAD_Y
            2 => Some(Button::Mode), // RETRO_DEVICE_ID_JOYPAD_SELECT
            3 => Some(Button::Start),
            4 => Some(Button::Up),
            5 => Some(Button::Down),
            6 => Some(Button::Left),
            7 => Some(Button::Right),
            8 => Some(Button::C),    // RETRO_DEVICE_ID_JOYPAD_A
            9 => Some(Button::Y),    // RETRO_DEVICE_ID_JOYPAD_X
            10 => Some(Button::Z),   // RETRO_DEVICE_ID_JOYPAD_L
            11 => Some(Button::Z),   // RETRO_DEVICE_ID_JOYPAD_R alias
            _ => None,
        }
    }

    /// Parse from the public string surface used by MCP tools.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "up" => Button::Up,
            "down" => Button::Down,
            "left" => Button::Left,
            "right" => Button::Right,
            "a" => Button::A,
            "b" => Button::B,
            "c" => Button::C,
            "start" => Button::Start,
            "x" => Button::X,
            "y" => Button::Y,
            "z" => Button::Z,
            "mode" => Button::Mode,
            _ => return None,
        })
    }

    pub const ALL: [Button; 12] = [
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
        Button::A,
        Button::B,
        Button::C,
        Button::Start,
        Button::X,
        Button::Y,
        Button::Z,
        Button::Mode,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Button::Up => "up",
            Button::Down => "down",
            Button::Left => "left",
            Button::Right => "right",
            Button::A => "a",
            Button::B => "b",
            Button::C => "c",
            Button::Start => "start",
            Button::X => "x",
            Button::Y => "y",
            Button::Z => "z",
            Button::Mode => "mode",
        }
    }
}

#[derive(Debug, Default)]
pub struct PortState {
    pub buttons: AtomicU16,
}

impl PortState {
    pub fn is_pressed(&self, b: Button) -> bool {
        (self.buttons.load(Ordering::Relaxed) & b.bit()) != 0
    }
    pub fn set(&self, b: Button, pressed: bool) {
        if pressed {
            self.buttons.fetch_or(b.bit(), Ordering::Relaxed);
        } else {
            self.buttons.fetch_and(!b.bit(), Ordering::Relaxed);
        }
    }
    pub fn snapshot(&self) -> u16 {
        self.buttons.load(Ordering::Relaxed)
    }
}

/// Two-port joypad state, shared between the MCP thread and the emulator
/// thread (the libra input callback reads it inline). Cheap to clone (Arc).
#[derive(Debug, Default, Clone)]
pub struct InputState {
    inner: Arc<InputInner>,
}

#[derive(Debug, Default)]
struct InputInner {
    ports: [PortState; 2],
}

impl InputState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn port(&self, idx: u32) -> Option<&PortState> {
        self.inner.ports.get(idx as usize)
    }

    /// Apply a partial button-state map to a port. Buttons not listed are
    /// left untouched. Unknown port indices are silently ignored.
    pub fn apply_partial(&self, port: u32, updates: &[(Button, bool)]) {
        if let Some(p) = self.port(port) {
            for (b, pressed) in updates {
                p.set(*b, *pressed);
            }
        }
    }

    pub fn press(&self, port: u32, b: Button) {
        if let Some(p) = self.port(port) {
            p.set(b, true);
        }
    }

    pub fn release(&self, port: u32, b: Button) {
        if let Some(p) = self.port(port) {
            p.set(b, false);
        }
    }

    /// Returns the current button-pressed map for a port as JSON-friendly
    /// `(name, bool)` pairs. Reports all 12 buttons so consumers don't have
    /// to special-case "missing" keys.
    pub fn snapshot_buttons(&self, port: u32) -> Vec<(&'static str, bool)> {
        let Some(p) = self.port(port) else {
            return Vec::new();
        };
        let bits = p.snapshot();
        Button::ALL
            .iter()
            .map(|b| (b.name(), (bits & b.bit()) != 0))
            .collect()
    }

    /// Read a libretro joypad id for the given port. Used by the FFI input
    /// callback installed in the runner.
    pub fn read_libretro(&self, port: u32, id: u32) -> i16 {
        let Some(b) = Button::from_libretro_id(id) else {
            return 0;
        };
        let Some(p) = self.port(port) else {
            return 0;
        };
        if p.is_pressed(b) {
            1
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_release_round_trip() {
        let st = InputState::new();
        st.press(0, Button::Start);
        assert_eq!(st.read_libretro(0, 3), 1);
        st.release(0, Button::Start);
        assert_eq!(st.read_libretro(0, 3), 0);
    }

    #[test]
    fn libretro_id_mapping() {
        // libretro B (0) is MD A; libretro A (8) is MD C
        assert_eq!(Button::from_libretro_id(0), Some(Button::A));
        assert_eq!(Button::from_libretro_id(8), Some(Button::C));
        assert_eq!(Button::from_libretro_id(2), Some(Button::Mode));
    }

    #[test]
    fn partial_apply_does_not_clear_others() {
        let st = InputState::new();
        st.press(0, Button::Up);
        st.press(0, Button::A);
        st.apply_partial(0, &[(Button::Up, false)]);
        assert_eq!(st.read_libretro(0, 4), 0); // up cleared
        assert_eq!(st.read_libretro(0, 0), 1); // A still set (libretro B id)
    }

    #[test]
    fn parse_button_names() {
        assert_eq!(Button::parse("Start"), Some(Button::Start));
        assert_eq!(Button::parse("MODE"), Some(Button::Mode));
        assert_eq!(Button::parse("foo"), None);
    }

    #[test]
    fn unknown_port_ignored() {
        let st = InputState::new();
        st.press(7, Button::A); // out-of-range
        assert_eq!(st.read_libretro(7, 0), 0);
    }
}
