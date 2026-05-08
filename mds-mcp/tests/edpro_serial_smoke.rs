// SPDX-License-Identifier: MIT
//! Hardware smoke test for the EdPro serial transport (M5.5b).
//!
//! **`#[ignore]` by default** — only runs when the user passes
//! `cargo test -- --ignored` *and* sets `MDS_EDPRO_PORT=/dev/...`. CI
//! never runs it (no hardware in the Linux runner).
//!
//! What it does:
//! - Open the configured port at 9600 baud (CDC ignores it).
//! - Send a single `?` RSP query (the most innocuous gdb stub probe; if
//!   the cart isn't running the stub it'll just time out).
//! - Wait up to 2 s for the framed reply (or any byte at all). Either
//!   outcome is "fine" — the goal is to exercise the open + write +
//!   read path, not to validate the protocol (the StubSync test suite
//!   already does that hardware-free).
//!
//! When the EdPro arrives, run with:
//!
//! ```sh
//! MDS_EDPRO_PORT=/dev/ttyACM0 cargo test -p mds-mcp \
//!     --test edpro_serial_smoke -- --ignored --nocapture
//! ```

#[tokio::test]
#[ignore = "requires hardware: set MDS_EDPRO_PORT and run with --ignored"]
async fn edpro_serial_smoke() {
    // We deliberately avoid making mds-mcp's edpro module a public lib
    // (the crate is bin-only) — instead this test just opens the port
    // through `tokio_serial` directly and writes raw bytes. That keeps
    // the crate boundary clean while still proving the host machine can
    // talk to the cart at all.
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_serial::SerialPortBuilderExt;

    let port = match std::env::var("MDS_EDPRO_PORT") {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!(
                "MDS_EDPRO_PORT unset; skipping (run with `MDS_EDPRO_PORT=/dev/ttyACM0 ...`)"
            );
            return;
        }
    };

    let mut stream = tokio_serial::new(&port, 9600)
        .timeout(Duration::from_secs(1))
        .open_native_async()
        .unwrap_or_else(|e| panic!("failed to open {port}: {e}"));

    // RSP `?` packet: framed as `$?#3f` (checksum of `?` is 0x3f).
    AsyncWriteExt::write_all(&mut stream, b"$?#3f")
        .await
        .expect("write ?");
    AsyncWriteExt::flush(&mut stream).await.expect("flush");

    let mut byte = [0u8; 1];
    let read_fut = AsyncReadExt::read_exact(&mut stream, &mut byte);
    match tokio::time::timeout(Duration::from_secs(2), read_fut).await {
        Ok(Ok(_n)) => eprintln!("got byte 0x{:02x}", byte[0]),
        Ok(Err(e)) => panic!("read failed: {e}"),
        Err(_) => eprintln!("read timed out (cart may not have stub deployed)"),
    }
}
