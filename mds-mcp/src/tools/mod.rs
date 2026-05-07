// SPDX-License-Identifier: MIT
//! MCP tool surface — 26 tools covering control / memory / vdp / cpu /
//! state / breakpoints / input. Tools whose backing core feature isn't ready
//! yet return a structured `not_implemented` / `debug_api_unavailable`
//! payload rather than failing the call.

#![allow(dead_code)]

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::{
    handler::server::wrapper::Parameters, model::{CallToolResult, Content}, schemars, tool, tool_router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::emulator::{
    breakpoints::BpKind, breakpoints::BpSpace, decode, frame as fbuf, input::Button, MemorySpace,
};
use crate::server::MdsServer;
use crate::target::{TargetKind, NOT_SUPPORTED};

/// Classify an `anyhow::Error` from an `EdProTarget` method into a
/// structured MCP response. EdProTarget conventions:
///
/// - `sync_mut()` returns `"{NOT_SUPPORTED}: edpro target not connected"`
///   when no transport is wired (string contains both sentinels). M5.6
///   surfaces this as `reason: "not_connected"` so callers can branch.
/// - Permanently-unsupported / TODO-stub methods bail with a string that
///   contains `NOT_SUPPORTED` (no "not connected") → `not_supported_on_target`.
/// - Anything else is a real I/O / decode failure → `err_text`.
fn classify_edpro_err(tool: &str, err: &anyhow::Error) -> CallToolResult {
    let msg = err.to_string();
    if msg.contains("not connected") {
        ok_json(serde_json::json!({
            "ok": false,
            "reason": "not_connected",
            "tool": tool,
            "message": format!("{tool}: edpro target not connected"),
        }))
    } else if msg.contains(NOT_SUPPORTED) {
        not_supported_on_target(tool)
    } else {
        err_text(format!("{tool} failed: {msg}"))
    }
}

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

/// Partial joypad state — every field optional. `None` means "leave that
/// button untouched"; `Some(true|false)` flips it.
#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct InputButtons {
    #[serde(default)] pub up: Option<bool>,
    #[serde(default)] pub down: Option<bool>,
    #[serde(default)] pub left: Option<bool>,
    #[serde(default)] pub right: Option<bool>,
    #[serde(default)] pub a: Option<bool>,
    #[serde(default)] pub b: Option<bool>,
    #[serde(default)] pub c: Option<bool>,
    #[serde(default)] pub start: Option<bool>,
    #[serde(default)] pub x: Option<bool>,
    #[serde(default)] pub y: Option<bool>,
    #[serde(default)] pub z: Option<bool>,
    #[serde(default)] pub mode: Option<bool>,
}

impl InputButtons {
    fn into_pairs(self) -> Vec<(Button, bool)> {
        let mut v = Vec::new();
        macro_rules! push { ($f:ident, $b:expr) => { if let Some(p) = self.$f { v.push(($b, p)); } }; }
        push!(up, Button::Up);
        push!(down, Button::Down);
        push!(left, Button::Left);
        push!(right, Button::Right);
        push!(a, Button::A);
        push!(b, Button::B);
        push!(c, Button::C);
        push!(start, Button::Start);
        push!(x, Button::X);
        push!(y, Button::Y);
        push!(z, Button::Z);
        push!(mode, Button::Mode);
        v
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct InputSetStateArgs {
    /// Joypad port 0 or 1 (default 0).
    #[serde(default)]
    pub port: Option<u32>,
    /// Partial set of buttons to flip. Buttons not listed are left alone.
    #[serde(default)]
    pub buttons: InputButtons,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InputButtonArgs {
    #[serde(default)]
    pub port: Option<u32>,
    /// One of "up"|"down"|"left"|"right"|"a"|"b"|"c"|"start"|"x"|"y"|"z"|"mode".
    pub button: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct InputGetStateArgs {
    #[serde(default)]
    pub port: Option<u32>,
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

/// Returned by emulator-only tools when running against the EdPro target
/// stub. Carries `ok:false` + a structured `reason` so callers can branch
/// without parsing free-form strings.
fn not_supported_on_target(tool: &str) -> CallToolResult {
    ok_json(serde_json::json!({
        "ok": false,
        "reason": NOT_SUPPORTED,
        "tool": tool,
        "message": format!("{tool} is not supported on the EdPro hardware target stub (M5.1)"),
    }))
}

impl MdsServer {
    /// True iff the active target is EdPro. The dispatcher uses this to
    /// route a tool call to `EdProTarget` instead of the emulator actor.
    /// M5.6 removed the per-tool `block_on_edpro` short-circuit:
    /// `EdProTarget` itself returns `not_supported_on_target` (or
    /// `not_connected`) on a per-method basis, which lets future M5.7+
    /// wiring switch tools from stub to wired without touching this file.
    fn is_edpro(&self) -> bool {
        self.target_kind() == TargetKind::EdPro
    }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.load_rom(std::path::Path::new(&args.path)).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_load_rom", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.unload_rom().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_unload_rom", &e),
            });
        }
        Ok(match self.actor().unload_rom().await {
            Ok(()) => ok_json(serde_json::json!({"ok": true})),
            Err(e) => err_text(format!("unload_rom failed: {e}")),
        })
    }

