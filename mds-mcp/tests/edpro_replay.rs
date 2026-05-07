// SPDX-License-Identifier: MIT
//! Golden-frame USB replay harness for the Mega Everdrive Pro target.
//!
//! Validates the EdPro wire protocol (USB-WR envelope + framed gdb RSP)
//! without requiring hardware. Each scenario asserts a synthetic pair of
//! byte streams matches what the host-side encoders would emit; once
//! real hardware arrives, recorded captures will replace the synthetic
//! `.bin` files in `tests/fixtures/edpro/` byte-for-byte.
//!
//! ## Why a self-contained RSP codec?
//!
//! `mds-mcp` is a binary-only crate (no `[lib]` in Cargo.toml) so an
//! integration test under `tests/` cannot import from
//! `crate::target::edpro`. We mirror the *output* of the production
//! encoders here: any future protocol drift in `src/target/edpro/rsp.rs`
//! that breaks compatibility with this golden set is surfaced as a
//! `cargo test` failure (the unit tests in `rsp.rs` lock the codec
//! against the same wire format, so any divergence is a real bug).
//!
//! Set `MDS_REGENERATE_FIXTURES=1` to overwrite the committed `.bin`
//! files from the in-test encoders and review the diff before committing.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Local RSP codec (mirrors src/target/edpro/rsp.rs)
// ---------------------------------------------------------------------------

const HEX: &[u8; 16] = b"0123456789abcdef";

fn push_hex_byte(out: &mut Vec<u8>, b: u8) {
    out.push(HEX[(b >> 4) as usize]);
    out.push(HEX[(b & 0x0F) as usize]);
}

fn push_hex_u32(out: &mut Vec<u8>, v: u32) {
    if v == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 8];
    let mut i = 0;
    let mut started = false;
    for shift in (0..8).rev() {
        let nib = ((v >> (shift * 4)) & 0xF) as u8;
        if nib != 0 || started {
            buf[i] = HEX[nib as usize];
            i += 1;
            started = true;
        }
    }
    out.extend_from_slice(&buf[..i]);
}

fn xor_checksum(payload: &[u8]) -> u8 {
    let mut c = 0u8;
    for b in payload {
        c ^= *b;
    }
    c
}

fn escape_payload(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len());
    for &b in payload {
        match b {
            b'#' | b'$' | b'}' | b'*' => {
                out.push(b'}');
                out.push(b ^ 0x20);
            }
            _ => out.push(b),
        }
    }
    out
}

/// Build a framed RSP packet around `payload`.
fn encode_packet(payload: &[u8]) -> Vec<u8> {
    let escaped = escape_payload(payload);
    let csum = xor_checksum(&escaped);
    let mut out = Vec::with_capacity(escaped.len() + 4);
    out.push(b'$');
    out.extend_from_slice(&escaped);
    out.push(b'#');
    push_hex_byte(&mut out, csum);
    out
}

// ---- RSP command builders -------------------------------------------------

fn cmd_query_supported(features: &[&str]) -> Vec<u8> {
    let mut out = Vec::from(b"qSupported".as_slice());
    if !features.is_empty() {
        out.push(b':');
        for (i, f) in features.iter().enumerate() {
            if i > 0 {
                out.push(b';');
            }
            out.extend_from_slice(f.as_bytes());
        }
    }
    out
}

fn cmd_query_start_no_ack_mode() -> Vec<u8> {
    b"QStartNoAckMode".to_vec()
}

fn cmd_read_registers() -> Vec<u8> {
    b"g".to_vec()
}

fn cmd_read_memory(addr: u32, len: u32) -> Vec<u8> {
    let mut out = Vec::from(b"m".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, len);
    out
}

fn cmd_write_memory(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(b"M".as_slice());
    push_hex_u32(&mut out, addr);
    out.push(b',');
    push_hex_u32(&mut out, data.len() as u32);
    out.push(b':');
    for &b in data {
        push_hex_byte(&mut out, b);
    }
    out
}

