// SPDX-License-Identifier: MIT
//
// rsp.h — GDB Remote Serial Protocol codec, byte-compatible with
// mds-mcp/src/target/edpro/rsp.rs (host side).
//
// Frame: $<payload>#<csum>
//   csum = (XOR of payload bytes) mod 256, two lowercase hex chars
//   `#`, `$`, `}`, `*` in payload escaped as `}` + (byte ^ 0x20)
//   `*` RLE: <byte>*<count>  expands to <byte> repeated (count - 29 + 1) total
//
// We DECODE escape + RLE; we do NOT compress on encode (gdb spec permits
// either; rsp.rs matches).

#ifndef MDS_RSP_H
#define MDS_RSP_H

#include <stddef.h>
#include <stdint.h>

// 400 bytes is what we advertise to gdb in qSupported (PacketSize=400).
// Round up to leave headroom for `}`-escapes (worst case 2x) plus framing.
#define RSP_BUF_MAX 1024

typedef enum {
    RSP_OK = 0,
    RSP_BAD_CHECKSUM,
    RSP_BAD_ESCAPE,
    RSP_OVERFLOW,
    RSP_NO_PACKET,        // no '$' start byte
    RSP_UNTERMINATED,     // '$' found but no closing '#xx'
    RSP_BAD_HEX
} rsp_err_t;

// Encode a payload into a framed RSP packet (`$payload#xx`). Applies
// `}` escapes for # $ } * before computing checksum. Returns the number
// of bytes written to `out`, or 0 on overflow.
size_t rsp_encode_packet(const uint8_t *payload, size_t plen,
                         uint8_t *out, size_t cap);

// Decode a framed RSP packet starting from `in[0..in_len)`. Skips leading
// non-`$` bytes. Reverses `}` escapes + expands `*` RLE. On success,
// writes payload to `out` and length to `*out_len`. `out` cap is `cap`.
rsp_err_t rsp_decode_packet(const uint8_t *in, size_t in_len,
                            uint8_t *out, size_t cap, size_t *out_len);

// Helpers (exposed for stub.c reply builders).

// Push two lowercase hex chars for `b` into out[*pos..*pos+2]. Returns
// 0 on success, -1 on overflow.
int rsp_hex_byte(uint8_t *out, size_t cap, size_t *pos, uint8_t b);

// Same for a u32, big-endian. 8 chars unconditional.
int rsp_hex_u32(uint8_t *out, size_t cap, size_t *pos, uint32_t v);

// Parse a lowercase/uppercase hex byte stream of 2*n chars → n bytes.
// Returns RSP_OK or RSP_BAD_HEX. Writes `n` bytes to `out`.
rsp_err_t rsp_parse_hex_bytes(const uint8_t *in, size_t in_len,
                              uint8_t *out, size_t cap, size_t *n_out);

// Parse a hex address/length token from `in[*pos..]` up to `term` (or
// end-of-input). Writes value to `*v`, advances `*pos`. Returns RSP_OK or
// RSP_BAD_HEX. `term` is 0 to mean "until end".
rsp_err_t rsp_parse_hex_u32(const uint8_t *in, size_t in_len, size_t *pos,
                            uint8_t term, uint32_t *v);

#endif  // MDS_RSP_H
