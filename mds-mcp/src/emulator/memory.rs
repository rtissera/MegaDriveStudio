// SPDX-License-Identifier: MIT
//! Memory-space taxonomy and (unsafe) read/write helpers around
//! `libra_get_memory_data` / `libra_get_memory_size`.
//!
//! All `unsafe` is isolated in this module. Callers must guarantee that the
//! emulator thread is the sole owner of `libra_ctx_t` and either (a) is
//! provably idle (paused, between frames), or (b) holds the command queue
//! that drives the frame loop.

use serde::Serialize;

use crate::ffi;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemorySpace {
    Ram,
    Vram,
    Cram,
    Vsram,
    Rom,
    Saveram,
    /// VDP register / state blob (clownmdemu fork; LIBRA_MEMORY_VDP_STATE).
    VdpState,
    /// Clown68000 register blob (clownmdemu fork; LIBRA_MEMORY_M68K).
    M68kState,
    /// Z80 register blob (clownmdemu fork; LIBRA_MEMORY_Z80).
    Z80,
}

impl MemorySpace {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "ram" => Self::Ram,
            "vram" => Self::Vram,
            "cram" => Self::Cram,
            "vsram" => Self::Vsram,
            "rom" => Self::Rom,
            "saveram" => Self::Saveram,
            "vdp_state" => Self::VdpState,
            "m68k_state" => Self::M68kState,
            "z80" => Self::Z80,
            _ => return None,
        })
    }

    /// Map a logical space onto the libretro memory ID exposed by the
    /// underlying core. Returns `None` for `Rom`, which is served from the
    /// host-side cached ROM bytes.
    pub fn libretro_id(self) -> Option<u32> {
        Some(match self {
            Self::Ram => ffi::RETRO_MEMORY_SYSTEM_RAM,
            Self::Vram => ffi::RETRO_MEMORY_VIDEO_RAM,
            Self::Saveram => ffi::RETRO_MEMORY_SAVE_RAM,
            Self::Cram => ffi::LIBRA_MEMORY_CRAM,
            Self::Vsram => ffi::LIBRA_MEMORY_VSRAM,
            Self::VdpState => ffi::LIBRA_MEMORY_VDP_STATE,
            Self::M68kState => ffi::LIBRA_MEMORY_M68K,
            Self::Z80 => ffi::LIBRA_MEMORY_Z80,
            Self::Rom => return None,
        })
    }
}

/// Snapshot a libretro memory region into a `Vec<u8>`.
///
/// # Safety
/// `ctx` must be a valid `libra_ctx*` returned from `libra_create` and not
/// currently inside `libra_run` on another thread. Returns `None` when the
/// core has not registered the requested ID.
#[cfg(libra_present)]
pub unsafe fn snapshot_region(ctx: *mut ffi::libra_ctx, id: u32) -> Option<Vec<u8>> {
    let ptr = ffi::libra_get_memory_data(ctx, id);
    if ptr.is_null() {
        return None;
    }
    let size = ffi::libra_get_memory_size(ctx, id);
    if size == 0 {
        return Some(Vec::new());
    }
    let slice = std::slice::from_raw_parts(ptr as *const u8, size);
    Some(slice.to_vec())
}

#[cfg(not(libra_present))]
pub unsafe fn snapshot_region(_ctx: *mut ffi::libra_ctx, _id: u32) -> Option<Vec<u8>> {
    None
}

/// Read a sub-range of a memory region. Returns `Ok(None)` if the region is
/// unmapped, `Err` on out-of-bounds.
#[cfg(libra_present)]
pub unsafe fn read_region(
    ctx: *mut ffi::libra_ctx,
    id: u32,
    addr: u32,
    length: u32,
) -> Result<Option<Vec<u8>>, String> {
    let ptr = ffi::libra_get_memory_data(ctx, id);
    if ptr.is_null() {
        return Ok(None);
    }
    let size = ffi::libra_get_memory_size(ctx, id);
    let start = addr as usize;
    let end = start
        .checked_add(length as usize)
        .ok_or_else(|| "addr+length overflow".to_string())?;
    if end > size {
        return Err(format!(
            "out-of-bounds read 0x{start:08X}..0x{end:08X} (region is {size} bytes)"
        ));
    }
    let slice = std::slice::from_raw_parts(ptr as *const u8, size);
    Ok(Some(slice[start..end].to_vec()))
}

#[cfg(not(libra_present))]
pub unsafe fn read_region(
    _ctx: *mut ffi::libra_ctx,
    _id: u32,
    _addr: u32,
    _length: u32,
) -> Result<Option<Vec<u8>>, String> {
    Ok(None)
}

/// Write a sub-range of a memory region.
#[cfg(libra_present)]
pub unsafe fn write_region(
    ctx: *mut ffi::libra_ctx,
    id: u32,
    addr: u32,
    bytes: &[u8],
) -> Result<(), String> {
    let ptr = ffi::libra_get_memory_data(ctx, id);
    if ptr.is_null() {
        return Err("region unmapped".into());
    }
    let size = ffi::libra_get_memory_size(ctx, id);
    let start = addr as usize;
    let end = start
        .checked_add(bytes.len())
        .ok_or_else(|| "addr+length overflow".to_string())?;
    if end > size {
        return Err(format!(
            "out-of-bounds write 0x{start:08X}..0x{end:08X} (region is {size} bytes)"
        ));
    }
    let dst = std::slice::from_raw_parts_mut(ptr as *mut u8, size);
    dst[start..end].copy_from_slice(bytes);
    Ok(())
}

#[cfg(not(libra_present))]
pub unsafe fn write_region(
    _ctx: *mut ffi::libra_ctx,
    _id: u32,
    _addr: u32,
    _bytes: &[u8],
) -> Result<(), String> {
    Err("libra not linked".into())
}
