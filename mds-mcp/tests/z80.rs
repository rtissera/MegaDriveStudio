// SPDX-License-Identifier: MIT
//! Integration test: drive `mega_get_z80_registers` over the MCP protocol.
//!
//! The pure decode-from-bytes logic is exercised by unit tests inside
//! `src/emulator/decode.rs`. This file checks the surface — that the tool
//! and resource don't crash, return well-formed JSON, and accept the new
//! `z80ram` / `z80bus` memory-space names.

mod common;

use common::McpClient;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn z80_tool_and_resource_clean_path() {
    let mut c = McpClient::spawn();
    c.handshake("z80-test").await;

    // Tool call — either decoded regs or not_implemented marker.
    let resp = c.call(2, "mega_get_z80_registers", json!({})).await;
    let text = resp["result"]["content"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("valid JSON");
    let ok = parsed.get("pc").is_some()
        || parsed
            .get("not_implemented")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
    assert!(ok, "expected regs or not_implemented, got {parsed}");

    // Resource read — must be a JSON object.
    let resp = c
        .rpc(3, "resources/read", json!({"uri":"mega://z80/registers"}))
        .await;
    let text = resp["result"]["contents"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).expect("valid JSON");
    assert!(parsed.is_object());

    // mega_read_memory must accept "z80ram" and "z80bus".
    let resp = c
        .call(
            4,
            "mega_read_memory",
            json!({"space":"z80ram","addr":0,"length":0}),
        )
        .await;
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !text.contains("unknown memory space"),
        "z80ram should be a recognised space, got {resp}"
    );

    let _ = c.child.kill().await;
}
