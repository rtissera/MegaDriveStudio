// SPDX-License-Identifier: MIT
//! Emulator-thread frame loop. Drains the command queue between frames,
//! calls `libra_run` (≤16.7 ms wall-clock target), CRC-diffs libretro
//! memory regions, and fans changes out as `ResourceEvent`s. While paused,
//! blocks on `cmd_rx.blocking_recv()`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use super::breakpoints::{Breakpoint, SharedBreakpoints};
#[cfg(libra_present)]
use super::debug_api::{self, DebugTrampolineCtx};
use super::decode;
use super::frame::{self, FrameSlot};
#[cfg(libra_present)]
use super::frame::PixelFormat;
use super::memory;
use super::{Command, MemorySpace, ResourceEvent, RomInfo, SetBpOutcome, Status, StepOutcome};
use crate::ffi;

const FRAME_DURATION: Duration = Duration::from_micros(16_667);

#[cfg(libra_present)]
struct LibraCtx {
    raw: *mut ffi::libra_ctx,
    core_loaded: bool,
    /// Heap-allocated frame slot whose pointer is handed to libra as `userdata`
    /// and dereferenced from the C video callback. Held in a `Box` so the
    /// pointer is stable for libra's lifetime.
    frame_slot: Box<FrameSlot>,
    /// Debug API function-pointer table (memory ID 0x109). Installed lazily
    /// after `libra_load_game` once the core actually exposes it.
    debug_api: Option<*const ffi::LibraMdDebugApi>,
    /// Heap-allocated trampoline context, leaked so it lives as long as the
    /// core. Pointer handed to clownmdemu as `userdata` for both BP/WP
    /// callbacks. Owned (we free it on `Drop`).
    debug_ctx: Option<*mut DebugTrampolineCtx>,
}

#[cfg(libra_present)]
unsafe extern "C" fn video_cb_trampoline(
    ud: *mut std::ffi::c_void,
    data: *const std::ffi::c_void,
    w: std::ffi::c_uint,
    h: std::ffi::c_uint,
    pitch: usize,
    pixel_format: std::ffi::c_int,
) {
    if ud.is_null() || data.is_null() || w == 0 || h == 0 || pixel_format < 0 {
        return; // hardware-rendered frames signal pixel_format == -1.
    }
    let Some(fmt) = PixelFormat::from_libretro(pixel_format) else {
        return;
    };
    let slot: &FrameSlot = unsafe { &*(ud as *const FrameSlot) };
    let total = pitch.saturating_mul(h as usize);
    if total == 0 {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, total) };
    slot.store(w, h, pitch, fmt, bytes, 0);
}

#[cfg(libra_present)]
impl LibraCtx {
    fn new() -> Option<Self> {
        // NULL audio specifically dodges the libra audio-resampler
        // heap-corruption bug while the parallel agent finishes the patch.
        let mut cfg: ffi::libra_config_t = unsafe { std::mem::zeroed() };
        cfg.audio_output_rate = 48_000;
        let frame_slot = Box::new(FrameSlot::new());
        cfg.video = Some(video_cb_trampoline);
        cfg.userdata = (&*frame_slot as *const FrameSlot) as *mut std::ffi::c_void;
        let raw = unsafe { ffi::libra_create(&cfg as *const _) };
        if raw.is_null() {
            return None;
        }
        Some(Self {
            raw,
            core_loaded: false,
            frame_slot,
            debug_api: None,
            debug_ctx: None,
        })
    }
    fn frame_slot(&self) -> &FrameSlot {
        &self.frame_slot
    }

