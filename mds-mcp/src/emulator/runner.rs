// SPDX-License-Identifier: MIT
//! Emulator-thread frame loop.
//!
//! Drains the command queue between frames. While running, calls
//! `libra_run` (≤16.7 ms wall-clock target) and re-CRCs the libretro memory
//! regions; differences fan out to subscribers as `ResourceEvent`s. While
//! paused, blocks on `cmd_rx.blocking_recv()` so the thread sleeps cleanly.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use super::decode;
use super::memory;
use super::{Command, MemorySpace, ResourceEvent, RomInfo, Status};
use crate::ffi;

const FRAME_DURATION: Duration = Duration::from_micros(16_667);

#[cfg(libra_present)]
struct LibraCtx {
    raw: *mut ffi::libra_ctx,
    core_loaded: bool,
}

#[cfg(libra_present)]
impl LibraCtx {
    fn new() -> Option<Self> {
        // Build a config with all callbacks NULL — we don't render or play
        // audio in the MCP server. NULL audio specifically dodges the libra
        // audio-resampler heap-corruption bug while the parallel agent
        // finishes the patch.
        let mut cfg: ffi::libra_config_t = unsafe { std::mem::zeroed() };
        cfg.audio_output_rate = 48_000;
        let raw = unsafe { ffi::libra_create(&cfg as *const _) };
        if raw.is_null() {
            return None;
        }
        Some(Self {
            raw,
            core_loaded: false,
        })
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
}

#[cfg(libra_present)]
impl Drop for LibraCtx {
    fn drop(&mut self) {
        if !self.raw.is_null() {
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
}

#[cfg(not(libra_present))]
impl LibraCtx {
    fn new() -> Option<Self> {
        Some(Self {
            raw: std::ptr::null_mut(),
            core_loaded: false,
        })
    }
    fn load_core(&mut self, _: &str) -> bool {
        false
    }
    fn load_game(&mut self, _: &str) -> bool {
        false
    }
    fn run_frame(&self) {}
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
}

impl State {
    fn new(core_path: PathBuf) -> Self {
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
        }
    }
}

pub fn run(
    core_path: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    bcast: broadcast::Sender<ResourceEvent>,
) {
    let mut state = State::new(core_path);
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

    // VDP registers — JSON.
    if let Some(id) = MemorySpace::VdpState.libretro_id() {
        if let Some(blob) = unsafe { memory::snapshot_region(state.libra.raw, id) } {
            if !blob.is_empty() {
                let crc = crc32fast::hash(&blob);
                if state.region_crcs.get("mega://vdp/registers").copied() != Some(crc) {
                    state.region_crcs.insert("mega://vdp/registers", crc);
                    let regs = decode::decode_vdp_registers(&blob);
                    if let Ok(s) = serde_json::to_vec(&regs) {
                        let _ = bcast.send(ResourceEvent {
                            uri: "mega://vdp/registers",
                            mime: "application/json",
                            payload: Arc::new(s),
                        });
                    }
                }
            }
        }
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