fn cmd_step(addr: Option<u32>) -> Vec<u8> {
    let mut out = Vec::from(b"s".as_slice());
    if let Some(a) = addr {
        push_hex_u32(&mut out, a);
    }
    out
}

fn cmd_continue(addr: Option<u32>) -> Vec<u8> {
    let mut out = Vec::from(b"c".as_slice());
    if let Some(a) = addr {
        push_hex_u32(&mut out, a);
    }
    out
}

// ---------------------------------------------------------------------------
// EdPro USB-WR envelope (mirrors src/target/edpro/{framing,proto}.rs)
// ---------------------------------------------------------------------------

/// EdPro framing: `'+' ~'+' OP ~OP` -> `[0x2B, 0xD4, op, !op]`.
fn encode_cmd(op: u8) -> [u8; 4] {
    [b'+', !b'+', op, !op]
}

const OP_USB_WR: u8 = 0x22;

/// Wrap `payload` in a USB-WR envelope. Mirrors `proto::usb_write`'s
/// transmitted bytes (4-byte cmd header + u16 BE length + payload).
fn usb_wr(payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= u16::MAX as usize);
    let mut out = Vec::with_capacity(6 + payload.len());
    out.extend_from_slice(&encode_cmd(OP_USB_WR));
    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// Convenience: USB-WR the framed RSP packet for `payload`.
fn host_send_rsp(payload: &[u8]) -> Vec<u8> {
    usb_wr(&encode_packet(payload))
}

/// Convenience: USB-WR a single ack byte (`+`).
fn host_send_ack() -> Vec<u8> {
    usb_wr(b"+")
}

// ---------------------------------------------------------------------------
// Reply builders (rx side — what the cart pushes back)
// ---------------------------------------------------------------------------

/// Cart -> host: optional `+` ack followed by a framed RSP reply.
fn cart_reply(payload: &[u8], ack: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 6);
    if ack {
        v.push(b'+');
    }
    v.extend_from_slice(&encode_packet(payload));
    v
}

// ---------------------------------------------------------------------------
// Scenario assembler
// ---------------------------------------------------------------------------

/// Concatenate the standard handshake exchange onto `tx` / `rx` buffers.
///
/// The handshake is two RSP round-trips, both run with ack-mode ON:
///  1. host: `qSupported:swbreak+;hwbreak+`
///     cart: `+ $PacketSize=400;swbreak+;hwbreak+#xx`
///     host: `+` (ack)
///  2. host: `QStartNoAckMode`
///     cart: `+ $OK#9a`
///     host: `+` (ack — last one before noack flips on)
fn append_handshake(tx: &mut Vec<u8>, rx: &mut Vec<u8>) {
    // 1. qSupported
    tx.extend_from_slice(&host_send_rsp(&cmd_query_supported(&[
        "swbreak+", "hwbreak+",
    ])));
    rx.extend_from_slice(&cart_reply(b"PacketSize=400;swbreak+;hwbreak+", true));
    tx.extend_from_slice(&host_send_ack());
    // 2. QStartNoAckMode
    tx.extend_from_slice(&host_send_rsp(&cmd_query_start_no_ack_mode()));
    rx.extend_from_slice(&cart_reply(b"OK", true));
    tx.extend_from_slice(&host_send_ack());
}

// ---------------------------------------------------------------------------
// Scenario builders — produce (tx, rx) byte pairs.
// ---------------------------------------------------------------------------

fn scenario_handshake() -> (Vec<u8>, Vec<u8>) {
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    (tx, rx)
}

fn scenario_boot_status() -> (Vec<u8>, Vec<u8>) {
    // No extra wire ops: get_status is a synchronous host-side call.
    // The fixture is identical to handshake, but we keep the scenario
    // distinct for the test naming + future hardware recordings.
    scenario_handshake()
}