    fn load_core(&mut self, path: &str) -> bool {
        let c = match std::ffi::CString::new(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let ok = unsafe { ffi::libra_load_core(self.raw, c.as_ptr()) };
        self.core_loaded = ok;
        ok
    }

    fn load_game(&mut self, path: &str) -> bool {
        let c = match std::ffi::CString::new(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        unsafe { ffi::libra_load_game(self.raw, c.as_ptr()) }
    }

    fn run_frame(&self) {
        unsafe { ffi::libra_run(self.raw) }
    }

    /// Probe + install the M4 debug API. Returns true on success; idempotent.
    fn install_debug_api(&mut self, bps: &SharedBreakpoints) -> bool {
        if self.debug_api.is_some() {
            return true;
        }
        let installed = unsafe { debug_api::install(self.raw, bps.clone()) };
        if let Some((api, ctx)) = installed {
            self.debug_api = Some(api);
            self.debug_ctx = Some(ctx);
            true
        } else {
            false
        }
    }

    fn debug_request_halt(&self) {
        if let Some(api) = self.debug_api {
            if let Some(f) = unsafe { (*api).request_halt } {
                unsafe { f() };
            }
        }
    }

    fn debug_clear_halt(&self) {
        if let Some(api) = self.debug_api {
            if let Some(f) = unsafe { (*api).clear_halt_request } {
                unsafe { f() };
            }
        }
    }

    fn take_halt(&self) -> Option<debug_api::HaltInfo> {
        let ctx = self.debug_ctx?;
        unsafe { (*ctx).take_halt() }
    }
}

#[cfg(libra_present)]
impl Drop for LibraCtx {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // Detach our callbacks before tearing down the core so the C side
            // does not call back into freed Rust memory.
            if let Some(api) = self.debug_api.take() {
                unsafe {
                    if let Some(f) = (*api).set_breakpoint_callback {
                        f(None, std::ptr::null_mut());
                    }
                    if let Some(f) = (*api).set_watchpoint_callback {
                        f(None, std::ptr::null_mut());
                    }
                }
            }
            if let Some(ctx) = self.debug_ctx.take() {
                drop(unsafe { Box::from_raw(ctx) });
            }
            unsafe {
                ffi::libra_unload_game(self.raw);
                if self.core_loaded {
                    ffi::libra_unload_core(self.raw);
                }
                ffi::libra_destroy(self.raw);
            }
        }
    }
}

#[cfg(not(libra_present))]
struct LibraCtx {
    raw: *mut ffi::libra_ctx,
    core_loaded: bool,
    frame_slot: Box<FrameSlot>,
}

#[cfg(not(libra_present))]
impl LibraCtx {
    fn new() -> Option<Self> {
        Some(Self {
            raw: std::ptr::null_mut(),
            core_loaded: false,
            frame_slot: Box::new(FrameSlot::new()),
        })
    }
    fn load_core(&mut self, _: &str) -> bool {
        false
    }
    fn load_game(&mut self, _: &str) -> bool {
        false
    }
    fn run_frame(&self) {}
    fn frame_slot(&self) -> &FrameSlot {
        &self.frame_slot
    }
    fn install_debug_api(&mut self, _bps: &SharedBreakpoints) -> bool {
        false
    }
    fn debug_request_halt(&self) {}
    fn debug_clear_halt(&self) {}
    fn take_halt(&self) -> Option<NoLibraHalt> {
        None
    }
}

#[cfg(not(libra_present))]
#[allow(dead_code)]
struct NoLibraHalt {
    pub id: u32,
    pub pc: u32,
    pub kind: u32,
}

unsafe impl Send for LibraCtx {}

struct State {
    libra: LibraCtx,
    core_path: PathBuf,
    rom_path: Option<PathBuf>,
    rom_bytes: Option<Vec<u8>>,
    paused: bool,
    frame: u64,
    fps_avg: f64,
    last_fps_window: Instant,
    frames_in_window: u32,
    region_crcs: HashMap<&'static str, u32>,
    saved_states: HashMap<u32, Vec<u8>>,
    breakpoints: SharedBreakpoints,
    next_bp_id: u32,
    /// Set when the BP callback (or simulator) hit a breakpoint.
    halted_on_bp: Option<u32>,
    /// True when agent A's debug API is wired in. Until then we serve the
    /// graceful-degradation responses from set_breakpoint / step_instruction.
    debug_api_available: bool,
}

impl State {
    fn new(core_path: PathBuf, breakpoints: SharedBreakpoints) -> Self {
        Self {
            libra: LibraCtx::new().expect("libra_create returned null"),
            core_path,
            rom_path: None,
            rom_bytes: None,
            paused: true,
            frame: 0,
            fps_avg: 0.0,
            last_fps_window: Instant::now(),
            frames_in_window: 0,
            region_crcs: HashMap::new(),
            saved_states: HashMap::new(),
            breakpoints,
            next_bp_id: 1,
            halted_on_bp: None,
            // Probed at load_core time once agent A's env extension lands.
            debug_api_available: false,
        }
    }
}

