| SPDX-License-Identifier: MIT
|
| save_regs.s — m68000 exception save/restore macros.
|
| GDB m68k register layout (18 longs, 72 bytes total):
|     [ 0..7 ]  D0..D7
|     [ 8..14]  A0..A6
|     [15]      A7 (= USP at exception time, since the stub runs in supervisor)
|     [16]      SR (zero-extended to 32 bits, low 16 bits significant)
|     [17]      PC
|
| 68000 short (group-2) exception stack frame (Trace, TRAP):
|     SP+0  SR  (word)
|     SP+2  PC  (long, big-endian)
| Total = 6 bytes. We pop them after saving.

	.global mds_regs
	.global mds_save_regs
	.global mds_restore_regs

	.bss
	.align 2
mds_regs:
	.space 18*4

	.text

|------------------------------------------------------------------------------
| mds_save_regs
| On entry: SP points at the exception frame (SR.w, PC.l).
| Saves D0-D7 / A0-A6 / USP / SR / PC into mds_regs[].
| Trashes D0/A0 (recoverable from saved frame).
| Caller is responsible for switching back to user SP / RTE.
|------------------------------------------------------------------------------
mds_save_regs:
	movem.l	%d0-%d7/%a0-%a6, mds_regs   | D0..D7, A0..A6 → mds_regs[0..14]
	move.l	%usp, %a0                    | A7 (user) → mds_regs[15]
	move.l	%a0, mds_regs+15*4
	moveq	#0, %d0
	move.w	(%sp), %d0                   | SR from frame
	move.l	%d0, mds_regs+16*4
	move.l	2(%sp), mds_regs+17*4        | PC from frame
	rts

|------------------------------------------------------------------------------
| mds_restore_regs
| Inverse of save: writes D0-D7/A0-A6/USP back from mds_regs[], updates the
| SR + PC fields in the exception frame so RTE picks them up.
| Must be called with SP still pointing at the exception frame.
|------------------------------------------------------------------------------
mds_restore_regs:
	move.l	mds_regs+17*4, %d0           | new PC into the frame
	move.l	%d0, 2(%sp)
	move.l	mds_regs+16*4, %d0           | new SR (low word)
	move.w	%d0, (%sp)
	move.l	mds_regs+15*4, %a0           | restore USP
	move.l	%a0, %usp
	movem.l	mds_regs, %d0-%d7/%a0-%a6
	rts