fn scenario_read_68k_regs() -> (Vec<u8>, Vec<u8>) {
    // 18 BE longs: D0=0x12345678, D1..D7=1..7, A0..A7=0x10..0x17,
    // PS=0x00002700 (SR=0x2700), PC=0x00FF_0000.
    let mut hex = String::new();
    hex.push_str("12345678");
    for i in 1u32..8 {
        hex.push_str(&format!("{i:08x}"));
    }
    for i in 0u32..8 {
        hex.push_str(&format!("{:08x}", 0x10 + i));
    }
    hex.push_str("00002700");
    hex.push_str("00ff0000");

    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    tx.extend_from_slice(&host_send_rsp(&cmd_read_registers()));
    rx.extend_from_slice(&cart_reply(hex.as_bytes(), false)); // noack mode
    (tx, rx)
}

fn scenario_read_memory_short() -> (Vec<u8>, Vec<u8>) {
    // Read 16 bytes from $FF0000 -> canned 0x00..0x0F.
    let mut hex = String::new();
    for i in 0u8..16 {
        hex.push_str(&format!("{i:02x}"));
    }
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    tx.extend_from_slice(&host_send_rsp(&cmd_read_memory(0x00FF_0000, 16)));
    rx.extend_from_slice(&cart_reply(hex.as_bytes(), false));
    (tx, rx)
}

fn scenario_write_memory_with_ack() -> (Vec<u8>, Vec<u8>) {
    // Write [0xCA, 0xFE] to $FF1000.
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    tx.extend_from_slice(&host_send_rsp(&cmd_write_memory(
        0x00FF_1000,
        &[0xCA, 0xFE],
    )));
    rx.extend_from_slice(&cart_reply(b"OK", false));
    (tx, rx)
}

fn scenario_bp_set_clear() -> (Vec<u8>, Vec<u8>) {
    // Set BP at 0x200 (m FF0200,2 -> abcd; M 200,2:4e41 -> OK)
    // Clear BP   (M 200,2:abcd -> OK)
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    // set: read original word
    tx.extend_from_slice(&host_send_rsp(&cmd_read_memory(0x200, 2)));
    rx.extend_from_slice(&cart_reply(b"abcd", false));
    // set: patch in TRAP #1 (0x4E41)
    tx.extend_from_slice(&host_send_rsp(&cmd_write_memory(0x200, &[0x4E, 0x41])));
    rx.extend_from_slice(&cart_reply(b"OK", false));
    // clear: restore saved word
    tx.extend_from_slice(&host_send_rsp(&cmd_write_memory(0x200, &[0xAB, 0xCD])));
    rx.extend_from_slice(&cart_reply(b"OK", false));
    (tx, rx)
}

fn scenario_step_then_stop() -> (Vec<u8>, Vec<u8>) {
    // Single step, cart returns rich stop reply T05swbreak:;
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    tx.extend_from_slice(&host_send_rsp(&cmd_step(None)));
    rx.extend_from_slice(&cart_reply(b"T05swbreak:;", false));
    (tx, rx)
}

fn scenario_resume_fire_and_forget() -> (Vec<u8>, Vec<u8>) {
    // Host sends 'c'; no reply pre-loaded — the host does NOT wait for
    // a stop reply on continue (per M5.4 stub guidance).
    let mut tx = Vec::new();
    let mut rx = Vec::new();
    append_handshake(&mut tx, &mut rx);
    tx.extend_from_slice(&host_send_rsp(&cmd_continue(None)));
    // rx intentionally has no extra bytes after handshake
    (tx, rx)
}

// ---------------------------------------------------------------------------
// Fixture I/O
// ---------------------------------------------------------------------------

fn fixtures_dir() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests");
    p.push("fixtures");
    p.push("edpro");
    p
}

