// SPDX-License-Identifier: MIT
//! Real serial-port-backed [`UsbTransport`] for the EdPro target (M5.5b).
//!
//! # Crate choice
//!
//! We use [`tokio-serial`] (5.4.x, MIT-licensed). It wraps the underlying
//! `serialport` crate's TTYPort/COMPort and ships native
//! `tokio::io::AsyncRead` + `AsyncWrite` impls on `SerialStream`, which
//! plugs straight into our async [`UsbTransport`] trait without
//! `spawn_blocking` plumbing or extra channels. Alternatives considered:
//!
//! - `serialport` (sync) + `tokio::task::spawn_blocking` — would force one
//!   blocking-task hop per `read`/`write`, adding latency on every byte.
//! - hand-rolled `std::fs::File` + termios — Linux-only and silently
//!   wrong on macOS / Windows.
//!
//! # Baud rate
//!
//! The EdPro USB endpoint is a USB-CDC class device; CDC ignores the
//! baud rate (it is delivered to the device as a control transfer the
//! firmware throws away). We mirror `ricky26/megalink-rs`
//! (`serialport::new(path, 9600).open()`, see `src/bin/megalink.rs` in
//! that repo) so a single configuration works for both tools out of the
//! box. The value is exposed via [`crate::target::EdProConfig::baud`]
//! so a future RS-232 transport (e.g. a custom serial-only stub for ED
//! X7) can override it.
//!
//! # Lifecycle
//!
//! [`SerialUsb::open`] resolves the port (returning a useful error on
//! `ENOENT` / `EACCES`) then constructs a `tokio_serial::SerialStream`
//! with 8N1 framing and a 1 s read timeout (matching megalink-rs's
//! steady-state value). The stream is owned outright; `Drop` closes the
//! handle automatically.

use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{SerialPortBuilderExt, SerialStream};

use super::usb::UsbTransport;

/// Real-hardware [`UsbTransport`]. Owns a `tokio_serial::SerialStream`.
///
/// Reads and writes go straight through `AsyncReadExt::read_exact` /
/// `AsyncWriteExt::write_all` — no extra buffering layer. `flush()` calls
/// the underlying TTY flush (Linux: `tcflush(TCOFLUSH)` style — see
/// `tokio-serial` docs).
pub struct SerialUsb {
    stream: SerialStream,
    port: String,
}

impl SerialUsb {
    /// Open `port` at `baud`, 8N1, no flow control, 1 s read timeout.
    ///
    /// Errors map onto our existing string-based `anyhow::Error` flow;
    /// the message is pre-tagged so the IDE can render a sensible hint:
    /// - `ENOENT` → `"port not found: …"`
    /// - `EACCES` → `"permission denied opening …; on Linux add your
    ///   user to the `dialout` group"`
    /// - anything else falls through verbatim from `tokio_serial`.
    pub async fn open(port: &str, baud: u32) -> anyhow::Result<Self> {
        // `tokio_serial::new` is sync (just builds the descriptor);
        // `open_native_async` is the call that actually opens the device.
        // We don't want the 1 s+ stall on a missing port to block the
        // runtime, so even though tokio-serial's open is implemented as
        // a blocking syscall we keep this on the same task: it returns
        // immediately on ENOENT / EACCES, and on success the cost is
        // dwarfed by the first round-trip with the cart.
        let builder = tokio_serial::new(port, baud)
            .data_bits(tokio_serial::DataBits::Eight)
            .stop_bits(tokio_serial::StopBits::One)
            .parity(tokio_serial::Parity::None)
            .flow_control(tokio_serial::FlowControl::None)
            .timeout(Duration::from_secs(1));

        let stream = builder
            .open_native_async()
            .map_err(|e| map_open_err(port, e))?;
        Ok(Self {
            stream,
            port: port.to_string(),
        })
    }

    /// Borrow the path the transport was opened with. Useful for
    /// surfacing the port in `get_status` after the cfg has been moved.
    #[allow(dead_code)]
    pub fn port(&self) -> &str {
        &self.port
    }
}

/// Translate a `tokio_serial::Error` into a human-friendly `anyhow::Error`
/// that surfaces the *actual* cause and includes the port name.
fn map_open_err(port: &str, e: tokio_serial::Error) -> anyhow::Error {
    use tokio_serial::ErrorKind;
    match e.kind() {
        ErrorKind::NoDevice => anyhow::anyhow!("port not found: {port}"),
        ErrorKind::Io(io_kind) => match io_kind {
            std::io::ErrorKind::NotFound => anyhow::anyhow!("port not found: {port}"),
            std::io::ErrorKind::PermissionDenied => anyhow::anyhow!(
                "permission denied opening {port}; on Linux add your user to the `dialout` group, then re-login"
            ),
            _ => anyhow::anyhow!("failed to open {port}: {e}"),
        },
        _ => anyhow::anyhow!("failed to open {port}: {e}"),
    }
}

#[async_trait]
impl UsbTransport for SerialUsb {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        // `AsyncReadExt::read_exact` returns `io::Result<usize>`; map any
        // io error onto a port-tagged anyhow error so callers can
        // distinguish "this port" from other transports in a multi-target
        // setup.
        AsyncReadExt::read_exact(&mut self.stream, buf)
            .await
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("serial read_exact failed on {}: {e}", self.port))
    }

    async fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        AsyncWriteExt::write_all(&mut self.stream, buf)
            .await
            .map_err(|e| anyhow::anyhow!("serial write_all failed on {}: {e}", self.port))
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        AsyncWriteExt::flush(&mut self.stream)
            .await
            .map_err(|e| anyhow::anyhow!("serial flush failed on {}: {e}", self.port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opening a port that demonstrably does not exist must yield a
    /// clear, port-tagged error — not a generic IO error or a panic.
    #[tokio::test]
    async fn open_nonexistent_port_errors_clearly() {
        let path = "/tmp/mds_edpro_test_nonexistent_port_xyz_42";
        // `unwrap_err()` would require `SerialUsb: Debug` — keep it
        // out of the public API for now and pattern-match instead.
        let err = match SerialUsb::open(path, 9600).await {
            Ok(_) => panic!("opening nonexistent port {path} unexpectedly succeeded"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains(path),
            "error should mention the port path: {err}"
        );
        assert!(
            err.to_ascii_lowercase().contains("not found")
                || err.to_ascii_lowercase().contains("no such")
                || err.to_ascii_lowercase().contains("failed to open"),
            "error should indicate the port is missing: {err}"
        );
    }

    /// `/dev/null` is openable as a file but isn't a serial port. Behaviour
    /// here is platform-dependent: on Linux `tokio-serial` accepts it
    /// (termios calls succeed on a chardev-with-tty stub) and only fails
    /// once we try to actually read; on macOS the `tcgetattr` call
    /// fails at open time. We just assert that *if* it errors, it errors
    /// cleanly — and document the platform variance for future readers.
    #[cfg(unix)]
    #[tokio::test]
    async fn open_devnull_is_documented_platform_dependent() {
        let res = SerialUsb::open("/dev/null", 9600).await;
        match res {
            // Linux historically opens it without error.
            Ok(_) => {}
            // macOS / stricter setups reject — error must be clean text.
            Err(e) => {
                let s = e.to_string();
                assert!(s.contains("/dev/null"), "error should tag the port: {s}");
            }
        }
    }
}
