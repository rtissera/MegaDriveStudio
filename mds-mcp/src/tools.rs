// SPDX-License-Identifier: MIT
//! MCP tool surface for M1: three tools — `mega_load_rom`, `mega_pause`,
//! `mega_read_memory`. The full 19-tool catalogue arrives in M2.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::emulator::{EmulatorActor, MemorySpace};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoadRomArgs {
    /// Absolute path to the ROM file (.bin / .md / .gen).
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadMemoryArgs {
    /// Memory space: one of "ram", "vram", "cram", "vsram", "rom", "z80",
    /// "saveram". M1 only implements "rom".
    pub space: String,
    /// Start address within the space.
    pub addr: u32,
    /// Number of bytes to read.
    pub length: u32,
}

#[derive(Debug, Serialize)]
struct ReadMemoryResult {
    addr: u32,
    length: u32,
    space: String,
    data: String, // base64
}

#[derive(Clone)]
pub struct MdsServer {
    actor: EmulatorActor,
    // Read by `#[tool_handler]`-generated code at dispatch time.
    #[allow(dead_code)]
    tool_router: ToolRouter<MdsServer>,
}

impl MdsServer {
    pub fn new() -> Self {
        Self {
            actor: EmulatorActor::new(),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl MdsServer {
    #[tool(
        description = "Load a Mega Drive / Genesis ROM into the emulator. Returns size, CRC-32, and the in-header game name."
    )]
    async fn mega_load_rom(
        &self,
        Parameters(args): Parameters<LoadRomArgs>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(args.path);
        match self.actor.load_rom(path).await {
            Ok(info) => {
                let payload = serde_json::json!({
                    "ok": true,
                    "size": info.size,
                    "crc32": format!("{:08X}", info.crc32),
                    "header_name": info.header_name,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&payload).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "load_rom failed: {e}"
            ))])),
        }
    }

    #[tool(
        description = "Toggle pause on the emulator. Returns the current frame counter."
    )]
    async fn mega_pause(&self) -> Result<CallToolResult, McpError> {
        match self.actor.pause().await {
            Ok(frame) => {
                let payload = serde_json::json!({ "ok": true, "frame": frame });
                Ok(CallToolResult::success(vec![Content::text(
                    payload.to_string(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "pause failed: {e}"
            ))])),
        }
    }

    #[tool(
        description = "Read raw bytes from a memory space. M1 only implements space=\"rom\". Returns base64-encoded data."
    )]
    async fn mega_read_memory(
        &self,
        Parameters(args): Parameters<ReadMemoryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let space = match parse_space(&args.space) {
            Ok(s) => s,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(e)]));
            }
        };
        match self.actor.read_memory(space, args.addr, args.length).await {
            Ok(bytes) => {
                let result = ReadMemoryResult {
                    addr: args.addr,
                    length: args.length,
                    space: args.space,
                    data: B64.encode(&bytes),
                };
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string(&result).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "read_memory failed: {e}"
            ))])),
        }
    }
}

fn parse_space(s: &str) -> Result<MemorySpace, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "ram" => MemorySpace::Ram,
        "vram" => MemorySpace::Vram,
        "cram" => MemorySpace::Cram,
        "vsram" => MemorySpace::Vsram,
        "rom" => MemorySpace::Rom,
        "z80" => MemorySpace::Z80,
        "saveram" => MemorySpace::Saveram,
        other => return Err(format!("unknown memory space: {other:?}")),
    })
}

#[tool_handler]
impl ServerHandler for MdsServer {
    fn get_info(&self) -> ServerInfo {
        let server_info = Implementation::new("mds-mcp", env!("CARGO_PKG_VERSION"));
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(server_info)
            .with_instructions(
                "Megadrive Studio MCP server (M1). Tools: mega_load_rom, mega_pause, \
                 mega_read_memory. Full catalogue (19 tools) lands in M2.",
            )
    }
}

impl Default for MdsServer {
    fn default() -> Self {
        Self::new()
    }
}
