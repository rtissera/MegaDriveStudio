// SPDX-License-Identifier: MIT
//! Emulator actor: owns the libra context on a dedicated thread, drives the
//! frame loop, and exposes an async command interface to the MCP layer.
//!
//! # Threading
//! There is a single OS thread that calls into `libra_run` (libretro cores
//! are not thread-safe). All access to the libra context flows through a
//! `tokio::sync::mpsc::UnboundedSender<Command>`. Each command carries a
//! `oneshot::Sender` that the worker uses to ship the response back.
//!
//! The worker also publishes resource-changed events on a
//! `tokio::sync::broadcast::Sender<ResourceEvent>`; subscribers read these
//! through `EmulatorActor::subscribe()`.
//!
//! # libra optionality
//! Real FFI calls are gated by `#[cfg(libra_present)]` (set by `build.rs`
//! when bindgen succeeded). If libra isn't linked, the worker still runs
//! and serves the same protocol — every "frame" is a no-op and emulator-
//! dependent fields are reported as defaults. This keeps the MCP smoke
//! tests green even before the libretro core fork lands.

pub mod decode;
pub mod memory;
pub mod runner;

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

pub use memory::MemorySpace;

/// Information returned by `mega_load_rom`.
#[derive(Debug, Clone, Serialize)]
pub struct RomInfo {
    pub size: u64,
    pub crc32: u32,
    pub header_name: String,
    pub region: String,
}

/// One-shot reply channel.
pub type Reply<T> = oneshot::Sender<Result<T>>;

/// Commands posted to the emulator thread.
#[derive(Debug)]
pub enum Command {
    LoadRom {
        path: PathBuf,
        reply: Reply<RomInfo>,
    },
    UnloadRom {
        reply: Reply<()>,
    },
    Pause {
        reply: Reply<u64>,
    },
    Resume {
        reply: Reply<u64>,
    },
    StepFrame {
        n: u32,
        reply: Reply<u64>,
    },
    ReadMemory {
        space: MemorySpace,
        addr: u32,
        length: u32,
        reply: Reply<Vec<u8>>,
    },
    WriteMemory {
        space: MemorySpace,
        addr: u32,
        bytes: Vec<u8>,
        reply: Reply<()>,
    },
    SnapshotRegion {
        space: MemorySpace,
        reply: Reply<Vec<u8>>,
    },
    Status {
        reply: Reply<Status>,
    },
    SaveState {
        slot: u32,
        reply: Reply<u64>,
    },
    LoadState {
        slot: u32,
        reply: Reply<()>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub rom_loaded: bool,
    pub paused: bool,
    pub frame: u64,
    pub fps_avg: f64,
    pub target: &'static str,
    pub libra_linked: bool,
}

/// Resource-changed broadcast event. Carries pre-encoded payloads so
/// subscribers don't have to call back into the emulator thread.
#[derive(Debug, Clone)]
#[allow(dead_code)] // fields read by the MCP layer once subscriptions ship to clients
pub struct ResourceEvent {
    pub uri: &'static str,
    pub mime: &'static str,
    pub payload: Arc<Vec<u8>>,
}

#[derive(Clone)]
pub struct EmulatorActor {
    cmd_tx: mpsc::UnboundedSender<Command>,
    #[allow(dead_code)] // used via subscribe(); held alive for fan-out subscribers
    bcast_tx: broadcast::Sender<ResourceEvent>,
}

impl EmulatorActor {
    /// Spawn the emulator OS thread and return a handle.
    ///
    /// `core_path` points at `clownmdemu_libretro.so` (or whatever core the
    /// user passed via `--core`); the worker tries to load it lazily on the
    /// first `LoadRom` command — failure is reported through that command's
    /// reply rather than at start-up, so the MCP layer can come up cleanly
    /// without an active core.
    pub fn spawn(core_path: PathBuf) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
        let (bcast_tx, _) = broadcast::channel::<ResourceEvent>(128);
        let bcast_clone = bcast_tx.clone();
        std::thread::Builder::new()
            .name("mds-emu".into())
            .spawn(move || {
                runner::run(core_path, cmd_rx, bcast_clone);
            })
            .expect("spawn emulator thread");
        Self { cmd_tx, bcast_tx }
    }

    #[allow(dead_code)] // wired up to MCP resources/subscribe in M3
    pub fn subscribe(&self) -> broadcast::Receiver<ResourceEvent> {
        self.bcast_tx.subscribe()
    }

    async fn dispatch<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(build(tx))
            .map_err(|_| anyhow!("emulator thread is gone"))?;
        rx.await
            .map_err(|_| anyhow!("emulator dropped reply channel"))?
    }

    pub async fn load_rom(&self, path: PathBuf) -> Result<RomInfo> {
        self.dispatch(|reply| Command::LoadRom { path, reply }).await
    }

    pub async fn unload_rom(&self) -> Result<()> {
        self.dispatch(|reply| Command::UnloadRom { reply }).await
    }

    pub async fn pause(&self) -> Result<u64> {
        self.dispatch(|reply| Command::Pause { reply }).await
    }

    pub async fn resume(&self) -> Result<u64> {
        self.dispatch(|reply| Command::Resume { reply }).await
    }

    pub async fn step_frame(&self, n: u32) -> Result<u64> {
        self.dispatch(|reply| Command::StepFrame { n, reply }).await
    }

    pub async fn read_memory(
        &self,
        space: MemorySpace,
        addr: u32,
        length: u32,
    ) -> Result<Vec<u8>> {
        self.dispatch(|reply| Command::ReadMemory {
            space,
            addr,
            length,
            reply,
        })
        .await
    }

    pub async fn write_memory(
        &self,
        space: MemorySpace,
        addr: u32,
        bytes: Vec<u8>,
    ) -> Result<()> {
        self.dispatch(|reply| Command::WriteMemory {
            space,
            addr,
            bytes,
            reply,
        })
        .await
    }

    pub async fn snapshot_region(&self, space: MemorySpace) -> Result<Vec<u8>> {
        self.dispatch(|reply| Command::SnapshotRegion { space, reply }).await
    }

    pub async fn status(&self) -> Result<Status> {
        self.dispatch(|reply| Command::Status { reply }).await
    }

    pub async fn save_state(&self, slot: u32) -> Result<u64> {
        self.dispatch(|reply| Command::SaveState { slot, reply }).await
    }

    pub async fn load_state(&self, slot: u32) -> Result<()> {
        self.dispatch(|reply| Command::LoadState { slot, reply }).await
    }

    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(Command::Shutdown);
    }
}

/// CRC-32/IEEE (poly 0xEDB88320). Not used on the hot path — the runner uses
/// `crc32fast` — but kept here for ROM-load identification, which only runs
/// once per `mega_load_rom`.
pub fn crc32_ieee(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// Best-effort region decode from the Mega Drive ROM header at offset 0x1F0.
pub fn region_from_header(rom: &[u8]) -> String {
    if rom.len() < 0x1F4 {
        return "?".into();
    }
    let r = std::str::from_utf8(&rom[0x1F0..0x1F4])
        .unwrap_or("")
        .trim_end_matches('\0')
        .trim();
    if r.is_empty() { "?".into() } else { r.into() }
}

/// Read the in-header game name (Mega Drive overseas title) from `0x150..0x180`.
pub fn header_name(rom: &[u8]) -> String {
    if rom.len() < 0x180 {
        return String::new();
    }
    std::str::from_utf8(&rom[0x150..0x180])
        .unwrap_or("")
        .trim_end()
        .to_string()
}
