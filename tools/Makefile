# tools/Makefile — M0 spike host tools
#
# dump_vram links against the static libra produced by:
#   cmake -S vendor/libra -B vendor/libra/build -DBUILD_SHARED_LIBS=OFF
#   cmake --build vendor/libra/build -j$(nproc)
#
# Static libra pulls in glslang + SPIRV-Cross (C++), so we link with c++.

CXX       ?= c++
# Note: an earlier ClownMDEmu run-with-libra heap corruption was rooted in
# RETRO_ENVIRONMENT_GET_CURRENT_SOFTWARE_FRAMEBUFFER sizing (libra PR #1,
# merged 2026-04-29). Bug is gone; SAN= remains opt-in for debugging.
SAN       ?=
CFLAGS    ?= -O2 -g -Wall -Wextra $(SAN)
LDFLAGS   ?= $(SAN)
LIBRA_DIR := ../vendor/libra
LIBRA_BLD := $(LIBRA_DIR)/build

INCLUDES  := -I$(LIBRA_DIR)/include -I$(LIBRA_DIR)/deps
LIBS      := $(LIBRA_BLD)/liblibra.a \
             $(LIBRA_BLD)/deps/glslang/glslang/libglslang-default-resource-limits.a \
             $(LIBRA_BLD)/deps/glslang/glslang/libglslang.a \
             $(LIBRA_BLD)/deps/glslang/SPIRV/libSPIRV.a \
             $(LIBRA_BLD)/deps/SPIRV-Cross/libspirv-cross-c.a \
             $(LIBRA_BLD)/deps/SPIRV-Cross/libspirv-cross-glsl.a \
             $(LIBRA_BLD)/deps/SPIRV-Cross/libspirv-cross-core.a
SYSLIBS   := -lpthread -ldl -lm

.PHONY: all clean
all: dump_vram

dump_vram: dump_vram.c $(LIBRA_BLD)/liblibra.a
	$(CXX) $(CFLAGS) $(INCLUDES) -o $@ dump_vram.c $(LIBS) $(LDFLAGS) $(SYSLIBS)

clean:
	rm -f dump_vram
