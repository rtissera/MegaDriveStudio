| SPDX-License-Identifier: MIT
|
| vectors.s — exception entry stubs for Trace ($0024) and TRAP #1 ($0084).
|
| The link.ld.frag (or runtime patch in mds_stub_init) installs the addresses
| of `__mds_vec_trace` and `__mds_vec_trap1` into the corresponding vector
| table slots. SGDK 2.x copies the ROM vector table to RAM at $FF0000 in
| boot/sega.s, so runtime patching is supported on that target — see
| README.md "Integration".

	.global __mds_vec_trace
	.global __mds_vec_trap1
	.extern mds_save_regs
	.extern mds_restore_regs
	.extern mds_stub_enter_handler

	| Exception IDs passed to the C handler. Match GDB stop-signal mapping
	| (5 = SIGTRAP for both — distinguishing field is reason:swbreak vs trace).
	.equ MDS_EXC_TRACE,  9
	.equ MDS_EXC_TRAP1, 33

	.text

|------------------------------------------------------------------------------
| Trace exception entry — vector $24 (offset 9 from base).
| 68000 sets bit T in SR; after each instruction completes we land here.
|------------------------------------------------------------------------------
__mds_vec_trace:
	jsr	mds_save_regs
	move.l	#MDS_EXC_TRACE, -(%sp)
	jsr	mds_stub_enter_handler
	addq.l	#4, %sp
	jsr	mds_restore_regs
	rte

|------------------------------------------------------------------------------
| TRAP #1 entry — vector $84 (offset 33). Software breakpoint.
| The patched opcode has been replaced w/ 0x4E41; saved PC points just past
| the trap. C handler will roll PC back to the BP address and restore the
| original opcode before resuming.
|------------------------------------------------------------------------------
__mds_vec_trap1:
	jsr	mds_save_regs
	move.l	#MDS_EXC_TRAP1, -(%sp)
	jsr	mds_stub_enter_handler
	addq.l	#4, %sp
	jsr	mds_restore_regs
	rte
