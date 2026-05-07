# SPDX-License-Identifier: MIT
#
# mds-stub-68k Makefile — produces libmdsstub.a for inclusion in SGDK
# user ROMs. Cross-compiles with marsdev's m68k-elf toolchain.

MARSDEV ?= $(HOME)/mars
TOOL := $(MARSDEV)/m68k-elf/bin/m68k-elf

CC := $(TOOL)-gcc
AS := $(TOOL)-as
AR := $(TOOL)-ar
NM := $(TOOL)-nm
SIZE := $(TOOL)-size

CFLAGS := -m68000 -Os -nostdlib -ffreestanding -fno-builtin \
          -Wall -Wextra -Wno-unused-parameter -std=c99 \
          -Iinclude -Isrc
ASFLAGS := -m68000

C_SRCS := src/stub.c src/rsp.c src/usb.c src/bp.c
S_SRCS := src/vectors.s src/save_regs.s
OBJS   := $(C_SRCS:.c=.o) $(S_SRCS:.s=.o)

TARGET := libmdsstub.a

.PHONY: all clean info

all: $(TARGET)

$(TARGET): $(OBJS)
	$(AR) rcs $@ $^

%.o: %.c
	$(CC) $(CFLAGS) -c $< -o $@

%.o: %.s
	$(AS) $(ASFLAGS) -o $@ $<

info: $(TARGET)
	@echo "=== nm $(TARGET) ==="
	$(NM) $(TARGET)
	@echo "=== size $(TARGET) ==="
	$(SIZE) --format=sysv $(TARGET)

clean:
	rm -f $(OBJS) $(TARGET)
