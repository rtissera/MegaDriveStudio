// SPDX-License-Identifier: MIT
//! Black-box integration smoke tests for the MCP surface.
//!
//! Spawns the release binary with stdio transport, sends a hand-rolled
//! JSON-RPC sequence, and asserts the tool + resource catalogues match
//! what M4 promises.

mod common;

use common::McpClient;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_smoke() {
    let mut c = McpClient::spawn();
    let init = c.handshake("smoke").await;
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "mds-mcp");

    // tools/list
    let tools = c.rpc(2, "tools/list", json!({})).await;
    let arr = tools["result"]["tools"].as_array().expect("tools array");
    assert!(
        arr.len() >= 22,
        "expected ≥22 tools, got {}: {:?}",
        arr.len(),
        arr.iter().map(|t| t["name"].as_str()).collect::<Vec<_>>()
    );
    let names: Vec<&str> = arr.iter().filter_map(|t| t["name"].as_str()).collect();
    for needed in [
        "mega_load_rom",
        "mega_unload_rom",
        "mega_pause",
        "mega_resume",
        "mega_step_frame",
        "mega_step_instruction",
        "mega_read_memory",
        "mega_write_memory",
        "mega_get_vdp_registers",
        "mega_get_palettes",
        "mega_get_sprites",
        "mega_dump_tile",
        "mega_get_68k_registers",
        "mega_get_z80_registers",
        "mega_set_breakpoint",
        "mega_clear_breakpoint",
        "mega_list_breakpoints",
        "mega_continue",
        "mega_screenshot",
        "mega_save_state",
        "mega_load_state",
        "mega_get_status",
    ] {
        assert!(names.contains(&needed), "tool {needed} missing");
    }

    // resources/list — 9 entries.
    let res = c.rpc(3, "resources/list", json!({})).await;
    let resources = res["result"]["resources"].as_array().expect("resources");
    assert_eq!(resources.len(), 9, "expected 9 resources, got {}", resources.len());
    let uris: Vec<&str> = resources.iter().filter_map(|r| r["uri"].as_str()).collect();
    for needed in [
        "mega://vram",
        "mega://cram",
        "mega://vsram",
        "mega://vdp/registers",
        "mega://sprites",
        "mega://m68k/registers",
        "mega://framebuffer",
        "mega://z80/registers",
        "mega://breakpoints",
    ] {
        assert!(uris.contains(&needed), "resource {needed} missing");
    }

    // mega_get_status — ROM not loaded.
    let status = c.call(4, "mega_get_status", json!({})).await;
    let text = status["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["rom_loaded"], false);

    // mega_load_rom on bogus path — clean error, no panic.
    let load = c
        .call(
            5,
            "mega_load_rom",
            json!({"path":"/tmp/__definitely-not-a-rom__.bin"}),
        )
        .await;
    let is_error =
        load["result"]["isError"].as_bool().unwrap_or(false) || load.get("error").is_some();
    assert!(is_error, "load_rom on bogus path should return an error");

    let _ = c.child.kill().await;
}
