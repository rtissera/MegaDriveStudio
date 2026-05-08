// SPDX-License-Identifier: MIT
//
// stub.c — single-file 68k debug stub: GDB Remote Serial Protocol over
// the cart's USB FIFO.
//
// Cleanroom on mborgerson/gdbstub (MIT). NO code copied verbatim from
// gdb's m68k-stub.c (GPLv2).
//
// Deployment model (per fact-check C13/C14/C20/C26 in
// /tmp/mds-edpro-factcheck.md): code lives in cart PSRAM at $300000+;
// data lives in MD work RAM at $FFEE00..$FFEFFF. The host has no PI-bus
// path to MD work RAM, so the stub itself zero-initialises BSS on first
// exception entry.
//
// Why a single file (vs separate rsp.c/bp.c/...): smaller text after
// gcc inlining, fewer call frames, smaller binary. Easy-wins shrink
// also drops:
//   - the BP table (host owns it; stub never sees Z0/z0)
//   - RLE `*N` expansion in decode (host's rsp.rs never emits RLE)
//   - escape encode (stub's outbound payloads contain no #/$/}/* bytes)
//   - the explicit ack-mode state byte (post-handshake all packets are
//     unacked; pre-handshake the host sends `+` which we just ignore as
//     non-`$` bytes during packet sync)
//
// Flow on exception:
//   entry.s:__stub_{trace,trap1}_entry
//     -> mds_save_regs   (asm: D0-D7/A0-A7/SR/PC -> mds_regs[18])
//     -> mds_stub_enter_handler(exc_id)   (this file)
//          - first-call path zero-inits BSS
//          - if exc_id == TRAP1: pc -= 2 (host-owned BP roll-back; the
//            host has already restored the original opcode in PSRAM
//            via RSP `M` before continue/step)
//          - send T-stop reply ("T05swbreak:;" or "T05;")
//          - rsp_loop(): pump packets until host says `c` or `s`
//          - on `c`: clear T-bit. On `s`: set T-bit.
//          - return -> mds_restore_regs -> RTE

#include "usb.h"

#include <stddef.h>
#include <stdint.h>

// Exposed by save_regs.s; layout: D0..D7 A0..A6 A7 SR PC = 18 longs.
extern uint32_t mds_regs[18];

// 190 B is the GDB PacketSize we advertise. We keep one decoded-payload
// buffer in BSS; outbound encoded frames are built on the supervisor
// stack inside `send_packet` (we run in supervisor mode after exception
// entry, with SGDK's stack at $FFFFFE growing down — kilobytes of
// headroom).
#define RSP_BUF_MAX 190

// The decoded-payload buffer (and `mds_regs` in save_regs.s) live in
// `.bss`, mapped by `mdsstub.ld` to MD work RAM at $FFEE00..$FFEFFF.
static uint8_t rsp_payload[RSP_BUF_MAX];

// Latch: did we already zero-init BSS once? Lives in BSS itself, but
// the first-entry sequence reads it BEFORE running the BSS clear, so
// it's effectively a "did the work-RAM contents survive?" coin flip.
// We use a magic "we're initialised" cookie at a known offset within
// our BSS region (`g_init_cookie`) so we can detect cold start vs.
// warm-reset of an already-running stub.
#define INIT_COOKIE 0x4D445300u    // 'MDS\0'
static uint32_t g_init_cookie;

// SR T-bit (bit 15 of SR). Setting it traces every instruction.
#define SR_T_BIT 0x8000u

// Exception IDs (must match entry.s).
#define MDS_EXC_TRACE   9
#define MDS_EXC_TRAP1  33

// M5.9: PSRAM-resident header fields, written by the host. The stub itself
// must reach these via the same PSRAM mapping the 68k uses (the linker
// places the header at PSRAM `$300000` so `__stub_entry` is a link-fixed
// address). The `paused_flag` slot is a u32 at offset 0x10 inside the
// blob — we reach it by indexing off the header. (`original_vbl` at
// offset 0x14 is read directly by the asm thunk in entry.s.)
extern uint32_t __stub_entry[];               // declared in entry.s
#define MDS_HDR_PAUSED_FLAG_INDEX   4         // (0x10 / 4)