pub fn run(
    core_path: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    bcast: broadcast::Sender<ResourceEvent>,
    breakpoints: SharedBreakpoints,
) {
    let mut state = State::new(core_path, breakpoints);
    let mut next_frame = Instant::now() + FRAME_DURATION;

    loop {
        // Drain pending commands without blocking.
        loop {
            match cmd_rx.try_recv() {
                Ok(Command::Shutdown) => return,
                Ok(cmd) => handle_command(&mut state, cmd, &bcast),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return,
            }
        }

        // If paused or no ROM loaded, block on the next command.
        if state.paused || state.rom_bytes.is_none() {
            match cmd_rx.blocking_recv() {
                Some(Command::Shutdown) | None => return,
                Some(cmd) => handle_command(&mut state, cmd, &bcast),
            }
            continue;
        }

        // Run a frame.
        state.libra.run_frame();
        state.frame = state.frame.saturating_add(1);
        state.frames_in_window += 1;
        // If a BP/WP fired during this frame, halt the actor and notify.
        check_halt(&mut state, &bcast);
        let elapsed = state.last_fps_window.elapsed();
        if elapsed >= Duration::from_secs(1) {
            state.fps_avg =
                state.frames_in_window as f64 / elapsed.as_secs_f64();
            state.frames_in_window = 0;
            state.last_fps_window = Instant::now();
        }

        publish_changes(&mut state, &bcast);

        let now = Instant::now();
        if next_frame <= now {
            next_frame = now + FRAME_DURATION;
        } else {
            std::thread::sleep(next_frame - now);
            next_frame += FRAME_DURATION;
        }
    }
}

fn handle_command(
    state: &mut State,
    cmd: Command,
    bcast: &broadcast::Sender<ResourceEvent>,
) {
    match cmd {
        Command::Shutdown => {}
        Command::LoadRom { path, reply } => {
            let r = (|| {
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
                if bytes.len() < 0x200 {
                    anyhow::bail!("ROM too small ({} bytes)", bytes.len());
                }
                let header_str = std::str::from_utf8(&bytes[0x100..0x110]).unwrap_or("");
                if !header_str.starts_with("SEGA") {
                    anyhow::bail!("missing SEGA magic at 0x100 (got {header_str:?})");
                }
                if !state.libra.core_loaded {
                    let core = state.core_path.to_string_lossy().to_string();
                    if !state.libra.load_core(&core) {
                        tracing::warn!(core = %core, "libra_load_core failed (core unavailable; continuing without live emulation)");
                    }
                }
                if state.libra.core_loaded {
                    let p = path.to_string_lossy().to_string();
                    if !state.libra.load_game(&p) {
                        tracing::warn!(rom = %p, "libra_load_game failed");
                    } else if state.libra.install_debug_api(&state.breakpoints) {
                        state.debug_api_available = true;
                        tracing::info!("libretro debug API (mem id 0x109) installed");
                    }
                }
                let info = RomInfo {
                    size: bytes.len() as u64,
                    crc32: super::crc32_ieee(&bytes),
                    header_name: super::header_name(&bytes),
                    region: super::region_from_header(&bytes),
                };
                state.rom_path = Some(path);
                state.rom_bytes = Some(bytes);
                state.frame = 0;
                state.paused = true; // start paused; client must Resume.
                state.region_crcs.clear();
                Ok::<_, anyhow::Error>(info)
            })();
            let _ = reply.send(r);
        }
        Command::UnloadRom { reply } => {
            state.rom_path = None;
            state.rom_bytes = None;
            state.paused = true;
            state.frame = 0;
            state.region_crcs.clear();
            let _ = reply.send(Ok(()));
        }
        Command::Pause { reply } => {
            state.paused = true;
            let _ = reply.send(Ok(state.frame));
        }
        Command::Resume { reply } => {
            state.paused = false;
            let _ = reply.send(Ok(state.frame));
        }
        Command::StepFrame { n, reply } => {
            if state.rom_bytes.is_none() {
                let _ = reply.send(Err(anyhow::anyhow!("no ROM loaded")));
                return;
            }
            for _ in 0..n {
                state.libra.run_frame();
                state.frame = state.frame.saturating_add(1);
                check_halt(state, bcast);
                if state.halted_on_bp.is_some() {
                    break;
                }
            }
            publish_changes(state, bcast);
            state.paused = true;
            let _ = reply.send(Ok(state.frame));
        }
        Command::ReadMemory {
            space,
            addr,
            length,
            reply,
        } => {
            let r = read_memory(state, space, addr, length);
            let _ = reply.send(r);
        }
        Command::WriteMemory {
            space,
            addr,
            bytes,
            reply,
        } => {
            let r = write_memory(state, space, addr, &bytes);
            let _ = reply.send(r);
        }
        Command::SnapshotRegion { space, reply } => {
            let r = snapshot(state, space);
            let _ = reply.send(r);
        }
        Command::Status { reply } => {
            let _ = reply.send(Ok(Status {
                rom_loaded: state.rom_bytes.is_some(),
                paused: state.paused,
                frame: state.frame,
                fps_avg: state.fps_avg,
                target: "emulator",
                libra_linked: cfg!(libra_present),
            }));
        }
        Command::SaveState { slot, reply } => {
            // Best-effort: store a snapshot of all known regions concatenated.
            // M3 will swap to libra_serialize once we wire it up.
            let r = save_state(state, slot);
            let _ = reply.send(r);
        }
        Command::LoadState { slot, reply } => {
            let r = load_state(state, slot);
            let _ = reply.send(r);
        }
        Command::Screenshot { reply } => {
            let _ = reply.send(Ok(state.libra.frame_slot().snapshot()));
        }
        Command::StepInstruction { n, reply } => {
            let r = step_instruction(state, n, bcast);
            let _ = reply.send(r);
        }
        Command::SetBreakpoint {
            addr,
            kind,
            space,
            reply,
        } => {
            let id = state.next_bp_id;
            state.next_bp_id = state.next_bp_id.wrapping_add(1);
            state.breakpoints.update(|t| {
                t.add(Breakpoint {
                    id,
                    addr,
                    kind,
                    space,
                    hit_count: 0,
                    enabled: true,
                });
            });
            publish_breakpoints(state, bcast);
            let _ = reply.send(Ok(SetBpOutcome {
                ok: true,
                id,
                // The BP is still recorded in the table; activates once the
                // patched core exposes the debug callback API.
                reason: if state.debug_api_available {
                    None
                } else {
                    Some("debug_api_unavailable")
                },
            }));
        }
        Command::ClearBreakpoint { id, reply } => {
            let removed = state.breakpoints.update(|t| t.remove(id));
            if removed {
                publish_breakpoints(state, bcast);
            }
            let _ = reply.send(Ok(removed));
        }
        Command::ListBreakpoints { reply } => {
            let snap = state.breakpoints.snapshot();
            let _ = reply.send(Ok(snap.entries.clone()));
        }
        Command::ContinueAfterHalt { reply } => {
            state.halted_on_bp = None;
            state.libra.debug_clear_halt();
            state.paused = false;
            let _ = reply.send(Ok(state.frame));
        }
        Command::SimulateBreakpointHit { pc, reply } => {
            let snap = state.breakpoints.snapshot();
            let hit = snap.find_exec(pc);
            if let Some((id, _)) = hit {
                state.breakpoints.update(|t| {
                    if let Some(b) = t.entries.iter_mut().find(|b| b.id == id) {
                        b.hit_count += 1;
                    }
                });
                state.halted_on_bp = Some(id);
                state.paused = true;
                publish_breakpoints(state, bcast);
                let _ = reply.send(Ok(Some(id)));
            } else {
                let _ = reply.send(Ok(None));
            }
        }
    }
}

