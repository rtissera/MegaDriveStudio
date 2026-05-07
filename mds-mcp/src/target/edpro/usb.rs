// SPDX-License-Identifier: MIT
//! USB transport abstraction for the EdPro target.
//!
//! The trait uses `async-trait` so it's dyn-compatible (`EdProTarget`
//! stores its transport as `Box<dyn UsbTransport + Send>`). Native AFIT
//! would require swapping that out for a generic, which would force
//! every tool method into a turbo-fish dance. The boxed-future cost is
//! a single allocation per USB op — irrelevant next to ~2 ms RTT.
//!
//! `MockUsb` is the hardware-free implementation used by every test in this
//! milestone. It records every `write_all` call (one entry per call, not
//! per byte) so golden-frame tests can assert on framing exactly.

// `MockUsb` and the trait are scaffolding for M5.x — every public item is
// intentionally unused outside tests until the real EdProTarget routes
// tools through `proto::*`. `cargo clippy --all-targets -D warnings`
// otherwise flags the whole module.
#![allow(dead_code)]

use async_trait::async_trait;
use std::collections::VecDeque;

/// Minimal byte-stream transport. Errors are flattened to `anyhow::Error`
/// for now; M5.2+ will likely introduce a typed `EdProError` once the real
/// serial backend (serialport-rs / tokio-serial) lands and we need to
/// distinguish `Timeout`, `Disconnected`, `BadFrame`, ...
#[async_trait]
pub trait UsbTransport: Send {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()>;
    async fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()>;
    async fn flush(&mut self) -> anyhow::Result<()>;
}

/// In-memory transport. `rx_queue` holds the bytes the cart "would" send;
/// each `write_all` is appended verbatim to `tx_log` and also stored as a
/// distinct frame in `tx_frames` so tests can assert on per-call boundaries.
#[derive(Debug, Default)]
pub struct MockUsb {
    rx_queue: VecDeque<u8>,
    tx_log: Vec<u8>,
    tx_frames: Vec<Vec<u8>>,
}

impl MockUsb {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with a pre-loaded reply queue. Replies are concatenated in
    /// order; `read_exact` pulls bytes one at a time off the front.
    pub fn with_replies(replies: Vec<Vec<u8>>) -> Self {
        let mut rx_queue = VecDeque::new();
        for r in replies {
            rx_queue.extend(r);
        }
        Self {
            rx_queue,
            ..Self::default()
        }
    }

    /// Append more bytes to the reply queue (handy mid-test).
    pub fn push_reply(&mut self, bytes: &[u8]) {
        self.rx_queue.extend(bytes);
    }

    /// All bytes ever written, concatenated.
    pub fn tx_log(&self) -> &[u8] {
        &self.tx_log
    }

    /// One entry per `write_all` call.
    pub fn tx_frames(&self) -> &[Vec<u8>] {
        &self.tx_frames
    }

    /// Bytes still waiting to be consumed via `read_exact`.
    pub fn rx_remaining(&self) -> usize {
        self.rx_queue.len()
    }
}

#[async_trait]
impl UsbTransport for MockUsb {
    async fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        for slot in buf.iter_mut() {
            *slot = self
                .rx_queue
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("MockUsb: rx_queue underflow"))?;
        }
        Ok(())
    }

    async fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        self.tx_log.extend_from_slice(buf);
        self.tx_frames.push(buf.to_vec());
        Ok(())
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_roundtrip() {
        let mut m = MockUsb::with_replies(vec![vec![0xA5, 0x00], vec![0xDE, 0xAD]]);
        m.write_all(&[1, 2, 3]).await.unwrap();
        m.write_all(&[4, 5]).await.unwrap();

        assert_eq!(m.tx_log(), &[1, 2, 3, 4, 5]);
        assert_eq!(m.tx_frames().len(), 2);
        assert_eq!(m.tx_frames()[0], vec![1, 2, 3]);
        assert_eq!(m.tx_frames()[1], vec![4, 5]);

        let mut buf = [0u8; 4];
        m.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, [0xA5, 0x00, 0xDE, 0xAD]);
        assert_eq!(m.rx_remaining(), 0);

        // Underflow surfaces as Err, not panic.
        let mut more = [0u8; 1];
        assert!(m.read_exact(&mut more).await.is_err());
    }

    #[tokio::test]
    async fn flush_is_noop() {
        let mut m = MockUsb::new();
        m.flush().await.unwrap();
    }
}