// ----------------------------------------------------------------------------
// VDP MMIO ports (M5.7).
// ----------------------------------------------------------------------------
//
// CRAM/VSRAM/VRAM are not directly memory-mapped on the 68k bus: the only
// access path is to write a 32-bit "address-set" command to the VDP control
// port at $C00004, then read words from the data port at $C00000 (each read
// auto-advances the internal VDP address counter).
//
// Address-set encoding (per Plutiedev "VDP Ports" / SGDK `vdp.c`):
//   cmd = ((A & 0x3FFF) << 16) | ((A >> 14) & 0x03) | CD;
// where CD = 0x00 for VRAM_READ, 0x20 for CRAM_READ, 0x10 for VSRAM_READ.
// We write the longword to $C00004 then loop word-reads from $C00000.
//
// VDP registers $00..$17 are WRITE-ONLY on hardware: there is no readback
// path. Hosts that need reg state must either keep a shadow themselves or
// rely on side-channel inference (e.g. user ROM exposing a known mailbox).
// The stub does NOT attempt to fake "read VDP regs" — see qMdsVdpStatus
// for what we *can* return: the VDP status word at $C00004 (read).
#define VDP_DATA_PORT  0x00C00000u
#define VDP_CTRL_PORT  0x00C00004u

#define VDP_CD_VRAM_READ   0x00000000u
#define VDP_CD_CRAM_READ   0x00000020u
#define VDP_CD_VSRAM_READ  0x00000010u

// CRAM = 64 words = 128 bytes. VSRAM = 40 words = 80 bytes.
#define CRAM_BYTES   128u
#define VSRAM_BYTES   80u

// Hard cap on a single qMdsVram response. Larger requests are silently
// truncated to this many bytes (host can chunk further). Cap is well under
// our 190-byte PacketSize advertisement: 128 raw bytes -> 256 hex chars +
// 4 framing bytes = 260; but our outbound encode buffer in send_packet is
// bounded at RSP_BUF_MAX (190). We sidestep that by allocating a
// dedicated stack buffer in the qMdsVram handler (see send_hex_packet_big).
#define VRAM_CHUNK_MAX  128u

// Encode address-set command for a 14-bit VRAM addr / 7-bit CRAM / 6-bit VSRAM.
static uint32_t vdp_addr_cmd(uint16_t addr, uint32_t code) {
    return ((uint32_t)(addr & 0x3FFF) << 16) | (((uint32_t)addr >> 14) & 0x03u) | code;
}

static void vdp_set_addr(uint32_t cmd) {
    *(volatile uint32_t *)VDP_CTRL_PORT = cmd;
}

static uint16_t vdp_read_word(void) {
    return *(volatile uint16_t *)VDP_DATA_PORT;
}

static uint16_t vdp_read_status(void) {
    return *(volatile uint16_t *)VDP_CTRL_PORT;
}

// Read `nwords` words from VDP data port into `out` (big-endian byte order
// preserved — the 68k stores u16 high-byte-first, matching what the host's
// `parse_hex_bytes` expects to round-trip into the wire stream).
static void vdp_read_block(uint8_t *out, uint16_t nwords) {
    for (uint16_t i = 0; i < nwords; ++i) {
        uint16_t w = vdp_read_word();
        out[i * 2 + 0] = (uint8_t)(w >> 8);
        out[i * 2 + 1] = (uint8_t)(w & 0xFF);
    }
}

// ----------------------------------------------------------------------------
// Hex helpers (inlined-by-gcc-because-tiny).
// ----------------------------------------------------------------------------
static const char k_hex[16] = "0123456789abcdef";

static int hex_nibble(uint8_t c, uint8_t *out) {
    if (c >= '0' && c <= '9') { *out = (uint8_t)(c - '0');       return 0; }
    if (c >= 'a' && c <= 'f') { *out = (uint8_t)(c - 'a' + 10);  return 0; }
    if (c >= 'A' && c <= 'F') { *out = (uint8_t)(c - 'A' + 10);  return 0; }
    return -1;
}

static void emit_hex_byte(uint8_t *buf, size_t *pos, uint8_t b) {
    buf[(*pos)++] = (uint8_t)k_hex[(b >> 4) & 0xF];
    buf[(*pos)++] = (uint8_t)k_hex[b & 0xF];
}

static void emit_hex_u32(uint8_t *buf, size_t *pos, uint32_t v) {
    for (int shift = 28; shift >= 0; shift -= 4) {
        buf[(*pos)++] = (uint8_t)k_hex[(v >> shift) & 0xF];
    }
}

