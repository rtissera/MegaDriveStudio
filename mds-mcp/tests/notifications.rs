// SPDX-License-Identifier: MIT
//! Integration test for `notifications/resources/updated` wiring.
//!
//! Without the libretro core actually running we have no real region changes
//! to detect — but the cfg(not(libra_present)) build still wires up the
//! broadcast channel. This test verifies the negotiation path:
//! initialize → notifications/initialized → resources/subscribe completes
//! cleanly, and the server stays responsive.

mod common;

use common::McpClient;
use serde_json::{json, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_and_stay_responsive() {
    let mut c = McpClient::spawn();
    let init = c.handshake("notif-test").await;
    let caps = &init["result"]["capabilities"];
    assert!(
        caps["resources"]["subscribe"].as_bool().unwrap_or(false),
        "server must advertise resources.subscribe"
    );

    // Subscribe to mega://vram.
    let sub = c
        .rpc(2, "resources/subscribe", json!({"uri": "mega://vram"}))
        .await;
    assert!(sub.get("error").is_none(), "subscribe failed: {sub}");

    // Subscribe to a bogus URI — must error.
    let bad = c
        .rpc(3, "resources/subscribe", json!({"uri": "mega://nonexistent"}))
        .await;
    assert!(bad.get("error").is_some(), "bad subscribe should error, got {bad}");

    // Server still responsive: resources/list returns 9 entries.
    let list: Value = c.rpc(4, "resources/list", json!({})).await;
    let arr = list["result"]["resources"].as_array().expect("resources");
    assert_eq!(arr.len(), 9);

    let _ = c.child.kill().await;
}
