// SPDX-License-Identifier: MIT
//! mds-mcp — Megadrive Studio MCP server.
//!
//! Launches an emulator worker thread (owns the libra context, drives the
//! 60 fps frame loop) and serves MCP over stdio (default) or Streamable-HTTP
//! on a loopback port (`--http <port>`).

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod emulator;
mod ffi;
mod notifications;
mod resources;
mod server;
mod tools;
mod transport;

use emulator::EmulatorActor;
use notifications::Notifier;

/// Megadrive Studio MCP server.
#[derive(Debug, Parser)]
#[command(name = "mds-mcp", version, about, long_about = None)]
struct Cli {
    /// Use stdio transport (default).
    #[arg(long, conflicts_with = "http")]
    stdio: bool,

    /// Listen on http://127.0.0.1:<port>/mcp instead of stdio. Always binds
    /// loopback only.
    #[arg(long, value_name = "PORT")]
    http: Option<u16>,

    /// Alias for `--http` provided for symmetry with the spec ("SSE" is the
    /// older MCP transport name; rmcp 1.5 ships Streamable-HTTP, which
    /// negotiates SSE on demand).
    #[arg(long, value_name = "PORT", conflicts_with = "http")]
    sse: Option<u16>,

    /// Path to the libretro core shared library. Defaults to
    /// `vendor/clownmdemu-libretro/clownmdemu_libretro.so` relative to CWD.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "vendor/clownmdemu-libretro/clownmdemu_libretro.so"
    )]
    core: PathBuf,

    /// UI refresh rate cap in Hz for `notifications/resources/updated`
    /// emission per URI (range 1..=30; default 4).
    #[arg(long, value_name = "HZ", default_value_t = 4)]
    ui_refresh_hz: u32,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let cli = Cli::parse();
    let port = cli.http.or(cli.sse);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        libra_present = cfg!(libra_present),
        transport = if port.is_some() { "http" } else { "stdio" },
        core = %cli.core.display(),
        "mds-mcp starting"
    );

    let actor = EmulatorActor::spawn(cli.core.clone());
    let notifier = Notifier::new(notifications::min_interval_for_hz(cli.ui_refresh_hz));
    notifier.spawn(&actor);

    match port {
        Some(p) => transport::run_http(actor, notifier, p).await?,
        None => transport::run_stdio(actor, notifier).await?,
    }
    Ok(())
}