// Parse hex chars from `in[*pos..in_len]` until a non-hex char or `term`
// (if term != 0). Updates `*pos` to point at the terminator (or past
// the parsed run). Returns 0 on success, -1 on no-progress.
static int parse_hex_u32(const uint8_t *in, size_t in_len, size_t *pos,
                         uint8_t term, uint32_t *v) {
    uint32_t acc = 0;
    int started = 0;
    while (*pos < in_len) {
        uint8_t c = in[*pos];
        if (term != 0 && c == term) break;
        uint8_t nib;
        if (hex_nibble(c, &nib) < 0) {
            if (!started) return -1;
            break;
        }
        acc = (acc << 4) | nib;
        started = 1;
        (*pos)++;
    }
    if (!started) return -1;
    *v = acc;
    return 0;
}

// ----------------------------------------------------------------------------
// Wire I/O (raw bytes — see USB envelope notes in README.md).
// ----------------------------------------------------------------------------
static void usb_send_buf(const uint8_t *p, size_t n) {
    for (size_t i = 0; i < n; ++i) mds_usb_write_byte(p[i]);
}

// Read one decoded RSP packet into `rsp_payload`. Returns the payload
// length on success, or 0 if the checksum was bad (caller retries).
//
// Drops `+`/`-` and stray bytes outside a `$..#` framed packet. We do
// NOT expand `*` RLE — the host's rsp.rs never emits it (gdb spec
// permits this; `mds-mcp/src/target/edpro/rsp.rs` matches).
static size_t recv_packet(void) {
    for (;;) {
        // Drain pre-`$` bytes (host might send a stray `+` ack early
        // during initial sync).
        for (;;) {
            uint8_t b = mds_usb_read_byte();
            if (b == '$') break;
        }
        size_t op = 0;
        uint8_t csum = 0;
        // Decode escape ON the fly (host MAY send `}`-escaped bytes for
        // literals # $ } * inside a payload). Stop on bare `#`.
        for (;;) {
            uint8_t b = mds_usb_read_byte();
            if (b == '#') break;
            if (op >= RSP_BUF_MAX) {
                // Overflow: drain to '#' and bail.
                while (mds_usb_read_byte() != '#') { /* spin */ }
                // Eat the 2 csum bytes too.
                (void)mds_usb_read_byte();
                (void)mds_usb_read_byte();
                return 0;
            }
            if (b == '}') {
                csum ^= b;
                uint8_t e = mds_usb_read_byte();
                csum ^= e;
                rsp_payload[op++] = (uint8_t)(e ^ 0x20);
            } else {
                csum ^= b;
                rsp_payload[op++] = b;
            }
        }
        uint8_t hi, lo;
        uint8_t c1 = mds_usb_read_byte();
        uint8_t c2 = mds_usb_read_byte();
        if (hex_nibble(c1, &hi) < 0 || hex_nibble(c2, &lo) < 0) return 0;
        uint8_t got = (uint8_t)((hi << 4) | lo);
        if (got != csum) return 0;
        return op;
    }
}

// Encode + send `payload[..n]` as `$payload#xx`. Stub-side payloads
// never contain # $ } * — we skip the escape pass on encode (per
// fact-check / "easy wins": handlers below only ever emit T05*, OK,
// hex digits, and ASCII feature names).
//
// The encoded frame is built on the supervisor stack (kilobytes of
// headroom from SGDK's stack at $FFFFFE) so it doesn't compete with
// `rsp_payload` for our 512-byte BSS budget.
static void send_packet(const uint8_t *payload, size_t n) {
    if (n + 4 > RSP_BUF_MAX) return;   // shouldn't happen by construction
    uint8_t out[RSP_BUF_MAX];
    size_t pos = 0;
    out[pos++] = '$';
    uint8_t csum = 0;
    for (size_t i = 0; i < n; ++i) {
        uint8_t b = payload[i];
        out[pos++] = b;
        csum ^= b;
    }
    out[pos++] = '#';
    out[pos++] = (uint8_t)k_hex[(csum >> 4) & 0xF];
    out[pos++] = (uint8_t)k_hex[csum & 0xF];
    usb_send_buf(out, pos);
}

