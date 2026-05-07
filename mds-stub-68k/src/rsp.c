// SPDX-License-Identifier: MIT
//
// rsp.c — GDB RSP codec. Wire-compatible w/ mds-mcp/src/target/edpro/rsp.rs.
// Cleanroom on mborgerson/gdbstub (MIT). No malloc, no globals.

#include "rsp.h"

static const char k_hex[16] = "0123456789abcdef";

static int hex_nibble(uint8_t c, uint8_t *out) {
    if (c >= '0' && c <= '9') { *out = (uint8_t)(c - '0');       return 0; }
    if (c >= 'a' && c <= 'f') { *out = (uint8_t)(c - 'a' + 10);  return 0; }
    if (c >= 'A' && c <= 'F') { *out = (uint8_t)(c - 'A' + 10);  return 0; }
    return -1;
}

int rsp_hex_byte(uint8_t *out, size_t cap, size_t *pos, uint8_t b) {
    if (*pos + 2 > cap) return -1;
    out[(*pos)++] = (uint8_t)k_hex[(b >> 4) & 0xF];
    out[(*pos)++] = (uint8_t)k_hex[b & 0xF];
    return 0;
}

int rsp_hex_u32(uint8_t *out, size_t cap, size_t *pos, uint32_t v) {
    if (*pos + 8 > cap) return -1;
    for (int shift = 28; shift >= 0; shift -= 4) {
        out[(*pos)++] = (uint8_t)k_hex[(v >> shift) & 0xF];
    }
    return 0;
}

rsp_err_t rsp_parse_hex_bytes(const uint8_t *in, size_t in_len,
                              uint8_t *out, size_t cap, size_t *n_out) {
    if ((in_len & 1) != 0) return RSP_BAD_HEX;
    size_t n = in_len / 2;
    if (n > cap) return RSP_OVERFLOW;
    for (size_t i = 0; i < n; ++i) {
        uint8_t hi, lo;
        if (hex_nibble(in[2 * i],     &hi) < 0) return RSP_BAD_HEX;
        if (hex_nibble(in[2 * i + 1], &lo) < 0) return RSP_BAD_HEX;
        out[i] = (uint8_t)((hi << 4) | lo);
    }
    *n_out = n;
    return RSP_OK;
}

rsp_err_t rsp_parse_hex_u32(const uint8_t *in, size_t in_len, size_t *pos,
                            uint8_t term, uint32_t *v) {
    uint32_t acc = 0;
    int started = 0;
    while (*pos < in_len) {
        uint8_t c = in[*pos];
        if (term != 0 && c == term) break;
        uint8_t nib;
        if (hex_nibble(c, &nib) < 0) {
            if (!started) return RSP_BAD_HEX;
            break;
        }
        acc = (acc << 4) | nib;
        started = 1;
        (*pos)++;
    }
    if (!started) return RSP_BAD_HEX;
    *v = acc;
    return RSP_OK;
}

static uint8_t xor_checksum(const uint8_t *p, size_t n) {
    uint8_t c = 0;
    for (size_t i = 0; i < n; ++i) c ^= p[i];
    return c;
}

size_t rsp_encode_packet(const uint8_t *payload, size_t plen,
                         uint8_t *out, size_t cap) {
    // We escape inline while computing csum on the *escaped* bytes (matches
    // rsp.rs: csum is over already-escaped payload).
    if (cap < 4) return 0;          // need at least "$#xx"
    size_t pos = 0;
    out[pos++] = '$';
    uint8_t csum = 0;
    for (size_t i = 0; i < plen; ++i) {
        uint8_t b = payload[i];
        if (b == '#' || b == '$' || b == '}' || b == '*') {
            if (pos + 2 > cap - 3) return 0;  // reserve for "#xx"
            out[pos++] = '}';
            csum ^= '}';
            uint8_t e = (uint8_t)(b ^ 0x20);
            out[pos++] = e;
            csum ^= e;
        } else {
            if (pos + 1 > cap - 3) return 0;
            out[pos++] = b;
            csum ^= b;
        }
    }
    if (pos + 3 > cap) return 0;
    out[pos++] = '#';
    out[pos++] = (uint8_t)k_hex[(csum >> 4) & 0xF];
    out[pos++] = (uint8_t)k_hex[csum & 0xF];
    return pos;
}

rsp_err_t rsp_decode_packet(const uint8_t *in, size_t in_len,
                            uint8_t *out, size_t cap, size_t *out_len) {
    // Find '$'.
    size_t start = 0;
    while (start < in_len && in[start] != '$') start++;
    if (start >= in_len) return RSP_NO_PACKET;
    // Find '#' after start.
    size_t hash = start + 1;
    while (hash < in_len && in[hash] != '#') hash++;
    if (hash >= in_len) return RSP_UNTERMINATED;
    if (hash + 2 >= in_len) return RSP_UNTERMINATED;
    const uint8_t *raw = &in[start + 1];
    size_t raw_len = hash - (start + 1);
    uint8_t expected = xor_checksum(raw, raw_len);
    uint8_t hi, lo;
    if (hex_nibble(in[hash + 1], &hi) < 0) return RSP_BAD_HEX;
    if (hex_nibble(in[hash + 2], &lo) < 0) return RSP_BAD_HEX;
    uint8_t got = (uint8_t)((hi << 4) | lo);
    if (got != expected) return RSP_BAD_CHECKSUM;

    // Reverse escapes + expand RLE into `out`.
    size_t op = 0;
    size_t i = 0;
    while (i < raw_len) {
        uint8_t b = raw[i];
        if (b == '}') {
            i++;
            if (i >= raw_len) return RSP_BAD_ESCAPE;
            if (op >= cap) return RSP_OVERFLOW;
            out[op++] = (uint8_t)(raw[i] ^ 0x20);
            i++;
        } else if (b == '*') {
            if (op == 0) return RSP_BAD_ESCAPE;
            i++;
            if (i >= raw_len) return RSP_BAD_ESCAPE;
            uint8_t cnt = raw[i];
            if (cnt < 29) return RSP_BAD_ESCAPE;
            uint8_t prev = out[op - 1];
            size_t extra = (size_t)(cnt - 29);
            for (size_t k = 0; k < extra; ++k) {
                if (op >= cap) return RSP_OVERFLOW;
                out[op++] = prev;
            }
            i++;
        } else {
            if (op >= cap) return RSP_OVERFLOW;
            out[op++] = b;
            i++;
        }
    }
    *out_len = op;
    return RSP_OK;
}
