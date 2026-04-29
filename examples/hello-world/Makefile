# =============================================================================
# Megadrive Studio — hello-world example
# Thin wrapper around SGDK's makefile.gen.
#
# Resolution order for $GDK:
#   1. Environment (set by start.sh in the bundle, or by the user manually).
#   2. Bundle-relative discovery: ../../toolchain/sgdk
#      (works when this Makefile sits at <bundle>/examples/hello-world/Makefile,
#      which is exactly where the released bundle places it).
#   3. Hard error with a clear message — no silent fallthrough to a non-existent
#      Docker image or a missing /sgdk.
# =============================================================================

# Where this Makefile lives + the conventional bundle root two levels up.
THIS_DIR    := $(realpath $(dir $(lastword $(MAKEFILE_LIST))))
BUNDLE_ROOT := $(realpath $(THIS_DIR)/../..)
BUNDLE_GDK  := $(BUNDLE_ROOT)/toolchain/sgdk

ifeq ($(GDK),)
  ifneq ($(wildcard $(BUNDLE_GDK)/makefile.gen),)
    GDK := $(BUNDLE_GDK)
  endif
endif

ifeq ($(GDK),)
  $(error $$GDK is unset and the bundled toolchain was not found at $(BUNDLE_GDK). \
    Either launch via the bundle's start.sh (which exports GDK + PATH), \
    or set GDK manually to your SGDK install root.)
endif

# Make the bundled m68k-elf cross binaries + JRE visible to child make.
ifneq ($(wildcard $(GDK)/bin/m68k-elf-gcc),)
  export PATH := $(GDK)/bin:$(PATH)
endif
ifneq ($(wildcard $(BUNDLE_ROOT)/toolchain/jre/bin/java),)
  export PATH := $(BUNDLE_ROOT)/toolchain/jre/bin:$(PATH)
  export JAVA_HOME := $(BUNDLE_ROOT)/toolchain/jre
endif

export GDK

ROM     = out/rom.bin
ROM_ELF = out/rom.elf

.PHONY: all debug release clean

all: release

release:
	@echo "[hello-world] release ROM (GDK=$(GDK))"
	$(MAKE) -f $(GDK)/makefile.gen EXTRA_CFLAGS="-O2"

debug:
	@echo "[hello-world] debug ROM (DWARF-4 symbols, GDK=$(GDK))"
	$(MAKE) -f $(GDK)/makefile.gen \
	    EXTRA_DEF="-DDEBUG" \
	    EXTRA_CFLAGS="-g -gdwarf-4 -O0"

clean:
	-$(MAKE) -f $(GDK)/makefile.gen clean
	-rm -f out/*.bin out/*.elf out/*.map out/*.lst
