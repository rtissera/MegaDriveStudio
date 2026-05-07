/* SPDX-License-Identifier: MIT */
/*
 * mdsstub.ld — linker script for the standalone, position-fixed 68k debug
 * stub blob.
 *
 * Layout: a single contiguous section at LOAD_ADDR. The first 16 bytes are
 * a header (magic + entry-point pointers) the host parses to find the trace
 * and trap1 entry points without computing offsets. Everything after the
 * header is code/rodata/data/bss merged into one chunk so `objcopy -O
 * binary` produces a flat blob.
 *
 * No segment for SGDK boot; this binary stands alone. The host uploads the
 * full blob (including pre-zeroed BSS area) to `LOAD_ADDR` via `MEM_WR`.
 *
 * If you change LOAD_ADDR here, also update:
 *   - `mds-stub-68k/Makefile`               (LOAD_ADDR := ...)
 *   - `mds-stub-68k/src/entry.s`            (header MAGIC banner only — no
 *                                            address constants there)
 *   - host: `mds-mcp/src/target/edpro/...`  (STUB_LOAD_ADDR constant)
 */

OUTPUT_FORMAT("elf32-m68k")
OUTPUT_ARCH(m68k)

ENTRY(__stub_entry)

MEMORY
{
    /* 16 KiB window in 68k work RAM. The blob is < 4 KB; the slack covers
     * BSS expansion and small future growth without colliding with SGDK's
     * heap (typically below $FF8000) or stack (top of $FFFFFE growing down). */
    ramstub (rwx) : ORIGIN = 0xFF8000, LENGTH = 0x4000
}

SECTIONS
{
    . = 0xFF8000;

    .stub : {
        __stub_base = .;
        KEEP(*(.stub.header))
        *(.text .text.*)
        *(.rodata .rodata.*)
        *(.data .data.*)
        . = ALIGN(2);
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(2);
        __bss_end = .;
        __stub_end = .;
    } > ramstub

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
        *(.gnu.attributes)
    }
}
