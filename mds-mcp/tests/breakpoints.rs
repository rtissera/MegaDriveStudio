// SPDX-License-Identifier: MIT
//! Black-box integration test for the breakpoint set→hit→clear→list cycle.
//!
//! Without the libretro core actually running we cannot fire the on-CPU
//! breakpoint callback for real, so we drive set / list / clear over MCP and
//! assert the data model is correct. End-to-end "callback fires → halt →
//! broadcast" is covered once agent A's patched libretro core ships.

mod common;

use common::{parse_tool_text, McpClient};
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn breakpoint_lifecycle() {
    let mut c = McpClient::spawn();
    c.handshake("bp-test").await;

    // empty list
    let v = parse_tool_text(&c.call(2, "mega_list_breakpoints", json!({})).await);
    assert_eq!(v["breakpoints"].as_array().map(|a| a.len()), Some(0));

    // set exec BP at 0x100
    let v = parse_tool_text(
        &c.call(
            3,
            "mega_set_breakpoint",
            json!({"addr": 256, "kind": "exec", "space": "rom"}),
        )
        .await,
    );
    assert_eq!(v["ok"], true);
    let id1 = v["id"].as_u64().expect("id1") as u32;
    let reason1 = v.get("reason").and_then(|r| r.as_str()).map(String::from);

    // set write watchpoint at 0xFF8000
    let v = parse_tool_text(
        &c.call(
            4,
            "mega_set_breakpoint",
            json!({"addr": 16744448u64, "kind": "write", "space": "ram"}),
        )
        .await,
    );
    let id2 = v["id"].as_u64().expect("id2") as u32;
    assert_ne!(id1, id2, "ids must be unique");

    // list — expect both
    let v = parse_tool_text(&c.call(5, "mega_list_breakpoints", json!({})).await);
    let arr = v["breakpoints"].as_array().expect("breakpoints");
    assert_eq!(arr.len(), 2);
    let ids: Vec<u32> = arr.iter().map(|b| b["id"].as_u64().unwrap() as u32).collect();
    assert!(ids.contains(&id1) && ids.contains(&id2));
    let kinds: Vec<&str> = arr.iter().map(|b| b["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"exec") && kinds.contains(&"write"));

    // clear id1
    let v = parse_tool_text(
        &c.call(6, "mega_clear_breakpoint", json!({"id": id1})).await,
    );
    assert_eq!(v["ok"], true);
    assert_eq!(v["removed"], true);

    // list — expect only id2
    let v = parse_tool_text(&c.call(7, "mega_list_breakpoints", json!({})).await);
    let arr = v["breakpoints"].as_array().expect("breakpoints");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_u64().unwrap() as u32, id2);

    // clear nonexistent id — removed=false, no error.
    let v = parse_tool_text(
        &c.call(8, "mega_clear_breakpoint", json!({"id": 9999})).await,
    );
    assert_eq!(v["removed"], false);

    // unknown kind — error.
    let resp = c
        .call(9, "mega_set_breakpoint", json!({"addr": 0, "kind": "wat"}))
        .await;
    assert!(
        resp["result"]["isError"].as_bool().unwrap_or(false),
        "expected isError on bogus kind, got {resp}"
    );

    // mega://breakpoints resource — read returns the list.
    let resp = c
        .rpc(10, "resources/read", json!({"uri":"mega://breakpoints"}))
        .await;
    let text = resp["result"]["contents"][0]["text"].as_str().expect("text");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed.as_array().map(|a| a.len()), Some(1));

    // On the cfg(not(libra_present)) build path the reason must say
    // "debug_api_unavailable". Skip when the patched core is wired in.
    if cfg!(not(libra_present)) {
        assert_eq!(reason1.as_deref(), Some("debug_api_unavailable"));
    }

    let _ = c.child.kill().await;
}
