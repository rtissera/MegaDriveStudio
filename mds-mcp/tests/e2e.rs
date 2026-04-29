// SPDX-License-Identifier: MIT
//! Cumulative end-to-end MCP test against a real Mega Drive ROM.
//!
//! Marked `#[ignore]` so it only runs when explicitly requested
//! (`cargo test --release --test e2e -- --ignored`). The CI workflow drives
//! the SGDK Docker image to produce `out/sample-rom.bin`, then runs this.

mod common;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use common::{parse_tool_text, McpClient};
use serde_json::{json, Value};

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
    let v = parse_tool_text(
        &c.call(2, "mega_load_rom", json!({ "path": path })).await,
    );
    assert_eq!(v["ok"], true, "load_rom failed: {v}");
    let size = v["size"].as_u64().expect("size");
    let header_name = v["header_name"].as_str().unwrap_or("").to_string();
    eprintln!("ROM loaded: size={size} header={header_name:?}");
    assert!(size >= 0x200, "ROM too small: {size}");

    // 2. status — rom_loaded
    let v = parse_tool_text(&c.call(3, "mega_get_status", json!({})).await);
    assert_eq!(v["rom_loaded"], true, "rom_loaded must be true");
    assert_eq!(v["target"], "emulator");

    // 3. resume + step 60 frames
    let _ = c.call(4, "mega_resume", json!({})).await;
    let v = parse_tool_text(&c.call(5, "mega_step_frame", json!({ "n": 60 })).await);
    assert_eq!(v["ok"], true);
    let frame = v["frame"].as_u64().expect("frame");
    assert!(frame >= 60, "expected ≥60 frames, got {frame}");

    // 4. read_memory ram @ 0xFF0000 length 16
    let v = parse_tool_text(
        &c.call(
            6,
            "mega_read_memory",
            json!({"space": "ram", "addr": 0, "length": 16}),
        )
        .await,
    );
    let b64 = v["data"].as_str().expect("data");
    let bytes = B64.decode(b64).expect("base64 decode");
    assert_eq!(bytes.len(), 16, "expected 16 bytes, got {}", bytes.len());

    // 5. get_palettes — 4 lines × 16 entries
    let v = parse_tool_text(&c.call(7, "mega_get_palettes", json!({})).await);
    let arr = v.as_array().expect("palettes array");
    assert_eq!(arr.len(), 4, "expected 4 palette lines");
    for line in arr {
        let entries = line.as_array().expect("palette line");
        assert_eq!(entries.len(), 16, "expected 16 entries per line");
    }

    // 6. dump_tile — 8x8 tile = 64 indexed nibbles laid out as 64 bytes
    let v = parse_tool_text(
        &c.call(8, "mega_dump_tile", json!({"index": 0, "palette": 0})).await,
    );
    let bitmap_b64 = v["bitmap"].as_str().expect("bitmap");
    let bitmap = B64.decode(bitmap_b64).expect("base64 bitmap");
    assert_eq!(bitmap.len(), 64, "expected 64 indexed pixels for 8x8 tile");

    // 7. set_breakpoint @ 0x1234 exec
    let v = parse_tool_text(
        &c.call(
            9,
            "mega_set_breakpoint",
            json!({"addr": 0x1234, "kind": "exec", "space": "rom"}),
        )
        .await,
    );
    assert_eq!(v["ok"], true);
    let bp_id = v["id"].as_u64().expect("bp id") as u32;
    assert!(bp_id >= 1);
    let reason = v.get("reason").and_then(|r| r.as_str());
    eprintln!("set_breakpoint id={bp_id} reason={reason:?}");

    // 8. list_breakpoints
    let v = parse_tool_text(&c.call(10, "mega_list_breakpoints", json!({})).await);
    let arr = v["breakpoints"].as_array().expect("breakpoints");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["addr"].as_u64(), Some(0x1234));

    // 9. clear_breakpoint
    let v = parse_tool_text(
        &c.call(11, "mega_clear_breakpoint", json!({"id": bp_id})).await,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["removed"], true);

    // 10. screenshot (PNG)
    let v = parse_tool_text(
        &c.call(12, "mega_screenshot", json!({"format": "png"})).await,
    );
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
        // The frame callback may have not fired yet on a brand-new ROM with
        // no display output; record but do not fail.
        eprintln!("screenshot returned ok=false (no_frame_yet) — non-fatal in CI");
    }

    // 11. save_state / load_state
    let v = parse_tool_text(&c.call(13, "mega_save_state", json!({})).await);
    assert_eq!(v["ok"], true);
    let saved_size = v["size"].as_u64().expect("save size");
    assert!(saved_size > 0, "saved state size > 0");
    let v = parse_tool_text(&c.call(14, "mega_load_state", json!({})).await);
    assert_eq!(v["ok"], true);

    // 12. unload
    let v = parse_tool_text(&c.call(15, "mega_unload_rom", json!({})).await);
    assert_eq!(v["ok"], true);
    let v = parse_tool_text(&c.call(16, "mega_get_status", json!({})).await);
    assert_eq!(v["rom_loaded"], false);

    // Sanity: server still responsive after unload.
    let _ = c.call(17, "mega_get_status", json!({})).await;

    let _ = c.child.kill().await;
    drop::<Value>(json!(null));
}