// M5.7: emit a framed `$<hex>#xx` packet whose payload is the hex
// representation of `nbytes` bytes from `src`. Skips escape (hex digits
// never collide with # $ } *). Buffer lives on the supervisor stack —
// kilobytes free below $FFFFFE.
//
// Worst case (qMdsVram): 128 raw bytes -> 256 hex chars + 4 framing = 260 B.
static void send_hex_framed(const uint8_t *src, size_t nbytes) {
    uint8_t out[VRAM_CHUNK_MAX * 2 + 4];
    if (nbytes * 2 + 4 > sizeof(out)) return;
    size_t pos = 0;
    out[pos++] = '$';
    uint8_t csum = 0;
    for (size_t i = 0; i < nbytes; ++i) {
        uint8_t hi = (uint8_t)k_hex[(src[i] >> 4) & 0xF];
        uint8_t lo = (uint8_t)k_hex[src[i] & 0xF];
        out[pos++] = hi; csum ^= hi;
        out[pos++] = lo; csum ^= lo;
    }
    out[pos++] = '#';
    out[pos++] = (uint8_t)k_hex[(csum >> 4) & 0xF];
    out[pos++] = (uint8_t)k_hex[csum & 0xF];
    usb_send_buf(out, pos);
}

static void send_ok(void) {
    static const uint8_t ok[2] = { 'O', 'K' };
    send_packet(ok, 2);
}
static void send_empty(void) {
    send_packet((const uint8_t *)"", 0);
}
static void send_error(uint8_t e) {
    uint8_t buf[3];
    buf[0] = 'E';
    buf[1] = (uint8_t)k_hex[(e >> 4) & 0xF];
    buf[2] = (uint8_t)k_hex[e & 0xF];
    send_packet(buf, 3);
}

// Stop reply: T<sig>swbreak:; for TRAP #1, T<sig>; for trace.
static void send_stop_reply(uint8_t signal, int swbreak) {
    uint8_t buf[16];
    size_t pos = 0;
    buf[pos++] = 'T';
    emit_hex_byte(buf, &pos, signal);
    if (swbreak) {
        const char *s = "swbreak:;";
        while (*s) buf[pos++] = (uint8_t)*s++;
    }
    send_packet(buf, pos);
}

// ----------------------------------------------------------------------------
// Packet handlers.
// ----------------------------------------------------------------------------
static void handle_g(void) {
    uint8_t buf[18 * 8];
    size_t pos = 0;
    for (int i = 0; i < 18; ++i) {
        emit_hex_u32(buf, &pos, mds_regs[i]);
    }
    send_packet(buf, pos);
}

static void handle_G(const uint8_t *p, size_t n) {
    if (n != 18 * 8) { send_error(0x01); return; }
    for (int i = 0; i < 18; ++i) {
        uint32_t v;
        size_t base = (size_t)(i * 8);
        size_t pos = base;
        if (parse_hex_u32(p, base + 8, &pos, 0, &v) < 0 || pos != base + 8) {
            send_error(0x02);
            return;
        }
        mds_regs[i] = v;
    }
    send_ok();
}

