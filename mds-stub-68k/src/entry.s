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
| Header layout (24 bytes, big-endian on m68k):
|   +0x00  u32  MAGIC = 'MDST' (0x4D445354)
|   +0x04  u32  entry_trace        — install at vector $0024
|   +0x08  u32  entry_trap1        — install at vector $0084
|   +0x0C  u32  entry_vbl          — install at vector $0078 for pause (M5.9)
|   +0x10  u32  paused_flag        — host writes 1 to request a pause; the
|                                    stub clears to 0 on resume. Lives in
|                                    PSRAM (not BSS) because the host has
|                                    no PI-bus path to MD work RAM.
|   +0x14  u32  original_vbl       — host stashes the user's existing VBL
|                                    handler addr here BEFORE patching
|                                    vector $78. The stub jumps to it on
|                                    every VBL when paused_flag == 0, so
|                                    the user's VBL handler keeps running
|                                    while the host arms the pause.
|
| The host parses bytes 4..16 of the binary blob, MEM_WRs entry_trace to
| $24, entry_trap1 to $84, and (for pause) entry_vbl to $78. On the next
| Trace / TRAP #1 / VBL the 68000 reads the long at the vector slot and
| jumps straight there — the vector value IS the target PC.

    .global __stub_entry
    .global __stub_trace_entry
    .global __stub_trap1_entry
    .global __stub_vbl_entry
    .extern mds_save_regs
    .extern mds_restore_regs
    .extern mds_stub_enter_handler
    .extern mds_stub_pause_handler

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
    | entry_vbl   — host installs at $0078 for M5.9 pause.
    .long __stub_vbl_entry
    | paused_flag — host writes 1 to request a pause; stub clears on resume.
    .long 0
    | original_vbl — host writes the saved user VBL handler addr.
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

|------------------------------------------------------------------------------
| VBL (level-6 autovector) entry — vector $78 points here when host has
| armed an M5.9 pause.
|
| Fast path (paused_flag == 0): chain straight to the user's original
| VBL handler so the running game keeps drawing frames between
| pause-arm and pause-fire. We touch ONE memory load + branch, then JMP
| through original_vbl. We do NOT save any regs on this path (the user
| VBL handler will save what it needs); the JMP target sees the same
| exception frame that the CPU pushed for us.
|
| Slow path (paused_flag != 0): full save_regs, hand off to
| mds_stub_pause_handler (which announces T05 + runs the RSP loop +
| clears paused_flag on resume), restore_regs, and RTE. The user's
| original VBL handler is SKIPPED for this single VBL — acceptable post
| pause, the user game just loses one frame of VBL processing.
|
| 68000 group-1 IRQ stack frame is the same 6-byte short frame as Trace
| and TRAP #1 (SR.w + PC.l), so mds_save_regs / mds_restore_regs work
| unchanged.
|------------------------------------------------------------------------------
__stub_vbl_entry:
    | Probe paused_flag — header offset 0x10. We MUST preserve every
    | user register on the fast path because the chained user-VBL
    | handler may rely on D0/A0 being whatever the IRQ pre-empted. Use
    | TST on the absolute long: zero-operand probe, no scratch needed
    | (it sets condition codes from the memory operand directly).
    tst.l   __stub_entry+0x10
    bne.s   1f
    | Fast path — chain to the user's VBL handler. Trick: push the dest
    | longword onto the supervisor stack, then RTS pops it into PC. That
    | gets us to the original VBL with ZERO scratch regs touched. The
    | exception frame (SR.w + PC.l) the CPU pushed for us is still in
    | place beneath, so the user handler's RTE pops it correctly.
    move.l  __stub_entry+0x14, -(%sp)
    rts
1:
    | Slow path — paused_flag != 0. Run the full save/handler/restore
    | cycle and RTE.
    jsr     mds_save_regs
    jsr     mds_stub_pause_handler
    jsr     mds_restore_regs
    rte