fn step_instruction(
    state: &mut State,
    n: u32,
    bcast: &broadcast::Sender<ResourceEvent>,
) -> anyhow::Result<StepOutcome> {
    if state.rom_bytes.is_none() {
        anyhow::bail!("no ROM loaded");
    }
    let n = n.clamp(1, 1_000_000);
    if state.debug_api_available {
        // Real instruction-granular step: ask the core to halt after one
        // instruction, run one frame's worth of cycles (which the core will
        // exit early once the halt request is honoured), clear the halt.
        for _ in 0..n {
            state.libra.debug_request_halt();
            state.libra.run_frame();
            state.libra.debug_clear_halt();
            // A breakpoint may have fired during the same step.
            if state.libra.take_halt().is_some() {
                state.halted_on_bp = Some(0);
                break;
            }
        }
    } else {
        // Fallback: frame granularity until the patched core lands.
        for _ in 0..n {
            state.libra.run_frame();
            state.frame = state.frame.saturating_add(1);
        }
    }
    publish_changes(state, bcast);
    state.paused = true;

    // Decode current PC/SR for the response so the IDE can refresh its CPU view.
    let m68k_blob = unsafe {
        memory::snapshot_region(state.libra.raw, ffi::LIBRA_MEMORY_M68K)
    }
    .unwrap_or_default();
    let regs = decode::decode_m68k(&m68k_blob).unwrap_or_default();
    Ok(StepOutcome {
        pc: regs.pc,
        sr: regs.sr,
        frame: state.frame,
        instructions_executed: if state.debug_api_available { n } else { 0 },
        granularity: if state.debug_api_available {
            "instruction"
        } else {
            "frame"
        },
    })
}

