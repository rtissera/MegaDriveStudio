/* SPDX-License-Identifier: MIT */
/*
 * mdsstub.ld — linker script for the standalone, position-fixed 68k debug
 * stub blob.
 *
 * Layout (per fact-check C13/C14/C26):
 *
 *   psram_stub  ORIGIN = 0x300000  LENGTH = 0x4000   (16 KiB)
 *     The stub TEXT/RODATA lives in cart PSRAM at the 3 MB mark — beyond
 *     typical 1-2 MB user ROM images. The 68k sees this as cart "ROM"
 *     mapped to address $300000. The host uploads the blob via MEM_WR
 *     against PI-bus PiBusAddr(0x300000), which lands here.
 *
 *   workram_bss ORIGIN = 0xFFEE00  LENGTH = 0x200    (512 B)
 *     The stub's runtime data (saved registers + RX/TX buffers) lives in
 *     MD work RAM. Referenced by absolute address — no relocation. The
 *     host CANNOT pre-zero this region (no PI-bus alias for work RAM, per
 *     fact-check C13); the stub zero-initialises BSS itself on first
 *     entry. Region picked to sit between SGDK heap (typically below
 *     $FFD000) and SGDK stack ($FFFFFE growing down). User ROMs MUST NOT
 *     touch $FFEE00-$FFEFFF during a debug session.
 *
 * Header (16 bytes, big-endian) at the very start of psram_stub:
 *   +0x00  u32  MAGIC = 'MDST'
 *   +0x04  u32  entry_trace  (absolute 68k address inside psram_stub)
 *   +0x08  u32  entry_trap1  (absolute 68k address inside psram_stub)
 *   +0x0C  u32  reserved
 *
 * The flat .bin produced by `objcopy -O binary` covers ONLY psram_stub:
 * BSS is not materialised. The blob is uploaded once via MEM_WR, then
 * the stub itself zeroes its BSS on first exception entry.
 *
 * If you change ORIGIN here, also update:
 *   - mds-mcp/src/target/edpro/stub_blob.rs   (STUB_LOAD_ADDR / STUB_BSS_ADDR)
 *   - mds-stub-68k/Makefile                   (LOAD_ADDR comment)
 */

OUTPUT_FORMAT("elf32-m68k")
OUTPUT_ARCH(m68k)

ENTRY(__stub_entry)

MEMORY
{
    psram_stub  (rwx) : ORIGIN = 0x300000, LENGTH = 0x4000
    workram_bss (rw)  : ORIGIN = 0xFFEE00, LENGTH = 0x200
}

SECTIONS
{
    .stub : {
        __stub_base = .;
        KEEP(*(.stub.header))
        *(.text .text.*)
        *(.rodata .rodata.*)
        *(.data .data.*)
        . = ALIGN(2);
        __stub_end = .;
    } > psram_stub

    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(2);
        __bss_end = .;
    } > workram_bss

    /DISCARD/ : {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
        *(.gnu.attributes)
    }
}
