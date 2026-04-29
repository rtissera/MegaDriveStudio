// SPDX-License-Identifier: MIT
//! M5-prep: smoke-test the EdPro target stub. We don't talk to real
//! hardware (the USB protocol lands in M5.1) — we just verify the surface:
//!
//! - `mega_get_status` reports `target: "edpro", connected: false`.
//! - emulator-only tools (load_rom, screenshot, save_state, ...) cleanly
//!   return `{ ok:false, reason:"not_supported_on_target" }` instead of
//!   crashing the actor or returning gibberish.

mod common;

use common::McpClient;
use serde_json::{json, Value};

fn parse(resp: &Value, label: &str) -> Value {
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    if text.is_empty() {
        panic!("{label}: empty/missing tool result text. raw = {resp}");
    }
    serde_json::from_str(text).unwrap_or_else(|e| {
        panic!("{label}: not JSON ({e}); text = {text:?}; raw = {resp}")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn edpro_stub_surface() {
    let mut c = McpClient::spawn_with_args(&["--target", "edpro"]);
    c.handshake("edpro-stub").await;

    // Status — must report the EdPro target.
    let resp = c.call(2, "mega_get_status", json!({})).await;
    let v = parse(&resp, "status");
    assert_eq!(v["target"], "edpro", "target must be edpro");
    assert_eq!(v["connected"], false, "connected must be false (stub)");
    assert_eq!(v["rom_loaded"], false);

    // Emulator-only tools must each report `not_supported_on_target` cleanly.
    let blocked = [
        ("mega_load_rom", json!({"path": "/tmp/x.bin"})),
        ("mega_unload_rom", json!({})),
        ("mega_pause", json!({})),
        ("mega_resume", json!({})),
        ("mega_step_frame", json!({})),
        ("mega_step_instruction", json!({})),
        ("mega_read_memory", json!({"space":"ram","addr":0,"length":4})),
        ("mega_get_palettes", json!({})),
        ("mega_dump_tile", json!({"index":0})),
        ("mega_set_breakpoint", json!({"addr":256,"kind":"exec"})),
        ("mega_screenshot", json!({})),
        ("mega_save_state", json!({})),
        ("mega_load_state", json!({})),
    ];
    for (i, (name, args)) in blocked.into_iter().enumerate() {
        let id = 100i64 + i as i64;
        let resp = c.call(id, name, args).await;
        let v = parse(&resp, name);
        assert_eq!(
            v["ok"], false,
            "{name} must return ok:false on edpro target",
        );
        assert_eq!(
            v["reason"], "not_supported_on_target",
            "{name} reason must be not_supported_on_target, got {v}",
        );
    }

    let _ = c.child.kill().await;
}
