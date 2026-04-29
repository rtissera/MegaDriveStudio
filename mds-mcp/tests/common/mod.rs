// SPDX-License-Identifier: MIT
//! Shared MCP-over-stdio test harness used by smoke / notifications /
//! breakpoints / z80 integration tests.

#![allow(dead_code)]

use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::timeout;

const TARGET_DEFAULT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target");

pub fn resolved_binary() -> String {
    if let Ok(p) = std::env::var("MDS_MCP_BIN") {
        return p;
    }
    let d = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| TARGET_DEFAULT.to_string());
    format!("{d}/release/mds-mcp")
}

pub struct McpClient {
    pub child: Child,
    pub stdin: ChildStdin,
    pub reader: BufReader<ChildStdout>,
}

impl McpClient {
    pub fn spawn() -> Self {
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
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let reader = BufReader::new(stdout);
        Self { child, stdin, reader }
    }

    pub async fn send(&mut self, value: Value) {
        let mut s = serde_json::to_string(&value).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    pub async fn read_until_id(&mut self, target_id: i64) -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            let n = timeout(Duration::from_secs(8), self.reader.read_line(&mut line))
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
            if v.get("id") == Some(&Value::from(target_id)) {
                return v;
            }
        }
    }

    /// Send `initialize` and `notifications/initialized`. `id=1` is consumed.
    pub async fn handshake(&mut self, client_name: &str) -> Value {
        self.send(serde_json::json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2024-11-05","capabilities":{},
                      "clientInfo":{"name":client_name,"version":"0.0.0"}}
        }))
        .await;
        let init = self.read_until_id(1).await;
        self.send(serde_json::json!({
            "jsonrpc":"2.0","method":"notifications/initialized"
        }))
        .await;
        init
    }

    pub async fn call(&mut self, id: i64, name: &str, args: Value) -> Value {
        self.send(serde_json::json!({
            "jsonrpc":"2.0","id":id,"method":"tools/call",
            "params":{"name":name,"arguments":args}
        }))
        .await;
        self.read_until_id(id).await
    }

    pub async fn rpc(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(serde_json::json!({
            "jsonrpc":"2.0","id":id,"method":method,"params":params
        }))
        .await;
        self.read_until_id(id).await
    }
}

/// Parse a `tools/call` response body's first text content as JSON.
pub fn parse_tool_text(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    serde_json::from_str(text).unwrap()
}