fn regenerate() -> bool {
    std::env::var("MDS_REGENERATE_FIXTURES")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Compare `(tx, rx)` against the committed `.bin` pair under the
/// fixtures directory. If `MDS_REGENERATE_FIXTURES=1` is set the files
/// are (re)written from the in-test encoders before the comparison.
fn check_or_write(name: &str, tx: &[u8], rx: &[u8]) {
    let dir = fixtures_dir();
    let tx_path = dir.join(format!("{name}.tx.bin"));
    let rx_path = dir.join(format!("{name}.rx.bin"));

    if regenerate() {
        std::fs::create_dir_all(&dir).expect("create fixtures dir");
        std::fs::write(&tx_path, tx).expect("write tx fixture");
        std::fs::write(&rx_path, rx).expect("write rx fixture");
        eprintln!("regenerated {tx_path:?} ({} B)", tx.len());
        eprintln!("regenerated {rx_path:?} ({} B)", rx.len());
        return;
    }

    let tx_disk = read_or_panic(&tx_path);
    let rx_disk = read_or_panic(&rx_path);
    assert_eq!(
        tx_disk, tx,
        "tx fixture {name}.tx.bin drifted from encoder. Run \
         MDS_REGENERATE_FIXTURES=1 cargo test --test edpro_replay \
         and review the diff."
    );
    assert_eq!(
        rx_disk, rx,
        "rx fixture {name}.rx.bin drifted from encoder. Run \
         MDS_REGENERATE_FIXTURES=1 cargo test --test edpro_replay \
         and review the diff."
    );
    // Sanity: each scenario stays under the documented 4 KiB / file budget.
    assert!(tx.len() <= 4096, "{name}.tx.bin exceeds 4 KiB budget");
    assert!(rx.len() <= 4096, "{name}.rx.bin exceeds 4 KiB budget");
}

fn read_or_panic(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| {
        panic!(
            "missing fixture {p:?}: {e}. Run \
             MDS_REGENERATE_FIXTURES=1 cargo test --test edpro_replay \
             to bootstrap the synthetic set."
        )
    })
}

// ---------------------------------------------------------------------------
// Per-scenario tests
// ---------------------------------------------------------------------------

#[test]
fn handshake() {
    let (tx, rx) = scenario_handshake();
    // Sanity-check the structure: tx contains 3 USB-WR envelopes
    // (qSupported, ack, QStartNoAckMode, ack — wait, 4). Each USB-WR
    // envelope is `[0x2B, 0xD4, 0x22, 0xDD, hi, lo, ...]`.
    let env_count = tx.windows(4).filter(|w| w == b"\x2B\xD4\x22\xDD").count();
    assert_eq!(env_count, 4, "expected 4 USB-WR envelopes in handshake tx");
    // rx contains exactly two `+`-prefixed RSP frames.
    assert_eq!(rx.iter().filter(|&&b| b == b'$').count(), 2);
    check_or_write("handshake", &tx, &rx);
}

#[test]
fn boot_status() {
    let (tx, rx) = scenario_boot_status();
    // Synchronous tool — no extra wire ops past handshake.
    assert_eq!(scenario_handshake(), (tx.clone(), rx.clone()));
    check_or_write("boot_status", &tx, &rx);
}

#[test]
fn read_68k_regs() {
    let (tx, rx) = scenario_read_68k_regs();
    // The post-handshake tx adds exactly one USB-WR envelope: `$g#67`.
    let needle = host_send_rsp(b"g");
    assert!(tx.windows(needle.len()).any(|w| w == needle));
    // rx ends with a 144-hex-char payload (72 bytes = 18 longs).
    let last_dollar = rx.iter().rposition(|&b| b == b'$').unwrap();
    let last_hash = rx.iter().rposition(|&b| b == b'#').unwrap();
    assert_eq!(last_hash - last_dollar - 1, 144);
    check_or_write("read_68k_regs", &tx, &rx);
}

