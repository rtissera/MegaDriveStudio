// SPDX-License-Identifier: MIT
//! Integration test for `notifications/resources/updated` wiring.
//!
//! Without the libretro core actually running we have no real region
//! changes to detect — but the cfg(not(libra_present)) build still wires
//! up the broadcast channel. This test verifies the negotiation path:
//! initialize → notifications/initialized → resources/subscribe completes
//! cleanly, and the server stays responsive (responds to a follow-up
//! resources/list within budget). Real notification delivery is exercised
//! end-to-end once the patched libretro core is linked.

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

async fn read_until_id(
    reader: &mut BufReader<tokio::process::ChildStdout>,
    target_id: i64,
    seen_methods: &mut Vec<String>,
) -> Value {
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
        let v: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
            seen_methods.push(method.to_string());
        }
        if v.get("id") == Some(&Value::from(target_id)) {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_and_stay_responsive() {
    let bin = resolved_binary();
    assert!(
        std::path::Path::new(&bin).exists(),
        "binary not built at {bin} — run `cargo build --release` first",
    );

    let mut child = Command::new(&bin)
        .arg("--ui-refresh-hz")
        .arg("4")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn mds-mcp");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut seen_methods: Vec<String> = Vec::new();

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "notif-test", "version": "0.0.0"}
            }
        }),
    )
    .await;
    let init = read_until_id(&mut reader, 1, &mut seen_methods).await;
    let caps = &init["result"]["capabilities"];
    assert!(
        caps["resources"]["subscribe"].as_bool().unwrap_or(false),
        "server must advertise resources.subscribe capability"
    );

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await;

    // Subscribe to mega://vram.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "resources/subscribe",
            "params": {"uri": "mega://vram"}
        }),
    )
    .await;
    let sub = read_until_id(&mut reader, 2, &mut seen_methods).await;
    assert!(
        sub.get("error").is_none(),
        "subscribe failed: {sub}"
    );

    // Subscribe to a bogus URI — must error.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/subscribe",
            "params": {"uri": "mega://nonexistent"}
        }),
    )
    .await;
    let bad = read_until_id(&mut reader, 3, &mut seen_methods).await;
    assert!(
        bad.get("error").is_some(),
        "bad subscribe should error, got {bad}"
    );

    // Server still responsive: resources/list returns 7 entries.
    send(
        &mut stdin,
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"resources/list"}),
    )
    .await;
    let list = read_until_id(&mut reader, 4, &mut seen_methods).await;
    let arr = list["result"]["resources"].as_array().expect("resources");
    assert_eq!(arr.len(), 7);

    let _ = child.kill().await;
}
