| SPDX-License-Identifier: MIT
|
| entry.s — blob header + exception entry thunks.
|
| Per fact-check C13/C14/C26 (see /tmp/mds-edpro-factcheck.md): the stub
| code lives in cart PSRAM at $300000 and is reached by the running 68k
| because cart PSRAM is mapped onto 68k addresses $0-$3FFFFF. The host
| writes the absolute entry-point addresses directly into vector slots
| $24 (Trace) and $84 (TRAP #1) via PI-bus MEM_WR — the vectors live in
| PSRAM too, so they're host-writable.
|
| Header layout (16 bytes, big-endian on m68k):
|   +0x00  u32  MAGIC = 'MDST' (0x4D445354)
|   +0x04  u32  entry_trace        — install at vector $0024
|   +0x08  u32  entry_trap1        — install at vector $0084
|   +0x0C  u32  reserved (0)
|
| The host parses bytes 4..12 of the binary blob, MEM_WRs the four bytes
| at offset 4 to $24 and the four bytes at offset 8 to $84. On the next
| Trace / TRAP #1 the 68000 reads the long at the vector slot and jumps
| straight there — the vector value IS the target PC.

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
