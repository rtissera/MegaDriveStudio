/* SPDX-License-Identifier: MIT */
/*
 * link.ld.frag — vector overrides for the on-cart 68k debug stub.
 *
 * INCLUDE this fragment from the SGDK linker script (md.ld) ahead of the
 * default `.vectors` definition, OR pass it via `-T`. It declares the
 * stub vector entry symbols so the link succeeds even without runtime
 * patching.
 *
 * IMPORTANT: SGDK 2.x already copies the vector table to RAM at $FF0000
 * during boot/sega.s. The runtime path in `mds_stub_init()` patches the
 * RAM copy directly — preferred over this link-time approach because it
 * doesn't conflict with SGDK's own runtime overrides (KDebug, VDP IRQ
 * handlers, etc).
 *
 * Use this fragment ONLY for non-SGDK / bare-metal projects that build
 * their vector table at link time and need the stub addresses resolved
 * statically.
 */

EXTERN(__mds_vec_trace);
EXTERN(__mds_vec_trap1);

/* Convenience aliases callers may reference from a custom .vectors section:
 *
 *   .vectors :
 *   {
 *       LONG(__stack_top)
 *       LONG(_start)
 *       . = 0x24;
 *       LONG(__mds_vec_trace)
 *       . = 0x84;
 *       LONG(__mds_vec_trap1)
 *       ...
 *   } > rom
 */
PROVIDE(mds_vec_trace_addr = __mds_vec_trace);
PROVIDE(mds_vec_trap1_addr = __mds_vec_trap1);