static void handle_m(const uint8_t *p, size_t n) {
    size_t pos = 1;
    uint32_t addr, len;
    if (parse_hex_u32(p, n, &pos, ',', &addr) < 0)         { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                          { send_error(1); return; }
    pos++;
    if (parse_hex_u32(p, n, &pos, 0, &len) < 0)             { send_error(1); return; }
    // Reply needs 2 hex chars per byte; cap conservatively against
    // RSP_BUF_MAX leaving room for `$` + `#xx` (4 bytes framing).
    // Stack-allocated reply buffer (no BSS bloat).
    if (len * 2 > (uint32_t)(RSP_BUF_MAX - 4))              { send_error(1); return; }
    uint8_t buf[RSP_BUF_MAX];
    size_t op = 0;
    const volatile uint8_t *src = (const volatile uint8_t *)(uintptr_t)addr;
    for (uint32_t i = 0; i < len; ++i) {
        emit_hex_byte(buf, &op, src[i]);
    }
    send_packet(buf, op);
}

static void handle_M(const uint8_t *p, size_t n) {
    size_t pos = 1;
    uint32_t addr, len;
    if (parse_hex_u32(p, n, &pos, ',', &addr) < 0)         { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                          { send_error(1); return; }
    pos++;
    if (parse_hex_u32(p, n, &pos, ':', &len) < 0)           { send_error(1); return; }
    if (pos >= n || p[pos] != ':')                          { send_error(1); return; }
    pos++;
    if (n - pos != len * 2)                                 { send_error(1); return; }
    volatile uint8_t *dst = (volatile uint8_t *)(uintptr_t)addr;
    for (uint32_t i = 0; i < len; ++i) {
        uint8_t hi, lo;
        if (hex_nibble(p[pos + i * 2],     &hi) < 0 ||
            hex_nibble(p[pos + i * 2 + 1], &lo) < 0) {
            send_error(2); return;
        }
        dst[i] = (uint8_t)((hi << 4) | lo);
    }
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

// qMdsCram → 128 raw bytes (CRAM, 64 words at addr 0).
static void handle_qmds_cram(void) {
    uint8_t buf[CRAM_BYTES];
    vdp_set_addr(vdp_addr_cmd(0, VDP_CD_CRAM_READ));
    vdp_read_block(buf, CRAM_BYTES / 2);
    send_hex_framed(buf, CRAM_BYTES);
}

// qMdsVsram → 80 raw bytes (VSRAM, 40 words at addr 0).
static void handle_qmds_vsram(void) {
    uint8_t buf[VSRAM_BYTES];
    vdp_set_addr(vdp_addr_cmd(0, VDP_CD_VSRAM_READ));
    vdp_read_block(buf, VSRAM_BYTES / 2);
    send_hex_framed(buf, VSRAM_BYTES);
}

// qMdsVdpStatus → 4 hex digits (one word from $C00004 read = VDP status).
static void handle_qmds_vdp_status(void) {
    uint16_t s = vdp_read_status();
    uint8_t buf[4];
    size_t pos = 0;
    emit_hex_byte(buf, &pos, (uint8_t)(s >> 8));
    emit_hex_byte(buf, &pos, (uint8_t)(s & 0xFF));
    send_packet(buf, pos);
}

// qMdsVram:HEX_ADDR,HEX_LEN → up to VRAM_CHUNK_MAX bytes from VRAM.
// Larger requests are truncated (no error). Odd `len` is rounded up to
// the next even byte (VDP reads are word-granular).
static void handle_qmds_vram(const uint8_t *p, size_t n) {
    // p starts at the ':' separator (caller advanced past "qMdsVram").
    size_t pos = 0;
    if (pos >= n || p[pos] != ':') { send_error(1); return; }
    pos++;
    uint32_t addr, len;
    if (parse_hex_u32(p, n, &pos, ',', &addr) < 0)  { send_error(1); return; }
    if (pos >= n || p[pos] != ',')                   { send_error(1); return; }
    pos++;
    if (parse_hex_u32(p, n, &pos, 0, &len) < 0)      { send_error(1); return; }
    // Truncate (rather than error) on oversize — host can chunk further.
    if (len > VRAM_CHUNK_MAX) len = VRAM_CHUNK_MAX;
    // Round up to even (word-aligned).
    uint32_t nwords = (len + 1u) >> 1;
    uint8_t buf[VRAM_CHUNK_MAX];
    vdp_set_addr(vdp_addr_cmd((uint16_t)(addr & 0xFFFF), VDP_CD_VRAM_READ));
    vdp_read_block(buf, (uint16_t)nwords);
    send_hex_framed(buf, (size_t)(nwords * 2));
}

static void handle_q(const uint8_t *p, size_t n) {
    if (payload_starts(p, n, "qSupported")) {
        const char *r = "PacketSize=190;swbreak+;qXfer:features:read-";
        size_t rn = 0; while (r[rn]) rn++;
        send_packet((const uint8_t *)r, rn);
        return;
    }
    if (payload_eq(p, n, "qAttached")) {
        send_packet((const uint8_t *)"1", 1);
        return;
    }
    // M5.7 custom VDP queries. Naming convention: "qMds<Name>" with
    // optional ":args". Hex-encoded bodies use `parse_hex_bytes`-compatible
    // lowercase pairs (matches host's existing decoder).
    if (payload_eq(p, n, "qMdsCram"))      { handle_qmds_cram();      return; }
    if (payload_eq(p, n, "qMdsVsram"))     { handle_qmds_vsram();     return; }
    if (payload_eq(p, n, "qMdsVdpStatus")) { handle_qmds_vdp_status(); return; }
    if (payload_starts(p, n, "qMdsVram")) {
        // Skip "qMdsVram" (8 bytes) and pass the rest (starts with ':').
        handle_qmds_vram(p + 8, n - 8);
        return;
    }
    send_empty();
}

static void handle_Q(const uint8_t *p, size_t n) {
    if (payload_eq(p, n, "QStartNoAckMode")) {
        // We never sent acks in the first place (handshake-time `+` from
        // the host gets swallowed as non-`$` bytes during sync). Just
        // reply OK and the host will flip its ack-state.
        send_ok();
        return;
    }
    send_empty();
}

// Returns 1 if the host requested resume (continue or step), 0 otherwise.
static int handle_packet(const uint8_t *p, size_t n) {
    if (n == 0) { send_empty(); return 0; }
    switch (p[0]) {
    case 'g': handle_g();        return 0;
    case 'G': handle_G(p+1, n-1); return 0;
    case 'm': handle_m(p, n);    return 0;
    case 'M': handle_M(p, n);    return 0;
    case 'c':
        mds_regs[16] &= ~(uint32_t)SR_T_BIT;
        return 1;
    case 's':
        mds_regs[16] |= SR_T_BIT;
        return 1;
    case 'q': handle_q(p, n);    return 0;
    case 'Q': handle_Q(p, n);    return 0;
    case '?':
        send_stop_reply(5, 0);
        return 0;
    case 'k':
        send_ok();
        return 0;
    case 'D':
        send_ok();
        return 1;
    default:
        // Unknown packet — including Z0/z0 (host owns BP table; stub
        // never sees those). Reply empty per gdb spec.
        send_empty();
        return 0;
    }
}

static void rsp_loop(void) {
    for (;;) {
        size_t plen = recv_packet();
        if (plen == 0) continue;   // bad checksum, retry
        if (handle_packet(rsp_payload, plen)) return;
    }
}

// ----------------------------------------------------------------------------
// First-call BSS zero-init.
// ----------------------------------------------------------------------------
//
// The host has no PI-bus path to MD work RAM (per fact-check C13), so it
// can't pre-zero our BSS at upload time. We zero BSS once on the first
// exception entry, using the `__bss_start`/`__bss_end` symbols from the
// linker script.

extern uint8_t __bss_start[];
extern uint8_t __bss_end[];

static void bss_init_if_needed(void) {
    if (g_init_cookie == INIT_COOKIE) return;
    for (uint8_t *p = __bss_start; p < __bss_end; ++p) {
        *p = 0;
    }
    g_init_cookie = INIT_COOKIE;
}

// ----------------------------------------------------------------------------
// Public entry from entry.s.
// Called once per exception. exc_id = MDS_EXC_TRACE or MDS_EXC_TRAP1.
// On return, entry.s does mds_restore_regs + RTE.
// ----------------------------------------------------------------------------
void mds_stub_enter_handler(uint32_t exc_id) {
    bss_init_if_needed();

    int swbreak = 0;
    if (exc_id == MDS_EXC_TRAP1) {
        // PC points just past the trap word; roll PC back to the BP
        // address so the host sees a fresh halt at the original site.
        // The HOST is responsible for restoring the original opcode in
        // PSRAM via RSP `M` before issuing `c` or `s`.
        mds_regs[17] -= 2;
        swbreak = 1;
    }
    // Always clear T-bit on entry; handle_packet sets it again on `s`.
    mds_regs[16] &= ~(uint32_t)SR_T_BIT;

    send_stop_reply(5, swbreak);   // SIGTRAP
    rsp_loop();
}

// ----------------------------------------------------------------------------
// M5.9: VBL hijack pause handler.
// ----------------------------------------------------------------------------
//
// Entry from `__stub_vbl_entry` (entry.s). The asm thunk handles the
// fast-path `paused_flag == 0` case inline (chain to the user's VBL
// handler) so we never even enter C. The slow path (paused) calls into
// `mds_stub_pause_handler` AFTER `mds_save_regs` has dumped the user's
// register state into `mds_regs[]`. On return, the asm thunk runs
// `mds_restore_regs` and RTEs.
//
// Re-entrancy: if a second VBL fires before the host clears the flag
// AND restores vector $78, we re-enter. Host design (stub_blob::unpause
// clears flag FIRST, then restores vector $78) means even a racing VBL
// sees the flag at 0 and chains to the original handler — no spurious
// pause loop.
void mds_stub_pause_handler(void) {
    bss_init_if_needed();
    // Always clear T-bit on entry; handle_packet sets it again on `s`.
    mds_regs[16] &= ~(uint32_t)SR_T_BIT;
    send_stop_reply(5, 0);   // T05 — generic SIGTRAP
    rsp_loop();
    // Resume requested. Clear paused_flag so subsequent VBLs (if any
    // arrive before the host's vector restore propagates) chain straight
    // through.
    __stub_entry[MDS_HDR_PAUSED_FLAG_INDEX] = 0;
}