/// After `libra_run` returns, check whether the BP/WP trampoline tripped.
/// If so, halt the actor and emit a halt event on the breakpoint resource.
fn check_halt(state: &mut State, bcast: &broadcast::Sender<ResourceEvent>) {
    if !state.debug_api_available {
        return;
    }
    let Some(info) = state.libra.take_halt() else { return };
    state.libra.debug_clear_halt();
    // Bump the hit count on the matching BP.
    state.breakpoints.update(|t| {
        if let Some(b) = t.entries.iter_mut().find(|b| b.id == info.id) {
            b.hit_count += 1;
        }
    });
    state.halted_on_bp = Some(info.id);
    state.paused = true;
    publish_breakpoints(state, bcast);
    // Refresh M68k regs so the IDE's CPU view auto-updates.
    publish_decoded(state, bcast, MemorySpace::M68kState, "mega://m68k/registers", |b| {
        decode::decode_m68k(b).and_then(|r| serde_json::to_vec(&r).ok())
    });
    if bcast.receiver_count() > 0 {
        let payload = serde_json::json!({
            "kind": "halted",
            "reason": if info.kind == 0 { "breakpoint" } else { "watchpoint" },
            "bp_id": info.id,
            "addr": info.pc,
            "frame": state.frame,
        });
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            let _ = bcast.send(ResourceEvent {
                uri: "mega://halts",
                mime: "application/json",
                payload: Arc::new(bytes),
            });
        }
    }
}

fn publish_breakpoints(state: &mut State, bcast: &broadcast::Sender<ResourceEvent>) {
    if bcast.receiver_count() == 0 {
        return;
    }
    let snap = state.breakpoints.snapshot();
    if let Ok(payload) = serde_json::to_vec(&snap.entries) {
        let _ = bcast.send(ResourceEvent {
            uri: "mega://breakpoints",
            mime: "application/json",
            payload: Arc::new(payload),
        });
    }
}

fn read_memory(
    state: &State,
    space: MemorySpace,
    addr: u32,
    length: u32,
) -> anyhow::Result<Vec<u8>> {
    if length == 0 {
        return Ok(Vec::new());
    }
    if length > 16 * 1024 * 1024 {
        anyhow::bail!("length {} exceeds sanity cap (16 MiB)", length);
    }
    if matches!(space, MemorySpace::Rom) {
        let rom = state
            .rom_bytes
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no ROM loaded"))?;
        let start = addr as usize;
        let end = start
            .checked_add(length as usize)
            .ok_or_else(|| anyhow::anyhow!("addr+length overflow"))?;
        if end > rom.len() {
            anyhow::bail!(
                "out-of-bounds ROM read 0x{start:08X}..0x{end:08X} (rom is {} bytes)",
                rom.len()
            );
        }
        return Ok(rom[start..end].to_vec());
    }
    let id = space
        .libretro_id()
        .ok_or_else(|| anyhow::anyhow!("no libretro id for space"))?;
    let bytes = unsafe { memory::read_region(state.libra.raw, id, addr, length) }
        .map_err(anyhow::Error::msg)?;
    bytes.ok_or_else(|| anyhow::anyhow!("region {space:?} unmapped (core not loaded?)"))
}

fn write_memory(
    state: &State,
    space: MemorySpace,
    addr: u32,
    bytes: &[u8],
) -> anyhow::Result<()> {
    if matches!(space, MemorySpace::Rom) {
        anyhow::bail!("ROM is read-only");
    }
    let id = space
        .libretro_id()
        .ok_or_else(|| anyhow::anyhow!("no libretro id for space"))?;
    unsafe { memory::write_region(state.libra.raw, id, addr, bytes) }
        .map_err(anyhow::Error::msg)
}

fn snapshot(state: &State, space: MemorySpace) -> anyhow::Result<Vec<u8>> {
    if matches!(space, MemorySpace::Rom) {
        return Ok(state.rom_bytes.clone().unwrap_or_default());
    }
    let id = space
        .libretro_id()
        .ok_or_else(|| anyhow::anyhow!("no libretro id for space"))?;
    Ok(unsafe { memory::snapshot_region(state.libra.raw, id) }.unwrap_or_default())
}

/// Region snapshots watched for change notifications.
const WATCHED_REGIONS: &[(&str, &str, MemorySpace)] = &[
    ("mega://vram", "application/octet-stream", MemorySpace::Vram),
    ("mega://cram", "application/octet-stream", MemorySpace::Cram),
    ("mega://vsram", "application/octet-stream", MemorySpace::Vsram),
];

