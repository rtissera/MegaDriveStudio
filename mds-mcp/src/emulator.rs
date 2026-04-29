// SPDX-License-Identifier: MIT
//! M1 stub for the emulator actor.
//!
//! For M1 we don't drive `libra_run` yet — we only need enough state to
//! validate the MCP tool surface end-to-end. The full frame-loop worker
//! arrives in M2 along with the patched clownmdemu libretro core.

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MemorySpace {
    Ram,
    Vram,
    Cram,
    Vsram,
    Rom,
    Z80,
    Saveram,
}

#[derive(Debug, Clone, Serialize)]
pub struct RomInfo {
    pub size: u64,
    pub crc32: u32,
    pub header_name: String,
}

#[derive(Default)]
struct State {
    rom_path: Option<PathBuf>,
    rom_bytes: Option<Vec<u8>>,
    paused: bool,
    frame: u64,
}

#[derive(Clone)]
pub struct EmulatorActor {
    inner: Arc<Mutex<State>>,
}

impl EmulatorActor {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::default())),
        }
    }

    /// Load a ROM file. M1 implementation: stat + sniff header; the actual
    /// libra core load is deferred to M2.
    pub async fn load_rom(&self, path: PathBuf) -> Result<RomInfo> {
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("reading ROM {}", path.display()))?;
        if bytes.len() < 0x200 {
            bail!("ROM too small: {} bytes (need ≥0x200)", bytes.len());
        }

        // Mega Drive header magic at 0x100.
        let header = &bytes[0x100..0x110];
        let header_str = std::str::from_utf8(header).unwrap_or("");
        let valid = header_str.starts_with("SEGA MEGA DRIVE")
            || header_str.starts_with("SEGA GENESIS")
            || header_str.starts_with("SEGA");
        if !valid {
            bail!(
                "missing SEGA magic at 0x100 (got: {:?})",
                header_str.trim_end()
            );
        }

        let crc32 = crc32_ieee(&bytes);
        let size = bytes.len() as u64;
        let name = std::str::from_utf8(&bytes[0x150..0x180])
            .unwrap_or("")
            .trim_end()
            .to_string();

        let mut g = self.inner.lock();
        g.rom_path = Some(path);
        g.rom_bytes = Some(bytes);
        g.paused = false;
        g.frame = 0;

        Ok(RomInfo {
            size,
            crc32,
            header_name: name,
        })
    }

    /// Toggle paused. Returns the current frame counter (always 0 in M1).
    pub async fn pause(&self) -> Result<u64> {
        let mut g = self.inner.lock();
        g.paused = !g.paused;
        Ok(g.frame)
    }

    /// Read memory. M1 only implements `Rom` — the live-emulator spaces need
    /// the libretro core wired up first.
    pub async fn read_memory(
        &self,
        space: MemorySpace,
        addr: u32,
        length: u32,
    ) -> Result<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        if length > 16 * 1024 * 1024 {
            bail!("length {} exceeds sanity cap (16 MiB)", length);
        }

        match space {
            MemorySpace::Rom => {
                let g = self.inner.lock();
                let rom = g
                    .rom_bytes
                    .as_ref()
                    .ok_or_else(|| anyhow!("no ROM loaded"))?;
                let start = addr as usize;
                let end = start
                    .checked_add(length as usize)
                    .ok_or_else(|| anyhow!("addr+length overflow"))?;
                if end > rom.len() {
                    bail!(
                        "out-of-bounds ROM read: 0x{:08X}..0x{:08X} (rom is {} bytes)",
                        start,
                        end,
                        rom.len()
                    );
                }
                Ok(rom[start..end].to_vec())
            }
            _ => Err(anyhow!(
                "memory space {:?} not implemented in M1 (deferred to M2)",
                space
            )),
        }
    }
}

/// CRC-32/IEEE (poly 0xEDB88320). Small, dependency-free.
fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let mut c = (crc ^ b as u32) & 0xFF;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        crc = (crc >> 8) ^ c;
    }
    !crc
}
