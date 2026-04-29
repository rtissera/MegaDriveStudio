// SPDX-License-Identifier: MIT
//! Black-box integration smoke tests for the MCP surface.
//!
//! Spawns the release binary with stdio transport, sends a hand-rolled
//! JSON-RPC sequence (initialize → notifications/initialized → tools/list →
//! resources/list → tools/call), and asserts on the framed JSON responses.

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::time::timeout;

const TARGET_DEFAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target");

fn resolved_binary() -> String {
    if let Ok(p) = std::env::var("MDS_MCP_BIN") {
        return p;
    }
    let d = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| TARGET_DEFAULT.to_string());
    format!("{d}/release/mds-mcp")
}

async fn send(stdin: &mut ChildStdin, value: Value) {
    let mut s = serde_json::to_string(&value).unwrap();
    s.push('\n');
    stdin.write_all(s.as_bytes()).await.unwrap();
    stdin.flush().await.unwrap();
}

async fn read_response(reader: &mut BufReader<tokio::process::ChildStdout>) -> Value {
    let mut line = String::new();
    loop {
        line.clear();
        let n = timeout(Duration::from_secs(8), reader.read_line(&mut line))
            .await
            .expect("response timed out")
            .unwrap();
        assert!(n > 0, "EOF on stdout");
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
            panic!("invalid JSON from server: {e}: {trimmed}");
        });
        // Ignore notifications (id missing); only real responses count.
        if v.get("id").is_some() {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_smoke() {
    let bin = resolved_binary();
    assert!(
        std::path::Path::new(&bin).exists(),
        "binary not built at {bin} — run `cargo build --release` first",
    );

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn mds-mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "smoke", "version": "0.0.0"}
            }
        }),
    )
    .await;
    let init = read_response(&mut reader).await;
    assert_eq!(init["id"], 1, "initialize response id");
    assert_eq!(init["result"]["serverInfo"]["name"], "mds-mcp");

    // 2. initialized notification
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;

    // 3. tools/list
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    let tools = read_response(&mut reader).await;
    let arr = tools["result"]["tools"].as_array().expect("tools array");
    assert!(
        arr.len() >= 19,
        "expected ≥19 tools, got {}: {:?}",
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
        "mega_continue",
        "mega_screenshot",
        "mega_save_state",
        "mega_load_state",
        "mega_get_status",
    ] {
        assert!(
            names.contains(&needed),
            "tool {needed} missing from tools/list"
        );
    }

    // 4. resources/list
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"resources/list"}),
    )
    .await;
    let res = read_response(&mut reader).await;
    let resources = res["result"]["resources"].as_array().expect("resources");
    assert_eq!(
        resources.len(),
        7,
        "expected 7 resources, got {}",
        resources.len()
    );
    let uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    for needed in [
        "mega://vram",
        "mega://cram",
        "mega://vsram",
        "mega://vdp/registers",
        "mega://sprites",
        "mega://m68k/registers",
        "mega://framebuffer",
    ] {
        assert!(uris.contains(&needed), "resource {needed} missing");
    }

    // 5. tools/call mega_get_status — ROM not loaded.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"mega_get_status","arguments":{}}
        }),
    )
    .await;
    let status = read_response(&mut reader).await;
    let text = status["result"]["content"][0]["text"]
        .as_str()
        .expect("status text");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["rom_loaded"], false, "expected rom_loaded=false");

    // 6. tools/call mega_load_rom with bogus path → clean error, no panic.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"mega_load_rom","arguments":{"path":"/tmp/__definitely-not-a-rom__.bin"}}
        }),
    )
    .await;
    let load = read_response(&mut reader).await;
    // Either CallToolResult.error=true or a JSON-RPC error — both are clean.
    let is_error = load["result"]["isError"].as_bool().unwrap_or(false)
        || load.get("error").is_some();
    assert!(is_error, "load_rom on bogus path should return an error");

    let _ = child.kill().await;
}
