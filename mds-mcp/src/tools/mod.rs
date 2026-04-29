// SPDX-License-Identifier: MIT
//! MCP tool surface — 19 tools covering control / memory / vdp / cpu / state.
//!
//! All tools live on `MdsServer` so the `#[tool_router]` and `#[tool_handler]`
//! pair share the same `Self` type, which the macro requires. Tools that
//! haven't reached implementation yet (M3 / M4) return a structured
//! `not_implemented` payload rather than failing the call, so clients can
//! discover the surface today and gate features by checking the response.

#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::{
    handler::server::wrapper::Parameters, model::{CallToolResult, Content}, schemars, tool, tool_router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::emulator::{decode, MemorySpace};
use crate::server::MdsServer;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoadRomArgs {
    /// Absolute path to the ROM file (.bin/.md/.gen).
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct StepFrameArgs {
    /// Number of frames to advance. Default 1.
    #[serde(default)]
    pub n: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct StepInstructionArgs {
    #[serde(default)]
    pub n: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadMemoryArgs {
    /// One of "ram"|"vram"|"cram"|"vsram"|"rom"|"saveram"|"vdp_state"|"m68k_state"|"z80".
    pub space: String,
    pub addr: u32,
    pub length: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WriteMemoryArgs {
    pub space: String,
    pub addr: u32,
    /// Base64-encoded payload.
    pub data: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DumpTileArgs {
    pub index: u32,
    #[serde(default)]
    pub palette: Option<u8>,
    /// "8x8" (default) or "8x16".
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct BreakpointArgs {
    #[serde(default)]
    pub addr: Option<u32>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub space: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ClearBreakpointArgs {
    pub id: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct ScreenshotArgs {
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct StateSlotArgs {
    #[serde(default)]
    pub slot: Option<u32>,
}

#[derive(Debug, Serialize)]
struct ReadMemoryResult {
    addr: u32,
    length: u32,
    space: String,
    data: String,
}

fn ok_json(value: serde_json::Value) -> CallToolResult {
    CallToolResult::success(vec![Content::text(value.to_string())])
}
fn err_text(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![Content::text(msg.into())])
}
fn not_implemented(reason: &str) -> CallToolResult {
    ok_json(serde_json::json!({
        "ok": false,
        "not_implemented": true,
        "reason": reason,
    }))
}

#[tool_router(router = tool_router, vis = "pub(crate)")]
impl MdsServer {
    #[tool(description = "Load a Mega Drive / Genesis ROM into the emulator. Returns size, CRC-32, in-header game name, and region.")]
    async fn mega_load_rom(
        &self,
        Parameters(args): Parameters<LoadRomArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let r = self.actor().load_rom(PathBuf::from(args.path)).await;
        Ok(match r {
            Ok(info) => ok_json(serde_json::json!({
                "ok": true,
                "size": info.size,
                "crc32": format!("{:08X}", info.crc32),
                "header_name": info.header_name,
                "region": info.region,
            })),
            Err(e) => err_text(format!("load_rom failed: {e}")),
        })
    }

    #[tool(description = "Unload the currently loaded ROM and reset emulator state.")]
    async fn mega_unload_rom(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.actor().unload_rom().await {
            Ok(()) => ok_json(serde_json::json!({"ok": true})),
            Err(e) => err_text(format!("unload_rom failed: {e}")),
        })
    }

    #[tool(description = "Pause the emulator. Returns the current frame counter.")]
    async fn mega_pause(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.actor().pause().await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("pause failed: {e}")),
        })
    }

    #[tool(description = "Resume the emulator. Returns the current frame counter.")]
    async fn mega_resume(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.actor().resume().await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("resume failed: {e}")),
        })
    }

    #[tool(description = "Advance the emulator by N frames (default 1) and pause. Returns the new frame counter.")]
    async fn mega_step_frame(
        &self,
        Parameters(args): Parameters<StepFrameArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let n = args.n.unwrap_or(1).min(10_000);
        Ok(match self.actor().step_frame(n).await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("step_frame failed: {e}")),
        })
    }

    #[tool(description = "Step N 68k instructions (M3 — not yet wired up to the libretro core).")]
    async fn mega_step_instruction(
        &self,
        Parameters(_args): Parameters<StepInstructionArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(not_implemented("M3 — needs core-side instruction-step hook"))
    }

    #[tool(description = "Read raw bytes from a memory space. Returns base64-encoded data.")]
    async fn mega_read_memory(
        &self,
        Parameters(args): Parameters<ReadMemoryArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let Some(space) = MemorySpace::parse(&args.space) else {
            return Ok(err_text(format!("unknown memory space: {:?}", args.space)));
        };
        Ok(
            match self
                .actor()
                .read_memory(space, args.addr, args.length)
                .await
            {
                Ok(bytes) => {
                    let res = ReadMemoryResult {
                        addr: args.addr,
                        length: args.length,
                        space: args.space,
                        data: B64.encode(&bytes),
                    };
                    ok_json(serde_json::to_value(&res).unwrap_or_default())
                }
                Err(e) => err_text(format!("read_memory failed: {e}")),
            },
        )
    }

    #[tool(description = "Write base64-encoded bytes to a memory space.")]
    async fn mega_write_memory(
        &self,
        Parameters(args): Parameters<WriteMemoryArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let Some(space) = MemorySpace::parse(&args.space) else {
            return Ok(err_text(format!("unknown memory space: {:?}", args.space)));
        };
        let bytes = match B64.decode(args.data.as_bytes()) {
            Ok(b) => b,
            Err(e) => return Ok(err_text(format!("invalid base64: {e}"))),
        };
        Ok(match self.actor().write_memory(space, args.addr, bytes).await {
            Ok(()) => ok_json(serde_json::json!({"ok": true})),
            Err(e) => err_text(format!("write_memory failed: {e}")),
        })
    }

    #[tool(description = "Get the 24 VDP registers and a decoded summary (planes, sprite table, H40/V30, ...).")]
    async fn mega_get_vdp_registers(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let blob = self
            .actor()
            .snapshot_region(MemorySpace::VdpState)
            .await
            .unwrap_or_default();
        let regs = decode::decode_vdp_registers(&blob);
        Ok(ok_json(serde_json::to_value(&regs).unwrap_or_default()))
    }

    #[tool(description = "Get the four 16-colour palette lines as RGB triplets (CRAM 9-bit BGR expanded to 8-bit).")]
    async fn mega_get_palettes(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let cram = self
            .actor()
            .snapshot_region(MemorySpace::Cram)
            .await
            .unwrap_or_default();
        let pal = decode::decode_palettes(&cram);
        Ok(ok_json(serde_json::to_value(&pal).unwrap_or_default()))
    }

    #[tool(description = "Decode the sprite attribute table (up to 80 sprites). Walks the linked list from VDP reg #5.")]
    async fn mega_get_sprites(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let vdp = self
            .actor()
            .snapshot_region(MemorySpace::VdpState)
            .await
            .unwrap_or_default();
        let regs = decode::decode_vdp_registers(&vdp);
        let vram = self
            .actor()
            .snapshot_region(MemorySpace::Vram)
            .await
            .unwrap_or_default();
        let sprites = decode::decode_sprites(&vram, regs.decoded.sprite_table, 80);
        Ok(ok_json(serde_json::to_value(&sprites).unwrap_or_default()))
    }

    #[tool(description = "Dump a tile from VRAM as an indexed bitmap plus its RGBA palette line.")]
    async fn mega_dump_tile(
        &self,
        Parameters(args): Parameters<DumpTileArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let format = args.format.as_deref().unwrap_or("8x8");
        let palette_idx = args.palette.unwrap_or(0).min(3);
        let vram = self
            .actor()
            .snapshot_region(MemorySpace::Vram)
            .await
            .unwrap_or_default();
        let cram = self
            .actor()
            .snapshot_region(MemorySpace::Cram)
            .await
            .unwrap_or_default();

        let (bitmap, height) = if format == "8x16" {
            let mut combined = Vec::with_capacity(128);
            if let Some(top) = decode::decode_tile_8x8(&vram, args.index) {
                combined.extend(top);
            }
            if let Some(bot) = decode::decode_tile_8x8(&vram, args.index + 1) {
                combined.extend(bot);
            }
            (combined, 16u32)
        } else {
            (decode::decode_tile_8x8(&vram, args.index).unwrap_or_default(), 8u32)
        };

        let pal = decode::decode_palettes(&cram);
        let mut rgba = Vec::with_capacity(16 * 4);
        if let Some(line) = pal.get(palette_idx as usize) {
            for c in line {
                rgba.extend_from_slice(&[c.r, c.g, c.b, 0xFF]);
            }
        }
        Ok(ok_json(serde_json::json!({
            "width": 8,
            "height": height,
            "bitmap": B64.encode(&bitmap),
            "palette": B64.encode(&rgba),
        })))
    }

    #[tool(description = "Decode the 68k registers (D0..D7, A0..A7, PC, SR, USP, SSP) from the m68k_state blob.")]
    async fn mega_get_68k_registers(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let blob = self
            .actor()
            .snapshot_region(MemorySpace::M68kState)
            .await
            .unwrap_or_default();
        match decode::decode_m68k(&blob) {
            Some(r) => Ok(ok_json(serde_json::to_value(&r).unwrap_or_default())),
            None => Ok(not_implemented(
                "m68k_state blob layout not yet stable in the libretro core fork",
            )),
        }
    }

    #[tool(description = "Decode the Z80 registers (M3 — pending the libretro core fork's Z80 state hook).")]
    async fn mega_get_z80_registers(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(not_implemented("M3 — Z80 state blob hook not yet exposed"))
    }

    #[tool(description = "Set a breakpoint (M4).")]
    async fn mega_set_breakpoint(
        &self,
        Parameters(_args): Parameters<BreakpointArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(not_implemented("M4 — breakpoints arrive with the GDB-stub layer"))
    }

    #[tool(description = "Clear a breakpoint by id (M4).")]
    async fn mega_clear_breakpoint(
        &self,
        Parameters(_args): Parameters<ClearBreakpointArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(not_implemented("M4 — breakpoints arrive with the GDB-stub layer"))
    }

    #[tool(description = "Continue execution after a breakpoint (alias for resume in M2).")]
    async fn mega_continue(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.actor().resume().await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("continue failed: {e}")),
        })
    }

    #[tool(description = "Take a screenshot (M3 — defers to the libra video callback).")]
    async fn mega_screenshot(
        &self,
        Parameters(_args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(not_implemented(
            "M3 — needs the libra video callback wired into a host framebuffer",
        ))
    }

    #[tool(description = "Save emulator state to a numbered slot (default 0).")]
    async fn mega_save_state(
        &self,
        Parameters(args): Parameters<StateSlotArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let slot = args.slot.unwrap_or(0);
        Ok(match self.actor().save_state(slot).await {
            Ok(size) => ok_json(serde_json::json!({"ok": true, "size": size})),
            Err(e) => err_text(format!("save_state failed: {e}")),
        })
    }

    #[tool(description = "Load emulator state from a numbered slot (default 0).")]
    async fn mega_load_state(
        &self,
        Parameters(args): Parameters<StateSlotArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let slot = args.slot.unwrap_or(0);
        Ok(match self.actor().load_state(slot).await {
            Ok(()) => ok_json(serde_json::json!({"ok": true})),
            Err(e) => err_text(format!("load_state failed: {e}")),
        })
    }

    #[tool(description = "Get emulator status: ROM-loaded flag, paused flag, frame counter, average FPS, and target.")]
    async fn mega_get_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        Ok(match self.actor().status().await {
            Ok(s) => ok_json(serde_json::json!({
                "rom_loaded": s.rom_loaded,
                "paused": s.paused,
                "frame": s.frame,
                "fps_avg": s.fps_avg,
                "target": s.target,
                "libra_linked": s.libra_linked,
            })),
            Err(e) => err_text(format!("status failed: {e}")),
        })
    }
}