#[test]
fn read_memory_short() {
    let (tx, rx) = scenario_read_memory_short();
    let needle = host_send_rsp(b"mff0000,10");
    assert!(
        tx.windows(needle.len()).any(|w| w == needle),
        "m packet for FF0000,16 not found in tx"
    );
    check_or_write("read_memory_short", &tx, &rx);
}

#[test]
fn write_memory_with_ack() {
    let (tx, rx) = scenario_write_memory_with_ack();
    let needle = host_send_rsp(b"Mff1000,2:cafe");
    assert!(
        tx.windows(needle.len()).any(|w| w == needle),
        "M packet for FF1000:cafe not found in tx"
    );
    check_or_write("write_memory_with_ack", &tx, &rx);
}

#[test]
fn bp_set_clear() {
    let (tx, rx) = scenario_bp_set_clear();
    let read_orig = host_send_rsp(b"m200,2");
    let patch_trap = host_send_rsp(b"M200,2:4e41");
    let restore = host_send_rsp(b"M200,2:abcd");
    for (label, needle) in [
        ("read original", &read_orig),
        ("patch TRAP #1", &patch_trap),
        ("restore saved word", &restore),
    ] {
        assert!(
            tx.windows(needle.len()).any(|w| w == needle.as_slice()),
            "{label} packet missing from tx"
        );
    }
    // Three round-trip replies after the handshake (each is `$...#xx`,
    // no leading ack since we're in noack mode).
    let post_handshake_dollars = rx.iter().filter(|&&b| b == b'$').count() - 2;
    assert_eq!(post_handshake_dollars, 3);
    check_or_write("bp_set_clear", &tx, &rx);
}

#[test]
fn step_then_stop() {
    let (tx, rx) = scenario_step_then_stop();
    let needle = host_send_rsp(b"s");
    assert!(tx.windows(needle.len()).any(|w| w == needle));
    // Reply payload starts with 'T' for a rich stop reply.
    let last_dollar = rx.iter().rposition(|&b| b == b'$').unwrap();
    assert_eq!(rx[last_dollar + 1], b'T');
    check_or_write("step_then_stop", &tx, &rx);
}

#[test]
fn resume_fire_and_forget() {
    let (tx, rx) = scenario_resume_fire_and_forget();
    let needle = host_send_rsp(b"c");
    assert!(tx.windows(needle.len()).any(|w| w == needle));
    // No post-handshake rx bytes — host doesn't await a stop reply.
    let (_, handshake_rx) = scenario_handshake();
    assert_eq!(rx, handshake_rx, "resume must not pre-load any extra rx");
    check_or_write("resume_fire_and_forget", &tx, &rx);
}

// ---------------------------------------------------------------------------
// Codec self-checks (lock the local codec to known goldens — if these
// drift we know the local mirror has bit-rotted vs. rsp.rs).
// ---------------------------------------------------------------------------

#[test]
fn local_codec_matches_known_goldens() {
    // These are the same goldens covered by rsp.rs unit tests; mirroring
    // them here protects against silent local-mirror drift.
    assert_eq!(encode_packet(b""), b"$#00");
    assert_eq!(encode_packet(b"OK"), b"$OK#04");
    assert_eq!(encode_packet(b"g"), b"$g#67");
    assert_eq!(encode_packet(b"abc"), b"$abc#60");
    // Escapes
    assert_eq!(encode_packet(b"#"), b"$}\x03#7e");
    assert_eq!(encode_packet(b"$"), b"$}\x04#79");
    assert_eq!(encode_packet(b"}"), b"$}\x5d#20");
    assert_eq!(encode_packet(b"*"), b"$}\x0a#77");
    // USB-WR envelope header bytes.
    assert_eq!(&encode_cmd(OP_USB_WR), b"\x2B\xD4\x22\xDD");
    // Hex helpers.
    let mut v = Vec::new();
    push_hex_u32(&mut v, 0xFF8000);
    assert_eq!(v, b"ff8000");
    let mut v = Vec::new();
    push_hex_u32(&mut v, 0);
    assert_eq!(v, b"0");
}
