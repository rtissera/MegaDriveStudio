// SPDX-License-Identifier: MIT
//
// stub.c — RSP dispatcher entry point. Cleanroom on mborgerson/gdbstub
// (MIT). NO code copied verbatim from gdb's m68k-stub.c (GPLv2).
//
// Flow on exception:
//   vectors.s:__mds_vec_{trace,trap1}
//     -> mds_save_regs   (asm: D0-D7/A0-A7/SR/PC -> mds_regs[18])
//     -> mds_stub_enter_handler(exc_id)   (this file)
//          - if exc_id == TRAP1: pc -= 2; restore original opcode
//          - send T-stop reply ("T05swbreak:;" or "T05;")
//          - rsp_loop(): pump packets until host says `c` or `s`
//          - on `c`: clear T-bit
//          - on `s`: set T-bit
//          - return -> mds_restore_regs -> RTE
//
// Single-step-after-bp protocol is host-driven (rsp.rs stub_sync.rs M5.1
// owns the dance). The stub just executes whatever the host sends.

#include "../include/mds_stub.h"
#include "rsp.h"
#include "usb.h"
#include "bp.h"

#include <stddef.h>
#include <stdint.h>

// Exposed by save_regs.s; layout must match: D0..D7 A0..A6 A7 SR PC = 18 longs.
extern uint32_t mds_regs[18];

// Forward decls of vector entry symbols (from vectors.s).
extern void __mds_vec_trace(void);
extern void __mds_vec_trap1(void);

// Buffer for inbound RSP packet (decoded payload + accumulator).
static uint8_t  rsp_in_raw[RSP_BUF_MAX];   // raw on-wire bytes
static uint8_t  rsp_in_payload[RSP_BUF_MAX];
static uint8_t  rsp_out[RSP_BUF_MAX];

static int g_init_done = 0;
static int g_no_ack    = 0;   // disabled until QStartNoAckMode handshake

// SR T-bit (bit 15 of SR). Setting it traces every instruction.
#define SR_T_BIT 0x8000u

// Exception IDs (must match vectors.s).
#define MDS_EXC_TRACE   9
#define MDS_EXC_TRAP1  33

// ----------------------------------------------------------------------------
// Vector table runtime patch.
// SGDK 2.x relocates the vector table to RAM at $FF0000 (boot/sega.s) so we
// can override Trace + TRAP #1 at startup. On bare hardware without SGDK,
// the user must use link.ld.frag at link time — see README.md.
// ----------------------------------------------------------------------------
#define MDS_VEC_BASE_RAM   0xFF0000u
#define MDS_VEC_TRACE_OFF  (9  * 4)   // $24
#define MDS_VEC_TRAP1_OFF  (33 * 4)   // $84

static void install_vectors(void) {
    volatile uint32_t *vt = (volatile uint32_t *)(uintptr_t)MDS_VEC_BASE_RAM;
    vt[9]  = (uint32_t)(uintptr_t)__mds_vec_trace;
    vt[33] = (uint32_t)(uintptr_t)__mds_vec_trap1;
}

// ----------------------------------------------------------------------------
// USB-framed packet I/O.
// We write raw RSP bytes ($..#xx) directly into the FIFO. The cart MCU's
// USB_WR-envelope-or-passthrough behaviour is the open question §10.Q1; if
// envelope is required, M5.5 wraps the send in a +~+ 0x22 ~0x22 len ... frame.
// For now we keep this layer dumb so the host gdb-proxy can mirror it.
// ----------------------------------------------------------------------------
static void usb_send_buf(const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n; ++i) mds_usb_write_byte(p[i]);
}

