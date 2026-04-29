// SPDX-License-Identifier: MIT
//! Cumulative end-to-end MCP test against a real Mega Drive ROM.
//!
//! Marked `#[ignore]` so it only runs when explicitly requested
//! (`cargo test --release --test e2e -- --ignored`). The CI workflow drives
//! the SGDK Docker image to produce `out/sample-rom.bin`, then runs this.

mod common;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use common::McpClient;
use serde_json::{json, Value};

/// Like `parse_tool_text` but with a useful error when the text is not JSON.
fn parse(resp: &Value, label: &str) -> Value {
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    if text.is_empty() {
        panic!("{label}: empty/missing tool result text. raw response = {resp}");
    }
    serde_json::from_str(text).unwrap_or_else(|e| {
        panic!("{label}: not JSON ({e}); text = {text:?}; raw = {resp}")
    })
}

fn rom_path() -> String {
    std::env::var("MDS_E2E_ROM").unwrap_or_else(|_| "../out/sample-rom.bin".to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn cumulative_real_rom() {
    let path = rom_path();
    assert!(
        std::path::Path::new(&path).exists(),
        "sample ROM missing at {path}; build it via `make` first or set MDS_E2E_ROM",
    );

    let mut c = McpClient::spawn();
    c.handshake("e2e").await;

    // 1. load_rom
    let resp = c.call(2, "mega_load_rom", json!({ "path": path })).await;
    let v = parse(&resp, "load_rom");
    assert_eq!(v["ok"], true, "load_rom failed: {v}");
    let size = v["size"].as_u64().expect("size");
    let header_name = v["header_name"].as_str().unwrap_or("").to_string();
    eprintln!("ROM loaded: size={size} header={header_name:?}");
    assert!(size >= 0x200, "ROM too small: {size}");

    // 2. status — rom_loaded
    let resp = c.call(3, "mega_get_status", json!({})).await;
    let v = parse(&resp, "status_after_load");
    assert_eq!(v["rom_loaded"], true, "rom_loaded must be true");
    assert_eq!(v["target"], "emulator");

    // 3. resume + step 60 frames
    let _ = c.call(4, "mega_resume", json!({})).await;
    let resp = c.call(5, "mega_step_frame", json!({ "n": 60 })).await;
    let v = parse(&resp, "step_frame");
    assert_eq!(v["ok"], true);
    let frame = v["frame"].as_u64().expect("frame");
    assert!(frame >= 60, "expected ≥60 frames, got {frame}");

    // 4. read_memory ram length 16
    let resp = c
        .call(
            6,
            "mega_read_memory",
            json!({"space": "ram", "addr": 0, "length": 16}),
        )
        .await;
    let v = parse(&resp, "read_memory");
    let b64 = v["data"].as_str().expect("data");
    let bytes = B64.decode(b64).expect("base64 decode");
    assert_eq!(bytes.len(), 16, "expected 16 bytes, got {}", bytes.len());

    // 5. get_palettes — 4 lines × 16 entries
    let resp = c.call(7, "mega_get_palettes", json!({})).await;
    let v = parse(&resp, "get_palettes");
    let arr = v.as_array().expect("palettes array");
    assert_eq!(arr.len(), 4, "expected 4 palette lines");
    for line in arr {
        let entries = line.as_array().expect("palette line");
        assert_eq!(entries.len(), 16, "expected 16 entries per line");
    }

    // 6. dump_tile — 8x8 tile = 64 indexed pixel bytes
    let resp = c
        .call(8, "mega_dump_tile", json!({"index": 0, "palette": 0}))
        .await;
    let v = parse(&resp, "dump_tile");
    let bitmap_b64 = v["bitmap"].as_str().expect("bitmap");
    let bitmap = B64.decode(bitmap_b64).expect("base64 bitmap");
    assert_eq!(bitmap.len(), 64, "expected 64 indexed pixels for 8x8 tile");

    // 7. set_breakpoint @ 0x1234 exec
    let resp = c
        .call(
            9,
            "mega_set_breakpoint",
            json!({"addr": 0x1234, "kind": "exec", "space": "rom"}),
        )
        .await;
    let v = parse(&resp, "set_breakpoint");
    assert_eq!(v["ok"], true);
    let bp_id = v["id"].as_u64().expect("bp id") as u32;
    assert!(bp_id >= 1);
    let reason = v.get("reason").and_then(|r| r.as_str());
    eprintln!("set_breakpoint id={bp_id} reason={reason:?}");

    // 8. list_breakpoints
    let resp = c.call(10, "mega_list_breakpoints", json!({})).await;
    let v = parse(&resp, "list_breakpoints");
    let arr = v["breakpoints"].as_array().expect("breakpoints");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["addr"].as_u64(), Some(0x1234));

    // 9. clear_breakpoint
    let resp = c.call(11, "mega_clear_breakpoint", json!({"id": bp_id})).await;
    let v = parse(&resp, "clear_breakpoint");
    assert_eq!(v["ok"], true);
    assert_eq!(v["removed"], true);

    // 10. screenshot (PNG magic check)
    let resp = c
        .call(12, "mega_screenshot", json!({"format": "png"}))
        .await;
    let v = parse(&resp, "screenshot");
    if v["ok"] == true {
        let data_b64 = v["data"].as_str().expect("png data");
        let png = B64.decode(data_b64).expect("png base64");
        assert!(png.len() >= 8, "PNG too short");
        assert_eq!(
            &png[..8],
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
            "not a PNG",
        );
    } else {
        eprintln!("screenshot ok=false (no_frame_yet) — non-fatal in CI");
    }

    // 11. save_state / load_state
    let resp = c.call(13, "mega_save_state", json!({})).await;
    let v = parse(&resp, "save_state");
    assert_eq!(v["ok"], true);
    let saved_size = v["size"].as_u64().expect("save size");
    assert!(saved_size > 0, "saved state size > 0");
    let resp = c.call(14, "mega_load_state", json!({})).await;
    let v = parse(&resp, "load_state");
    assert_eq!(v["ok"], true);

    // 12. unload + final status
    let resp = c.call(15, "mega_unload_rom", json!({})).await;
    let v = parse(&resp, "unload_rom");
    assert_eq!(v["ok"], true);
    let resp = c.call(16, "mega_get_status", json!({})).await;
    let v = parse(&resp, "status_after_unload");
    assert_eq!(v["rom_loaded"], false);

    // Sanity: still responsive.
    let _ = c.call(17, "mega_get_status", json!({})).await;

    let _ = c.child.kill().await;
}
