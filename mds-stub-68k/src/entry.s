| SPDX-License-Identifier: MIT
|
| entry.s — blob header + exception entry thunks.
|
| This is the first thing in the linked binary (KEEP'd by mdsstub.ld at
| section .stub.header). The host parses the 16-byte header to discover
| the trace and trap1 entry points, then writes JMP-thunks at vector
| slots $24 (Trace) and $84 (TRAP #1) so the running user ROM bounces
| into us when an exception fires.
|
| Why a JMP thunk via the host instead of writing the absolute entry
| address directly into vector $24? Because on a plain 68000 (no VBR)
| the vectors live in cart "ROM" — reachable by the MCU through MEM_WR
| against the PSRAM, but only as raw u32 longs. The host writes the
| absolute entry address at $24 / $84 *as the vector value*. The CPU
| reads the long and jumps to it via the standard exception-dispatch
| sequence. No JMP instruction needed; the vector value IS the target
| PC. (We use the term "thunk" loosely here.)
|
| Header layout (16 bytes, big-endian on m68k):
|   +0x00  u32  MAGIC = 'MDST' (0x4D445354)
|   +0x04  u32  entry_trace        — install at vector $0024
|   +0x08  u32  entry_trap1        — install at vector $0084
|   +0x0C  u32  reserved (0)
|
| Following the header the code section starts. Because we built with a
| fixed LOAD_ADDR ($FF8000), the host can compute "where to point each
| vector" by reading bytes 4..12 of the binary blob — no symbol lookup,
| no relocation, no offset arithmetic. Just MEM_WR the four bytes from
| header[+4] to address $24, and the four bytes from header[+8] to
| address $84. Done.

    .global __stub_entry
    .global __stub_trace_entry
    .global __stub_trap1_entry
    .extern mds_save_regs
    .extern mds_restore_regs
    .extern mds_stub_enter_handler

    .equ MDS_EXC_TRACE,  9
    .equ MDS_EXC_TRAP1, 33

|------------------------------------------------------------------------------
| Header section. Linker keeps this at the very start of .stub.
|------------------------------------------------------------------------------
    .section .stub.header, "ax"
    .align 2

__stub_entry:
    | Magic 'MDST' — 0x4D 0x44 0x53 0x54.
    .long 0x4D445354
    | entry_trace — host installs at $0024.
    .long __stub_trace_entry
    | entry_trap1 — host installs at $0084.
    .long __stub_trap1_entry
    | reserved.
    .long 0

|------------------------------------------------------------------------------
| Trace exception entry — vector $24 points here.
| 68000 sets bit T in SR; after each instruction we land here.
|------------------------------------------------------------------------------
    .text
    .align 2

__stub_trace_entry:
    jsr     mds_save_regs
    move.l  #MDS_EXC_TRACE, -(%sp)
    jsr     mds_stub_enter_handler
    addq.l  #4, %sp
    jsr     mds_restore_regs
    rte

|------------------------------------------------------------------------------
| TRAP #1 entry — vector $84 points here. Software breakpoint.
|------------------------------------------------------------------------------
__stub_trap1_entry:
    jsr     mds_save_regs
    move.l  #MDS_EXC_TRAP1, -(%sp)
    jsr     mds_stub_enter_handler
    addq.l  #4, %sp
    jsr     mds_restore_regs
    rte