// Read one RSP frame off the wire. Skips ack bytes ('+'/'-'). Stores decoded
// payload bytes into `rsp_in_payload[*plen]`. Returns RSP_OK or an error.
// On bad checksum we send '-' and try again.
static rsp_err_t usb_recv_packet(size_t *plen) {
    for (;;) {
        size_t n = 0;
        // Drain pre-`$` bytes (stand-alone acks or stray junk).
        for (;;) {
            uint8_t b = mds_usb_read_byte();
            if (b == '$') {
                rsp_in_raw[0] = b;
                n = 1;
                break;
            }
            // ack from host or noise — discard
        }
        // Accumulate until '#' + 2 csum bytes.
        size_t hash_at = 0;
        while (n < sizeof rsp_in_raw) {
            uint8_t b = mds_usb_read_byte();
            rsp_in_raw[n++] = b;
            if (b == '#') { hash_at = n; break; }
        }
        if (hash_at == 0) return RSP_OVERFLOW;
        if (n + 2 > sizeof rsp_in_raw) return RSP_OVERFLOW;
        rsp_in_raw[n++] = mds_usb_read_byte();
        rsp_in_raw[n++] = mds_usb_read_byte();
        rsp_err_t e = rsp_decode_packet(rsp_in_raw, n,
                                        rsp_in_payload, sizeof rsp_in_payload,
                                        plen);
        if (!g_no_ack) {
            mds_usb_write_byte(e == RSP_OK ? '+' : '-');
        }
        if (e == RSP_OK) return RSP_OK;
        if (e == RSP_BAD_CHECKSUM) continue;   // retransmit cycle
        return e;
    }
}

static void usb_send_payload(const uint8_t *payload, size_t n) {
    size_t out_n = rsp_encode_packet(payload, n, rsp_out, sizeof rsp_out);
    if (out_n == 0) return;   // overflow: caller bug — nothing safe to do
    usb_send_buf(rsp_out, out_n);
}

static void send_ok(void)        { usb_send_payload((const uint8_t *)"OK", 2); }
static void send_empty(void)     { usb_send_payload((const uint8_t *)"", 0); }
static void send_error(uint8_t e) {
    static const char hex[] = "0123456789abcdef";
    uint8_t buf[3];
    buf[0] = 'E';
    buf[1] = (uint8_t)hex[(e >> 4) & 0xF];
    buf[2] = (uint8_t)hex[e & 0xF];
    usb_send_payload(buf, 3);
}

// Stop reply: T<sig>swbreak:; for TRAP #1, T<sig>; for trace.
static void send_stop_reply(uint8_t signal, int swbreak) {
    uint8_t buf[32];
    size_t pos = 0;
    static const char hex[] = "0123456789abcdef";
    buf[pos++] = 'T';
    buf[pos++] = (uint8_t)hex[(signal >> 4) & 0xF];
    buf[pos++] = (uint8_t)hex[signal & 0xF];
    if (swbreak) {
        const char *s = "swbreak:;";
        while (*s) buf[pos++] = (uint8_t)*s++;
    }
    usb_send_payload(buf, pos);
}

// ----------------------------------------------------------------------------
// Packet handlers.
// ----------------------------------------------------------------------------
static void handle_g(void) {
    // 18 regs × 4 bytes × 2 hex chars = 144 chars.
    uint8_t buf[18 * 8];
    size_t pos = 0;
    for (int i = 0; i < 18; ++i) {
        uint32_t v = mds_regs[i];
        rsp_hex_u32(buf, sizeof buf, &pos, v);
    }
    usb_send_payload(buf, pos);
}

static void handle_G(const uint8_t *p, size_t n) {
    if (n != 18 * 8) { send_error(0x01); return; }
    for (int i = 0; i < 18; ++i) {
        uint32_t v;
        size_t idx = (size_t)(i * 8);
        size_t pos = idx;
        if (rsp_parse_hex_u32(p, n, &pos, 0, &v) != RSP_OK ||
            pos != idx + 8) {
            send_error(0x02);
            return;
        }
        mds_regs[i] = v;
    }
    send_ok();
}

