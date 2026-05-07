# SPDX-License-Identifier: MIT
#
# mds-stub-68k Makefile — produces a position-fixed flat 68k binary
# (`mdsstub.bin`) that the host (`mds-mcp`) uploads to cart PSRAM via the
# Mega Everdrive Pro `MEM_WR` opcode at debug-attach time.
#
# This is NOT a static library and is NOT linked into the user's SGDK ROM.
# The user ROM has zero awareness the stub exists. Deployment flow (per
# fact-check C13/C14/C20/C26 in /tmp/mds-edpro-factcheck.md):
#
#   1. host sends HOST_RST(Soft) to halt CPU
#   2. host MEM_WRs `mdsstub.bin` to PSRAM (PI-bus addr $300000)
#   3. host MEM_WRs entry_trace/entry_trap1 into vectors $24/$84
#      (PI-bus addrs $24/$84 — vector table is in PSRAM)
#   4. host sends HOST_RST(Off) to release CPU; user ROM runs until
#      a TRAP #1 or T-bit step → control lands in this stub
#
# Code (text+rodata) lives in PSRAM at $300000+. Data (BSS) lives in MD
# work RAM at $FFEE00..$FFEFFF — the stub zero-initialises BSS on first
# entry because the host has no PI-bus path to MD work RAM.
#
# See README.md for the full deployment contract and rationale.

MARSDEV ?= $(HOME)/mars
TOOL    := $(MARSDEV)/m68k-elf/bin/m68k-elf

CC      := $(TOOL)-gcc
LD      := $(TOOL)-ld
AS      := $(TOOL)-as
OBJCOPY := $(TOOL)-objcopy
OBJDUMP := $(TOOL)-objdump
SIZE    := $(TOOL)-size
NM      := $(TOOL)-nm

# Documented load address — must match `psram_stub` ORIGIN in `mdsstub.ld`
# and `STUB_LOAD_ADDR` in `mds-mcp/src/target/edpro/stub_blob.rs`. Used
# only for human-readable reporting (the actual placement is driven by
# `mdsstub.ld`).
LOAD_ADDR := 0x300000

# 68000 strict (no 010+ instructions, no MOVEC, no VBR).
# -Os: optimize for size; we have a ~4 KB budget.
# -nostdlib / -ffreestanding / -fno-builtin: no libc, no memcpy emission.
# -fno-asynchronous-unwind-tables / -fno-unwind-tables: no .eh_frame.
# -fomit-frame-pointer: A6 stays free for fast register save.
CFLAGS := -m68000 -Os -nostdlib -ffreestanding -fno-builtin \
          -fno-asynchronous-unwind-tables -fno-unwind-tables \
          -fomit-frame-pointer \
          -Wall -Wextra -Wno-unused-parameter -std=c99 \
          -Isrc

ASFLAGS := -m68000

LDFLAGS := -nostdlib -T mdsstub.ld -Wl,--no-warn-rwx-segments \
           -Wl,-Map=mdsstub.map

C_SRCS := src/stub.c src/usb.c
S_SRCS := src/entry.s src/save_regs.s
OBJS   := $(C_SRCS:.c=.o) $(S_SRCS:.s=.o)

ELF := mdsstub.elf
BIN := mdsstub.bin

.PHONY: all clean info verify

all: $(BIN)

$(ELF): $(OBJS) mdsstub.ld
	$(CC) $(CFLAGS) $(LDFLAGS) -o $@ $(OBJS)

$(BIN): $(ELF)
	$(OBJCOPY) -O binary $< $@

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

%.o: %.s
	$(AS) $(ASFLAGS) -o $@ $<

info: $(ELF)
	@echo "=== size ==="
	@$(SIZE) $(ELF)
	@echo "=== nm (top 20 symbols) ==="
	@$(NM) -n $(ELF) | head -20
	@echo "=== entry header (first 16 bytes of $(BIN)) ==="
	@hexdump -C $(BIN) | head -1

verify: $(BIN) $(ELF)
	@echo "=== bin size ==="
	@stat -c '%n: %s bytes' $(BIN)
	@echo "=== load addr from elf ==="
	@$(NM) $(ELF) | grep -E '__stub_(base|end)\b' || true
	@echo "=== first instructions ==="
	@$(OBJDUMP) -d $(ELF) | head -30

clean:
	rm -f $(OBJS) $(ELF) $(BIN) mdsstub.map
