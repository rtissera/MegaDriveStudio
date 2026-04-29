// SPDX-License-Identifier: MIT
//! Transport wiring: stdio (default) and Streamable-HTTP / SSE over a local
//! TCP port.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rmcp::{
    transport::{
        stdio,
        streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        },
    },
    ServiceExt,
};

use crate::emulator::EmulatorActor;
use crate::notifications::Notifier;
use crate::server::MdsServer;

/// Run the MCP server bound to stdin/stdout. Blocks until the client closes.
pub async fn run_stdio(actor: EmulatorActor, notifier: Notifier) -> Result<()> {
    let server = MdsServer::new(actor.clone(), notifier);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    actor.shutdown();
    Ok(())
}

/// Run the MCP server bound to `127.0.0.1:<port>` as a Streamable-HTTP
/// service mounted at `/mcp`. The bind address is fixed to loopback —
/// exposing the emulator over all interfaces would let any LAN host
/// inject ROM bytes / read RAM.
pub async fn run_http(actor: EmulatorActor, notifier: Notifier, port: u16) -> Result<()> {
    let bind: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local = listener
        .local_addr()
        .map_err(|e| anyhow!("local_addr: {e}"))?;
    tracing::info!(addr = %local, path = "/mcp", "mds-mcp listening (streamable-http)");
    eprintln!("mds-mcp listening on http://{local}/mcp");

    let session_manager = Arc::new(LocalSessionManager::default());
    let actor_for_factory = actor.clone();
    let notifier_for_factory = notifier.clone();
    let service = StreamableHttpService::new(
        move || Ok(MdsServer::new(actor_for_factory.clone(), notifier_for_factory.clone())),
        session_manager,
        StreamableHttpServerConfig::default(),
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    axum::serve(listener, router).await?;
    actor.shutdown();
    Ok(())
}
