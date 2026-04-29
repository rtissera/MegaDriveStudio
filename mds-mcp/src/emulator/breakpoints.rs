// SPDX-License-Identifier: MIT
//! Breakpoint table backing `mega_set_breakpoint` / `mega_clear_breakpoint`
//! / `mega_list_breakpoints`.
//!
//! Concurrency: copy-on-write `RwLock<Arc<BreakpointTable>>`. The MCP layer
//! mutates from async tool handlers; the emulator thread reads from the
//! libretro debug callback (per-instruction for exec, per-bus-access for
//! watchpoints) — readers grab a cheap `Arc` snapshot.

#![allow(dead_code)] // hot-path helpers light up when agent A's debug
                     // callback ships.

use std::sync::Arc;

use parking_lot::RwLock;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BpKind {
    Exec,
    Read,
    Write,
    Access,
}

impl BpKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "exec" | "execute" => Self::Exec,
            "read" => Self::Read,
            "write" => Self::Write,
            "access" | "rw" => Self::Access,
            _ => return None,
        })
    }
    pub fn as_libra_mask(self) -> u32 {
        use crate::ffi::*;
        match self {
            Self::Exec => LIBRA_BP_EXEC,
            Self::Read => LIBRA_BP_READ,
            Self::Write => LIBRA_BP_WRITE,
            Self::Access => LIBRA_BP_ACCESS,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BpSpace {
    Rom,
    Ram,
}

impl BpSpace {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "rom" => Self::Rom,
            "ram" => Self::Ram,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Breakpoint {
    pub id: u32,
    pub addr: u32,
    pub kind: BpKind,
    pub space: BpSpace,
    pub hit_count: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BreakpointTable {
    /// Sorted by id. Linear scans are fine for the typical 0..16 entries.
    pub entries: Vec<Breakpoint>,
    /// Fast-path: sorted addresses of *enabled* exec breakpoints. The bp
    /// callback does a `binary_search` here per instruction.
    pub exec_pcs: Vec<u32>,
}

impl BreakpointTable {
    pub fn add(&mut self, bp: Breakpoint) {
        self.entries.push(bp);
        self.entries.sort_by_key(|b| b.id);
        self.rebuild_fast_paths();
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|b| b.id != id);
        let removed = self.entries.len() != before;
        if removed {
            self.rebuild_fast_paths();
        }
        removed
    }

    fn rebuild_fast_paths(&mut self) {
        self.exec_pcs = self
            .entries
            .iter()
            .filter(|b| b.enabled && matches!(b.kind, BpKind::Exec))
            .map(|b| b.addr)
            .collect();
        self.exec_pcs.sort_unstable();
    }

    /// Hot-path check: did the m68k just step onto a breakpointed PC?
    pub fn matches_pc(&self, pc: u32) -> bool {
        self.exec_pcs.binary_search(&pc).is_ok()
    }

    /// Find the (id, addr) of the first matching breakpoint and bump its hit
    /// count in a *cloned* table (used by the runner inside its swap-update
    /// path). Returns `None` if the PC isn't on any breakpoint.
    pub fn find_exec(&self, pc: u32) -> Option<(u32, u32)> {
        self.entries
            .iter()
            .find(|b| b.enabled && matches!(b.kind, BpKind::Exec) && b.addr == pc)
            .map(|b| (b.id, b.addr))
    }

    pub fn find_watch(&self, addr: u32, write: bool) -> Option<(u32, u32)> {
        self.entries
            .iter()
            .find(|b| {
                if !b.enabled || b.addr != addr {
                    return false;
                }
                match b.kind {
                    BpKind::Read => !write,
                    BpKind::Write => write,
                    BpKind::Access => true,
                    BpKind::Exec => false,
                }
            })
            .map(|b| (b.id, b.addr))
    }
}

/// Shared, copy-on-write handle to the breakpoint table. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct SharedBreakpoints(Arc<RwLock<Arc<BreakpointTable>>>);

impl SharedBreakpoints {
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Arc::new(BreakpointTable::default()))))
    }

    /// Fetch a snapshot Arc — readers (BP callback) hold this for the
    /// duration of one check, then drop it.
    pub fn snapshot(&self) -> Arc<BreakpointTable> {
        self.0.read().clone()
    }

    /// Apply `f` to a clone of the current table and atomically swap it in.
    pub fn update<R, F: FnOnce(&mut BreakpointTable) -> R>(&self, f: F) -> R {
        let mut g = self.0.write();
        let mut new_table = (**g).clone();
        let r = f(&mut new_table);
        *g = Arc::new(new_table);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_round_trip() {
        let s = SharedBreakpoints::new();
        s.update(|t| {
            t.add(Breakpoint {
                id: 1,
                addr: 0x100,
                kind: BpKind::Exec,
                space: BpSpace::Rom,
                hit_count: 0,
                enabled: true,
            });
            t.add(Breakpoint {
                id: 2,
                addr: 0xFF0000,
                kind: BpKind::Write,
                space: BpSpace::Ram,
                hit_count: 0,
                enabled: true,
            });
        });
        let snap = s.snapshot();
        assert_eq!(snap.entries.len(), 2);
        assert!(snap.matches_pc(0x100));
        assert!(!snap.matches_pc(0x101));
        assert!(snap.find_watch(0xFF0000, true).is_some());
        assert!(snap.find_watch(0xFF0000, false).is_none());

        s.update(|t| {
            assert!(t.remove(1));
        });
        let snap = s.snapshot();
        assert_eq!(snap.entries.len(), 1);
        assert!(!snap.matches_pc(0x100));
    }

    #[test]
    fn kind_and_space_parse() {
        assert_eq!(BpKind::parse("EXEC"), Some(BpKind::Exec));
        assert_eq!(BpKind::parse("rw"), Some(BpKind::Access));
        assert_eq!(BpKind::parse("wat"), None);
        assert_eq!(BpSpace::parse("ROM"), Some(BpSpace::Rom));
        assert_eq!(BpSpace::parse("ram"), Some(BpSpace::Ram));
        assert_eq!(BpSpace::parse("vram"), None);
    }
}
