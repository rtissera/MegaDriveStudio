# =============================================================================
# Megadrive Studio — hello-world example
# Thin wrapper around SGDK's makefile.gen.
#
# This Makefile is invoked from inside the zerasul/sgdk:2.00 Docker image
# by the `Build ROM (Docker SGDK)` task. It also works on a host with SGDK
# installed natively (set GDK and MARSDEV).
# =============================================================================

GDK     ?= /sgdk
MARSDEV ?=

ROM     = out/rom.bin
ROM_ELF = out/rom.elf

.PHONY: all debug release clean

all: release

release:
	@echo "[hello-world] release ROM"
	$(MAKE) -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) \
	    EXTRA_CFLAGS="-O2"

debug:
	@echo "[hello-world] debug ROM (DWARF-4 symbols)"
	$(MAKE) -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) \
	    EXTRA_DEF="-DDEBUG" \
	    EXTRA_CFLAGS="-g -gdwarf-4 -O0"

clean:
	-$(MAKE) -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) clean
	-rm -f out/*.bin out/*.elf out/*.map out/*.lst