fn publish_changes(state: &mut State, bcast: &broadcast::Sender<ResourceEvent>) {
    if bcast.receiver_count() == 0 {
        return;
    }
    for (uri, mime, space) in WATCHED_REGIONS {
        let Some(id) = space.libretro_id() else {
            continue;
        };
        let snap = match unsafe { memory::snapshot_region(state.libra.raw, id) } {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let new_crc = crc32fast::hash(&snap);
        let prev = state.region_crcs.get(uri).copied();
        if prev == Some(new_crc) {
            continue;
        }
        state.region_crcs.insert(uri, new_crc);
        let _ = bcast.send(ResourceEvent {
            uri,
            mime,
            payload: Arc::new(snap),
        });
    }

    // Framebuffer — PNG (CRC the raw pixel buffer to dedup).
    if let Some(fb) = state.libra.frame_slot().snapshot() {
        let crc = crc32fast::hash(&fb.data);
        if state.region_crcs.get("mega://framebuffer").copied() != Some(crc) {
            state.region_crcs.insert("mega://framebuffer", crc);
            let rgba = frame::to_rgba8(&fb);
            if let Ok(png) = frame::rgba8_to_png(&rgba, fb.w, fb.h) {
                let _ = bcast.send(ResourceEvent {
                    uri: "mega://framebuffer",
                    mime: "image/png",
                    payload: Arc::new(png),
                });
            }
        }
    }

    // Decoded JSON resources: VDP regs, 68k regs, Z80 regs.
    publish_decoded(state, bcast, MemorySpace::VdpState, "mega://vdp/registers", |b| {
        serde_json::to_vec(&decode::decode_vdp_registers(b)).ok()
    });
    publish_decoded(state, bcast, MemorySpace::M68kState, "mega://m68k/registers", |b| {
        decode::decode_m68k(b).and_then(|r| serde_json::to_vec(&r).ok())
    });
    if let Some(id) = MemorySpace::Z80.libretro_id() {
        let blob = unsafe { memory::snapshot_region(state.libra.raw, id) }.unwrap_or_default();
        if !blob.is_empty() {
            let crc = crc32fast::hash(&blob);
            if state.region_crcs.get("mega://z80/registers").copied() != Some(crc) {
                state.region_crcs.insert("mega://z80/registers", crc);
                let bus_blob = MemorySpace::Z80Bus
                    .libretro_id()
                    .and_then(|id| unsafe { memory::snapshot_region(state.libra.raw, id) })
                    .unwrap_or_default();
                if let Some(regs) = decode::decode_z80(&blob, &bus_blob) {
                    if let Ok(s) = serde_json::to_vec(&regs) {
                        let _ = bcast.send(ResourceEvent {
                            uri: "mega://z80/registers",
                            mime: "application/json",
                            payload: Arc::new(s),
                        });
                    }
                }
            }
        }
    }
}

fn publish_decoded(
    state: &mut State,
    bcast: &broadcast::Sender<ResourceEvent>,
    space: MemorySpace,
    uri: &'static str,
    decode: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
) {
    let Some(id) = space.libretro_id() else { return };
    let Some(blob) = (unsafe { memory::snapshot_region(state.libra.raw, id) }) else { return };
    if blob.is_empty() {
        return;
    }
    let crc = crc32fast::hash(&blob);
    if state.region_crcs.get(uri).copied() == Some(crc) {
        return;
    }
    state.region_crcs.insert(uri, crc);
    if let Some(payload) = decode(&blob) {
        let _ = bcast.send(ResourceEvent {
            uri,
            mime: "application/json",
            payload: Arc::new(payload),
        });
    }
}

fn save_state(state: &mut State, slot: u32) -> anyhow::Result<u64> {
    let mut blob = Vec::new();
    for (_, _, space) in WATCHED_REGIONS {
        let snap = snapshot(state, *space).unwrap_or_default();
        blob.extend_from_slice(&(snap.len() as u32).to_le_bytes());
        blob.extend_from_slice(&snap);
    }
    let len = blob.len() as u64;
    state.saved_states.insert(slot, blob);
    Ok(len)
}

fn load_state(state: &mut State, slot: u32) -> anyhow::Result<()> {
    if !state.saved_states.contains_key(&slot) {
        anyhow::bail!("slot {slot} is empty");
    }
    // M3: actually restore via libra_unserialize.
    Ok(())
}
