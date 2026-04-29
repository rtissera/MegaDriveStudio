# tools/Makefile — M0 spike host tools
#
# dump_vram links against the static libra produced by:
#   cmake -S vendor/libra -B vendor/libra/build -DBUILD_SHARED_LIBS=OFF
#   cmake --build vendor/libra/build -j$(nproc)
#
# Static libra pulls in glslang + SPIRV-Cross (C++), so we link with c++.

CXX       ?= c++
# ASAN is currently REQUIRED at runtime: there is a heap-corruption bug in
# libra's audio path on the ClownMDEmu sample rate (≈223 kHz → 48 kHz) that
# crashes glibc's default malloc inside libra_audio_flush.  ASAN's red zones
# happen to mask it.  Once the upstream bug is fixed, drop -fsanitize=address.
SAN       ?= -fsanitize=address
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
