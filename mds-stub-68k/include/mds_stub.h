// SPDX-License-Identifier: MIT
//
// mds_stub.h — public API for the on-cart Mega Drive 68k debug stub.
//
// The stub catches Trace ($0024) + TRAP #1 ($0084) exceptions, dumps register
// state into a fixed RAM buffer, then talks GDB Remote Serial Protocol (RSP)
// over the Mega Everdrive Pro USB FIFO at $A130D0/D2 to the host MCP server.
// See docs/02-m5-architecture.md §5 for the design.
//
// Cleanroom on https://github.com/mborgerson/gdbstub (MIT). Do NOT link
// gdb's m68k-stub.c (GPLv2).

#ifndef MDS_STUB_H
#define MDS_STUB_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Call once early in main() (or _start), before enabling interrupts.
// Patches Trace ($24) + TRAP #1 ($84) vectors to point at the stub
// (writes to RAM-shadowed vector table at $FF0000 if SGDK relocated the
// vector base there; otherwise overrides ROM vectors at link time via
// link.ld.frag — see README.md for the integration matrix).
//
// Sends a "hello" probe over the USB FIFO so the host can confirm the stub
// is alive. Idempotent — safe to call multiple times.
void mds_stub_init(void);

// Force-enter the stub from C (e.g. an `assert()` failure). Equivalent to
// executing TRAP #1 inline. Implemented as a tiny inline helper so the
// caller's PC ends up in the saved frame.
static inline void mds_stub_break(void) {
    __asm__ volatile("trap #1");
}

#ifdef __cplusplus
}
#endif
#endif  // MDS_STUB_H
