// SPDX-License-Identifier: MIT
//! mds-mcp — MCP server entry point.
//!
//! Sets up structured logging on stderr (stdout is reserved for the MCP
//! stdio transport — anything written to stdout that is not a JSON-RPC frame
//! breaks the protocol), constructs the [`EmulatorActor`], and serves it over
//! stdio.

use anyhow::Result;
use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::EnvFilter;

mod emulator;
mod ffi;
mod tools;

use tools::MdsServer;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting mds-mcp (stdio transport, M1 scaffold)"
    );

    let server = MdsServer::new();

    // Stub emulator-thread placeholder: in M2 this becomes the real frame-loop
    // worker that owns the libra context.
    tracing::info!("stub: would run emulator thread (libra integration deferred to M2)");

    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!(error = ?e, "stdio serve failed");
    })?;

    service.waiting().await?;
    Ok(())
}