    #[tool(description = "Pause the emulator. Returns the current frame counter.")]
    async fn mega_pause(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.pause().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_pause", &e),
            });
        }
        Ok(match self.actor().pause().await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("pause failed: {e}")),
        })
    }

    #[tool(description = "Resume the emulator. Returns the current frame counter.")]
    async fn mega_resume(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.resume().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_resume", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            let n = args.n.unwrap_or(1);
            return Ok(match t.step_frame(n).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_step_frame", &e),
            });
        }
        let n = args.n.unwrap_or(1).min(10_000);
        Ok(match self.actor().step_frame(n).await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("step_frame failed: {e}")),
        })
    }

    #[tool(description = "Step N 68k instructions (default 1) and pause. Returns {pc, sr, frame, instructions_executed, granularity}. `granularity` is \"instruction\" when the patched libretro core's debug API is linked, \"frame\" otherwise.")]
    async fn mega_step_instruction(
        &self,
        Parameters(args): Parameters<StepInstructionArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // EdPro stub steps a single 68k instruction per RSP `s`. `n`
            // is not honoured yet (would need a host-side loop); future
            // milestone may hoist it once continue+stop-pump is wired.
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.step_instruction().await {
                Ok(stop) => ok_json(serde_json::json!({
                    "ok": true,
                    "stop_reply": format!("{stop:?}"),
                    "granularity": "instruction",
                })),
                Err(e) => classify_edpro_err("mega_step_instruction", &e),
            });
        }
        let n = args.n.unwrap_or(1).clamp(1, 1_000_000);
        Ok(match self.actor().step_instruction(n).await {
            Ok(out) => ok_json(serde_json::json!({
                "ok": true,
                "pc": out.pc,
                "sr": out.sr,
                "frame": out.frame,
                "instructions_executed": out.instructions_executed,
                "granularity": out.granularity,
            })),
            Err(e) => err_text(format!("step_instruction failed: {e}")),
        })
    }

    #[tool(description = "Read raw bytes from a memory space. Returns base64-encoded data.")]
    async fn mega_read_memory(
        &self,
        Parameters(args): Parameters<ReadMemoryArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // EdPro side has a flat 24-bit M68k address space — `space`
            // is informational only (the on-cart stub doesn't switch
            // based on it). We pass `addr` straight through to the RSP
            // `m` packet.
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.read_memory(args.addr, args.length).await {
                Ok(bytes) => {
                    let res = ReadMemoryResult {
                        addr: args.addr,
                        length: args.length,
                        space: args.space,
                        data: B64.encode(&bytes),
                    };
                    ok_json(serde_json::to_value(&res).unwrap_or_default())
                }
                Err(e) => classify_edpro_err("mega_read_memory", &e),
            });
        }
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
        if self.is_edpro() {
            let bytes = match B64.decode(args.data.as_bytes()) {
                Ok(b) => b,
                Err(e) => return Ok(err_text(format!("invalid base64: {e}"))),
            };
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.write_memory(args.addr, &bytes).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_write_memory", &e),
            });
        }
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
        if self.is_edpro() {
            // EdPro reads 24 raw bytes from the VDP control port
            // ($C00004); we surface them as the same `decode_vdp_registers`
            // shape the emulator path uses so callers see one schema.
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.get_vdp_registers().await {
                Ok(blob) => {
                    let regs = decode::decode_vdp_registers(&blob);
                    ok_json(serde_json::to_value(&regs).unwrap_or_default())
                }
                Err(e) => classify_edpro_err("mega_get_vdp_registers", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.get_palettes().await {
                Ok(_) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_get_palettes", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.get_sprites().await {
                Ok(_) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_get_sprites", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.dump_tile(args.index).await {
                Ok(_) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_dump_tile", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.get_68k_registers().await {
                Ok(r) => ok_json(serde_json::json!({
                    "d": r.d,
                    "a": r.a,
                    "pc": r.pc,
                    "sr": r.sr,
                    "usp": r.usp,
                    "ssp": r.ssp,
                })),
                Err(e) => classify_edpro_err("mega_get_68k_registers", &e),
            });
        }
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

    #[tool(description = "Decode the Z80 registers from the Z80 state blob plus the bus state blob. Returns {af, bc, de, hl, ix, iy, pc, sp, halt, iff1, iff2, im, cycles, bus_requested, bus_reset}.")]
    async fn mega_get_z80_registers(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.get_z80_registers().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_get_z80_registers", &e),
            });
        }
        let state_blob = self
            .actor()
            .snapshot_region(MemorySpace::Z80)
            .await
            .unwrap_or_default();
        let bus_blob = self
            .actor()
            .snapshot_region(MemorySpace::Z80Bus)
            .await
            .unwrap_or_default();
        match decode::decode_z80(&state_blob, &bus_blob) {
            Some(r) => Ok(ok_json(serde_json::to_value(&r).unwrap_or_default())),
            None => Ok(not_implemented(
                "z80 state blob unavailable (libretro core fork not linked yet)",
            )),
        }
    }

    #[tool(description = "Set a breakpoint. `kind`: \"exec\"|\"read\"|\"write\"|\"access\" (default \"exec\"). `space`: \"rom\"|\"ram\" (default \"ram\"). Returns {ok, id}; sets `reason:\"debug_api_unavailable\"` when the patched libretro core's debug API isn't linked yet (the BP is still registered for `mega_list_breakpoints`).")]
    async fn mega_set_breakpoint(
        &self,
        Parameters(args): Parameters<BreakpointArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // EdPro patch-style BPs are exec-only (TRAP #1 word). `kind`
            // and `space` aren't honoured yet; future milestone may
            // reject non-exec or fall back to RSP `Z` packets.
            let Some(addr) = args.addr else {
                return Ok(err_text("missing required field: addr"));
            };
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.set_breakpoint(addr).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true, "addr": addr})),
                Err(e) => classify_edpro_err("mega_set_breakpoint", &e),
            });
        }
        let Some(addr) = args.addr else {
            return Ok(err_text("missing required field: addr"));
        };
        let kind = match args.kind.as_deref() {
            Some(s) => match BpKind::parse(s) {
                Some(k) => k,
                None => return Ok(err_text(format!("unknown breakpoint kind: {s:?}"))),
            },
            None => BpKind::Exec,
        };
        let space = match args.space.as_deref() {
            Some(s) => match BpSpace::parse(s) {
                Some(k) => k,
                None => return Ok(err_text(format!("unknown breakpoint space: {s:?}"))),
            },
            None => BpSpace::Ram,
        };
        Ok(match self.actor().set_breakpoint(addr, kind, space).await {
            Ok(out) => {
                let mut v = serde_json::json!({"ok": out.ok, "id": out.id});
                if let Some(reason) = out.reason {
                    v["reason"] = serde_json::Value::String(reason.to_string());
                }
                ok_json(v)
            }
            Err(e) => err_text(format!("set_breakpoint failed: {e}")),
        })
    }

    #[tool(description = "Clear a breakpoint by id. Returns {ok, removed}.")]
    async fn mega_clear_breakpoint(
        &self,
        Parameters(args): Parameters<ClearBreakpointArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // EdPro tracks BPs by 24-bit address rather than the
            // host-side opaque id used by the emulator. Until a future
            // milestone unifies the surface we treat `args.id` as the
            // 68k address — same convention `set_breakpoint` echoes back.
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.clear_breakpoint(args.id).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true, "removed": true})),
                Err(e) => classify_edpro_err("mega_clear_breakpoint", &e),
            });
        }
        Ok(match self.actor().clear_breakpoint(args.id).await {
            Ok(removed) => ok_json(serde_json::json!({"ok": true, "removed": removed})),
            Err(e) => err_text(format!("clear_breakpoint failed: {e}")),
        })
    }

    #[tool(description = "List active breakpoints. Returns {breakpoints: [{id, addr, kind, space, hit_count, enabled}, ...]}.")]
    async fn mega_list_breakpoints(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // `list_breakpoints` is sync + non-erroring on EdPro — empty
            // when disconnected, populated otherwise. We project addrs
            // into the same `{id, addr, ...}` shape the emulator path
            // emits (id == addr for EdPro patch-style BPs).
            let lock = self.edpro_target();
            let t = lock.lock().await;
            let bps: Vec<serde_json::Value> = t
                .list_breakpoints()
                .into_iter()
                .map(|addr| {
                    serde_json::json!({
                        "id": addr,
                        "addr": addr,
                        "kind": "exec",
                        "space": "rom",
                        "hit_count": 0,
                        "enabled": true,
                    })
                })
                .collect();
            return Ok(ok_json(serde_json::json!({ "breakpoints": bps })));
        }
        Ok(match self.actor().list_breakpoints().await {
            Ok(list) => ok_json(serde_json::json!({
                "breakpoints": list,
            })),
            Err(e) => err_text(format!("list_breakpoints failed: {e}")),
        })
    }

    #[tool(description = "Continue execution after a breakpoint halt. If the emulator isn't halted-on-BP, behaves like `mega_resume`. Returns the current frame counter.")]
    async fn mega_continue(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.continue_run().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_continue", &e),
            });
        }
        Ok(match self.actor().continue_after_halt().await {
            Ok(frame) => ok_json(serde_json::json!({"ok": true, "frame": frame})),
            Err(e) => err_text(format!("continue failed: {e}")),
        })
    }

    #[tool(description = "Take a screenshot of the current emulator framebuffer. format: \"png\" (default, base64) or \"raw\" (RGBA8 bytes, base64). Returns {ok:false, reason:\"no_frame_yet\"} if the video callback has not fired.")]
    async fn mega_screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.screenshot().await {
                Ok(_) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_screenshot", &e),
            });
        }
        let frame = match self.actor().screenshot().await {
            Ok(Some(f)) => f,
            Ok(None) => {
                return Ok(ok_json(serde_json::json!({
                    "ok": false,
                    "reason": "no_frame_yet"
                })));
            }
            Err(e) => return Ok(err_text(format!("screenshot failed: {e}"))),
        };
        let want = args.format.as_deref().unwrap_or("png");
        let rgba = fbuf::to_rgba8(&frame);
        Ok(match want {
            "raw" => ok_json(serde_json::json!({
                "ok": true,
                "format": "raw",
                "width": frame.w,
                "height": frame.h,
                "data": B64.encode(&rgba),
            })),
            _ => match fbuf::rgba8_to_png(&rgba, frame.w, frame.h) {
                Ok(png) => ok_json(serde_json::json!({
                    "ok": true,
                    "format": "png",
                    "width": frame.w,
                    "height": frame.h,
                    "data": B64.encode(&png),
                })),
                Err(e) => err_text(format!("png encode failed: {e}")),
            },
        })
    }

    #[tool(description = "Save emulator state to a numbered slot (default 0).")]
    async fn mega_save_state(
        &self,
        Parameters(args): Parameters<StateSlotArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.save_state(args.slot.unwrap_or(0)).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_save_state", &e),
            });
        }
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
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.load_state(args.slot.unwrap_or(0)).await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_load_state", &e),
            });
        }
        let slot = args.slot.unwrap_or(0);
        Ok(match self.actor().load_state(slot).await {
            Ok(()) => ok_json(serde_json::json!({"ok": true})),
            Err(e) => err_text(format!("load_state failed: {e}")),
        })
    }

    #[tool(description = "Get emulator/hardware status: rom_loaded, paused, frame, fps_avg, target (\"emulator\" or \"edpro\"), libra_linked, connected (EdPro only).")]
    async fn mega_get_status(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            // Pull live status straight from `EdProTarget` so `connected`
            // and `bp_count` reflect the wired state machine. We extend
            // the target's compact JSON with the legacy fields the
            // emulator path emits so existing UI consumers don't fork.
            let lock = self.edpro_target();
            let t = lock.lock().await;
            let st = t.get_status();
            return Ok(ok_json(serde_json::json!({
                "rom_loaded": false,
                "paused": true,
                "frame": 0,
                "fps_avg": 0.0,
                "target": "edpro",
                "libra_linked": cfg!(libra_present),
                "connected": st["connected"].as_bool().unwrap_or(false),
                "edpro_port": st["port"].as_str().unwrap_or("").to_string(),
                "bp_count": st["bp_count"].as_u64().unwrap_or(0),
            })));
        }
        Ok(match self.actor().status().await {
            Ok(s) => ok_json(serde_json::json!({
                "rom_loaded": s.rom_loaded,
                "paused": s.paused,
                "frame": s.frame,
                "fps_avg": s.fps_avg,
                "target": s.target,
                "libra_linked": s.libra_linked,
                "connected": true,
            })),
            Err(e) => err_text(format!("status failed: {e}")),
        })
    }

    #[tool(description = "Set joypad button state on a port (0 or 1, default 0). `buttons` is a partial map — only listed buttons flip; the rest stay as they were. Buttons: up,down,left,right,a,b,c,start,x,y,z,mode (6-button pad).")]
    async fn mega_input_set_state(
        &self,
        Parameters(args): Parameters<InputSetStateArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.input_set_state().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_input_set_state", &e),
            });
        }
        let port = args.port.unwrap_or(0);
        let pairs = args.buttons.into_pairs();
        self.actor().input().apply_partial(port, &pairs);
        Ok(ok_json(serde_json::json!({"ok": true, "port": port, "applied": pairs.len()})))
    }

    #[tool(description = "Press a single joypad button (sets pressed=true). Pair with `mega_input_release` or `mega_input_set_state` to release. Buttons: up,down,left,right,a,b,c,start,x,y,z,mode.")]
    async fn mega_input_press(
        &self,
        Parameters(args): Parameters<InputButtonArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.input_press().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_input_press", &e),
            });
        }
        let port = args.port.unwrap_or(0);
        let Some(b) = Button::parse(&args.button) else {
            return Ok(err_text(format!("unknown button: {:?}", args.button)));
        };
        self.actor().input().press(port, b);
        Ok(ok_json(serde_json::json!({"ok": true, "port": port, "button": b.name()})))
    }

    #[tool(description = "Release a single joypad button (sets pressed=false).")]
    async fn mega_input_release(
        &self,
        Parameters(args): Parameters<InputButtonArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.input_release().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_input_release", &e),
            });
        }
        let port = args.port.unwrap_or(0);
        let Some(b) = Button::parse(&args.button) else {
            return Ok(err_text(format!("unknown button: {:?}", args.button)));
        };
        self.actor().input().release(port, b);
        Ok(ok_json(serde_json::json!({"ok": true, "port": port, "button": b.name()})))
    }

    #[tool(description = "Get the current joypad state for a port (default 0). Returns {port, buttons:{up,down,...,mode}}.")]
    async fn mega_input_get_state(
        &self,
        Parameters(args): Parameters<InputGetStateArgs>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        if self.is_edpro() {
            let lock = self.edpro_target();
            let mut t = lock.lock().await;
            return Ok(match t.input_get_state().await {
                Ok(()) => ok_json(serde_json::json!({"ok": true})),
                Err(e) => classify_edpro_err("mega_input_get_state", &e),
            });
        }
        let port = args.port.unwrap_or(0);
        let map: serde_json::Map<String, serde_json::Value> = self
            .actor()
            .input()
            .snapshot_buttons(port)
            .into_iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::Bool(v)))
            .collect();
        Ok(ok_json(serde_json::json!({"port": port, "buttons": map})))
    }
}
