// SPDX-License-Identifier: MIT
//! M5.6: smoke-test the EdPro target dispatcher. We don't talk to real
//! hardware — the integration test only exercises the *disconnected*
//! `EdProTarget` path because injecting a `MockUsb` requires reaching
//! past the spawned binary into its private state. The "wired against
//! a connected MockUsb returns real results" coverage lives as unit
//! tests inside `src/target/edpro/mod.rs` (M5.5; option (b) per the
//! M5.6 task spec).
//!
//! - `mega_get_status` reports `target: "edpro", connected: false`.
//! - Tools whose backing `EdProTarget` method is wired but needs a live
//!   transport (read_memory, set_breakpoint, step_instruction, ...)
//!   surface `reason: "not_connected"` instead of `not_supported_on_target`.
//! - Tools whose `EdProTarget` method permanently bails (load_rom on
//!   hardware, screenshot, save_state, ...) keep the legacy
//!   `not_supported_on_target` reason.

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

    // Tools whose `EdProTarget` method permanently returns
    // `not_supported_on_target` (these never need a live transport).
    let permanently_unsupported = [
        ("mega_load_rom", json!({"path": "/tmp/x.bin"})),
        ("mega_unload_rom", json!({})),
        ("mega_pause", json!({})),
        ("mega_step_frame", json!({})),
        ("mega_get_palettes", json!({})),
        ("mega_get_sprites", json!({})),
        ("mega_dump_tile", json!({"index":0})),
        ("mega_get_z80_registers", json!({})),
        ("mega_screenshot", json!({})),
        ("mega_save_state", json!({})),
        ("mega_load_state", json!({})),
        ("mega_input_set_state", json!({})),
        ("mega_input_press", json!({"button":"a"})),
        ("mega_input_release", json!({"button":"a"})),
        ("mega_input_get_state", json!({})),
    ];
    for (i, (name, args)) in permanently_unsupported.into_iter().enumerate() {
        let id = 100i64 + i as i64;
        let resp = c.call(id, name, args).await;
        let v = parse(&resp, name);
        assert_eq!(v["ok"], false, "{name} must return ok:false on edpro target");
        assert_eq!(
            v["reason"], "not_supported_on_target",
            "{name} reason must be not_supported_on_target, got {v}",
        );
    }

    // Tools that ARE wired in `EdProTarget` but require a live transport.
    // Disconnected target ⇒ structured `not_connected` reason. These are
    // the M5.5 wired methods exposed via M5.6 dispatcher plumbing.
    let needs_connection = [
        ("mega_resume", json!({})),
        ("mega_continue", json!({})),
        ("mega_step_instruction", json!({})),
        ("mega_read_memory", json!({"space":"ram","addr":0,"length":4})),
        ("mega_write_memory", json!({"space":"ram","addr":0,"data":""})),
        ("mega_set_breakpoint", json!({"addr":256,"kind":"exec"})),
        ("mega_clear_breakpoint", json!({"id":256})),
        ("mega_get_68k_registers", json!({})),
        ("mega_get_vdp_registers", json!({})),
    ];
    for (i, (name, args)) in needs_connection.into_iter().enumerate() {
        let id = 200i64 + i as i64;
        let resp = c.call(id, name, args).await;
        let v = parse(&resp, name);
        assert_eq!(
            v["ok"], false,
            "{name} must return ok:false when EdPro target is disconnected",
        );
        assert_eq!(
            v["reason"], "not_connected",
            "{name} reason must be not_connected on disconnected EdPro target, got {v}",
        );
    }

    // `mega_list_breakpoints` is sync + non-erroring on EdPro — it just
    // returns an empty array, even when disconnected.
    let resp = c.call(300, "mega_list_breakpoints", json!({})).await;
    let v = parse(&resp, "mega_list_breakpoints");
    assert!(
        v["breakpoints"].as_array().map(|a| a.is_empty()).unwrap_or(false),
        "list_breakpoints must return [] on disconnected EdPro, got {v}",
    );

    let _ = c.child.kill().await;
}