static void handle_m(const uint8_t *p, size_t n) {
    // m<addr>,<len>
    size_t pos = 1;
    uint32_t addr, len;
    if (rsp_parse_hex_u32(p, n, &pos, ',', &addr) != RSP_OK) { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                            { send_error(1); return; }
    pos++;
    if (rsp_parse_hex_u32(p, n, &pos, 0, &len) != RSP_OK)     { send_error(1); return; }
    if (len * 2 > sizeof rsp_out - 4)                         { send_error(1); return; }
    uint8_t buf[RSP_BUF_MAX];
    size_t op = 0;
    const volatile uint8_t *src = (const volatile uint8_t *)(uintptr_t)addr;
    for (uint32_t i = 0; i < len; ++i) {
        rsp_hex_byte(buf, sizeof buf, &op, src[i]);
    }
    usb_send_payload(buf, op);
}

static void handle_M(const uint8_t *p, size_t n) {
    // M<addr>,<len>:<hex>
    size_t pos = 1;
    uint32_t addr, len;
    if (rsp_parse_hex_u32(p, n, &pos, ',', &addr) != RSP_OK) { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                            { send_error(1); return; }
    pos++;
    if (rsp_parse_hex_u32(p, n, &pos, ':', &len) != RSP_OK)   { send_error(1); return; }
    if (pos >= n || p[pos] != ':')                            { send_error(1); return; }
    pos++;
    if (n - pos != len * 2)                                   { send_error(1); return; }
    volatile uint8_t *dst = (volatile uint8_t *)(uintptr_t)addr;
    for (uint32_t i = 0; i < len; ++i) {
        uint8_t bytebuf;
        size_t bp = pos + i * 2;
        size_t scratch_pos = bp;
        uint32_t v;
        if (rsp_parse_hex_u32(p, bp + 2, &scratch_pos, 0, &v) != RSP_OK) {
            send_error(2); return;
        }
        bytebuf = (uint8_t)(v & 0xFF);
        dst[i] = bytebuf;
    }
    send_ok();
}

static void handle_Z0(const uint8_t *p, size_t n) {
    // Z0,<addr>,<kind>
    size_t pos = 3;   // skip "Z0,"
    if (n < 5 || p[0] != 'Z' || p[1] != '0' || p[2] != ',') { send_error(1); return; }
    uint32_t addr, kind;
    if (rsp_parse_hex_u32(p, n, &pos, ',', &addr) != RSP_OK) { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                            { send_error(1); return; }
    pos++;
    if (rsp_parse_hex_u32(p, n, &pos, 0, &kind) != RSP_OK)    { send_error(1); return; }
    (void)kind;
    if (mds_bp_set(addr) < 0) { send_error(0x10); return; }
    send_ok();
}

static void handle_z0(const uint8_t *p, size_t n) {
    size_t pos = 3;
    if (n < 5 || p[0] != 'z' || p[1] != '0' || p[2] != ',') { send_error(1); return; }
    uint32_t addr, kind;
    if (rsp_parse_hex_u32(p, n, &pos, ',', &addr) != RSP_OK) { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                            { send_error(1); return; }
    pos++;
    if (rsp_parse_hex_u32(p, n, &pos, 0, &kind) != RSP_OK)    { send_error(1); return; }
    (void)kind;
    if (mds_bp_clear(addr) < 0) { send_error(0x11); return; }
    send_ok();
}

static int payload_eq(const uint8_t *p, size_t n, const char *s) {
    size_t i = 0;
    while (s[i] != 0) {
        if (i >= n) return 0;
        if (p[i] != (uint8_t)s[i]) return 0;
        i++;
    }
    return i == n;
}

static int payload_starts(const uint8_t *p, size_t n, const char *s) {
    size_t i = 0;
    while (s[i] != 0) {
        if (i >= n) return 0;
        if (p[i] != (uint8_t)s[i]) return 0;
        i++;
    }
    return 1;
}

static void handle_q(const uint8_t *p, size_t n) {
    if (payload_starts(p, n, "qSupported")) {
        usb_send_payload((const uint8_t *)"PacketSize=400;swbreak+;qXfer:features:read-",
                         44);
        return;
    }
    if (payload_eq(p, n, "qAttached")) {
        // 1 = attached to existing process. Standard reply for stub-style targets.
        usb_send_payload((const uint8_t *)"1", 1);
        return;
    }
    if (payload_eq(p, n, "qC")) {
        // No thread concept — empty reply.
        send_empty();
        return;
    }
    send_empty();
}

static void handle_Q(const uint8_t *p, size_t n) {
    if (payload_eq(p, n, "QStartNoAckMode")) {
        g_no_ack = 1;
        send_ok();
        return;
    }
    send_empty();
}

// Returns 1 if the host requested resume (continue or step), 0 otherwise.
// On resume: sets/clears T-bit in the saved SR before returning so RTE
// resumes the user program with the right trace state.
static int handle_packet(const uint8_t *p, size_t n) {
    if (n == 0) { send_empty(); return 0; }
    switch (p[0]) {
    case 'g': handle_g();   return 0;
    case 'G': handle_G(p+1, n-1); return 0;
    case 'm': handle_m(p, n);  return 0;
    case 'M': handle_M(p, n);  return 0;
    case 'Z': if (n >= 2 && p[1] == '0') { handle_Z0(p, n); return 0; }
              send_empty(); return 0;
    case 'z': if (n >= 2 && p[1] == '0') { handle_z0(p, n); return 0; }
              send_empty(); return 0;
    case 'c':
        // Continue. Clear T-bit. No immediate reply per gdb spec — the next
        // reply is the stop packet sent when the user program halts again.
        // Optional address arg ignored: gdb on m68k doesn't really use
        // `c<addr>`; if requested we'd update mds_regs[17].
        mds_regs[16] &= ~(uint32_t)SR_T_BIT;
        return 1;
    case 's':
        // Single-step: set T-bit, no immediate reply. The next Trace
        // exception will produce a fresh stop packet.
        mds_regs[16] |= SR_T_BIT;
        return 1;
    case 'q': handle_q(p, n); return 0;
    case 'Q': handle_Q(p, n); return 0;
    case '?':
        // Halt-reason query. Reply with the latest stop signal.
        send_stop_reply(5, 0);
        return 0;
    case 'k':
        // Kill — degrade to "stopped". We can't actually kill on hw.
        send_ok();
        return 0;
    case 'D':
        send_ok();
        return 1;       // detach: resume execution
    default:
        send_empty();
        return 0;
    }
}

// Pump packets until the host issues `c`/`s`/`D`.
static void rsp_loop(void) {
    for (;;) {
        size_t plen = 0;
        rsp_err_t e = usb_recv_packet(&plen);
        if (e != RSP_OK) continue;
        if (handle_packet(rsp_in_payload, plen)) return;
    }
}

// ----------------------------------------------------------------------------
// Public API.
// ----------------------------------------------------------------------------
void mds_stub_init(void) {
    if (g_init_done) return;
    mds_usb_init();
    install_vectors();
    g_init_done = 1;
    // Send a one-byte hello so the host can latch onto the stream.
    // (Outside any RSP frame; gdb-proxy ignores non-`$` bytes.)
    mds_usb_write_byte('!');
}

void mds_stub_enter_handler(uint32_t exc_id) {
    int swbreak = 0;
    if (exc_id == MDS_EXC_TRAP1) {
        // Roll PC back to the trap site and restore original opcode so the
        // host can step or continue cleanly.
        uint32_t pc = mds_regs[17];
        mds_bp_restore_at(pc);
        mds_regs[17] = pc - 2;
        swbreak = 1;
    }
    // Always clear T-bit on entry; handle_packet sets it again on `s`.
    mds_regs[16] &= ~(uint32_t)SR_T_BIT;

    send_stop_reply(5, swbreak);   // SIGTRAP
    rsp_loop();
}
