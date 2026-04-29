// SPDX-License-Identifier: MIT
//! Bridge between the libretro debug API (memory ID 0x109 in the clownmdemu
//! fork) and our Rust-side breakpoint table.
//!
//! The trampolines run on the emulator thread inside `libra_run`. They must be
//! lock-free on the hot path: we read the breakpoint table via the COW
//! `SharedBreakpoints::snapshot()` (one Arc clone, no locks).
//!
//! Halt detection: when a callback returns non-zero, the C-side halts the 68k
//! and `libra_run` returns early. We stash the hit metadata in atomics so the
//! runner can detect it after `libra_run` returns and broadcast a Halted event.

#![cfg(libra_present)]

use std::os::raw::{c_int, c_ulong};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::breakpoints::SharedBreakpoints;
use crate::ffi;

/// Heap-allocated context passed to the C callback as `userdata`. Lives as
/// long as the runner's `LibraCtx` (we leak it intentionally; one per core).
pub struct DebugTrampolineCtx {
    pub breakpoints: SharedBreakpoints,
    pub halted: AtomicBool,
    pub halted_id: AtomicU32,
    pub halted_pc: AtomicU32,
    /// 0 = exec, 1 = watchpoint
    pub halted_kind: AtomicU32,
}

impl DebugTrampolineCtx {
    pub fn new(breakpoints: SharedBreakpoints) -> Box<Self> {
        Box::new(Self {
            breakpoints,
            halted: AtomicBool::new(false),
            halted_id: AtomicU32::new(0),
            halted_pc: AtomicU32::new(0),
            halted_kind: AtomicU32::new(0),
        })
    }

    pub fn take_halt(&self) -> Option<HaltInfo> {
        if self.halted.swap(false, Ordering::AcqRel) {
            Some(HaltInfo {
                id: self.halted_id.load(Ordering::Acquire),
                pc: self.halted_pc.load(Ordering::Acquire),
                kind: self.halted_kind.load(Ordering::Acquire),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HaltInfo {
    pub id: u32,
    pub pc: u32,
    pub kind: u32,
}

/// Trampoline invoked by clownmdemu before each 68k instruction. Returns
/// non-zero to halt the dispatch loop.
pub unsafe extern "C" fn bp_trampoline(
    userdata: *mut std::ffi::c_void,
    pc: c_ulong,
) -> c_int {
    if userdata.is_null() {
        return 0;
    }
    let ctx = unsafe { &*(userdata as *const DebugTrampolineCtx) };
    let pc32 = pc as u32;
    let snap = ctx.breakpoints.snapshot();
    if snap.matches_pc(pc32) {
        if let Some((id, _addr)) = snap.find_exec(pc32) {
            ctx.halted_id.store(id, Ordering::Release);
            ctx.halted_pc.store(pc32, Ordering::Release);
            ctx.halted_kind.store(0, Ordering::Release);
            ctx.halted.store(true, Ordering::Release);
            return 1;
        }
    }
    0
}

/// Watchpoint trampoline. Fires on every 68k bus read/write.
pub unsafe extern "C" fn wp_trampoline(
    userdata: *mut std::ffi::c_void,
    addr: c_ulong,
    _size: std::os::raw::c_uchar,
    is_write: c_int,
    _value: c_ulong,
) -> c_int {
    if userdata.is_null() {
        return 0;
    }
    let ctx = unsafe { &*(userdata as *const DebugTrampolineCtx) };
    let addr32 = addr as u32;
    let snap = ctx.breakpoints.snapshot();
    if let Some((id, _)) = snap.find_watch(addr32, is_write != 0) {
        ctx.halted_id.store(id, Ordering::Release);
        ctx.halted_pc.store(addr32, Ordering::Release);
        ctx.halted_kind.store(1, Ordering::Release);
        ctx.halted.store(true, Ordering::Release);
        return 1;
    }
    0
}

/// Probe the loaded core for `LIBRA_MEMORY_DEBUG_API` and install both
/// callbacks. Returns the api pointer + the leaked trampoline ctx pointer for
/// the runner to keep around (so it can call request_halt / clear_halt later
/// and inspect halt state). Returns `None` if the core does not advertise the
/// API (older fork).
///
/// # Safety
/// `ctx` must be a valid libra context with a loaded core.
pub unsafe fn install(
    ctx: *mut ffi::libra_ctx,
    bps: SharedBreakpoints,
) -> Option<(*const LibraMdDebugApi, *mut DebugTrampolineCtx)> {
    let ptr = unsafe { ffi::libra_get_memory_data(ctx, ffi::LIBRA_MEMORY_DEBUG_API) };
    if ptr.is_null() {
        return None;
    }
    let size = unsafe { ffi::libra_get_memory_size(ctx, ffi::LIBRA_MEMORY_DEBUG_API) };
    if size < std::mem::size_of::<LibraMdDebugApi>() {
        return None;
    }
    let api = ptr as *const LibraMdDebugApi;
    let api_ref = unsafe { &*api };
    let trampoline_ctx = Box::leak(DebugTrampolineCtx::new(bps)) as *mut DebugTrampolineCtx;
    let ud = trampoline_ctx as *mut std::ffi::c_void;
    if let Some(set_bp) = api_ref.set_breakpoint_callback {
        unsafe { set_bp(Some(bp_trampoline), ud) };
    }
    if let Some(set_wp) = api_ref.set_watchpoint_callback {
        unsafe { set_wp(Some(wp_trampoline), ud) };
    }
    Some((api, trampoline_ctx))
}

pub use ffi::LibraMdDebugApi;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emulator::breakpoints::{Breakpoint, BpKind, BpSpace};

    #[test]
    fn trampoline_lookup_lock_free() {
        let bps = SharedBreakpoints::new();
        bps.update(|t| {
            t.add(Breakpoint {
                id: 7,
                addr: 0x1234,
                kind: BpKind::Exec,
                space: BpSpace::Rom,
                hit_count: 0,
                enabled: true,
            });
        });
        let ctx = Box::leak(DebugTrampolineCtx::new(bps));
        let ud = (ctx as *mut DebugTrampolineCtx) as *mut std::ffi::c_void;

        // Miss
        let r = unsafe { bp_trampoline(ud, 0x1000) };
        assert_eq!(r, 0);
        assert!(ctx.take_halt().is_none());

        // Hit
        let r = unsafe { bp_trampoline(ud, 0x1234) };
        assert_eq!(r, 1);
        let info = ctx.take_halt().expect("halt info");
        assert_eq!(info.id, 7);
        assert_eq!(info.pc, 0x1234);
        assert_eq!(info.kind, 0);

        // Reclaim
        unsafe { drop(Box::from_raw(ctx as *mut DebugTrampolineCtx)) };
    }
}
