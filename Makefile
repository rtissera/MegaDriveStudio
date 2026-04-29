# =============================================================================
# Megadrive Studio — Makefile
# =============================================================================

# ── Environnement ─────────────────────────────────────────────────────────────
# Source l'env Megadrive Studio si disponible
-include $(HOME)/.local/megadrive-studio/env.sh

GDK      ?= $(HOME)/sgdk
MARSDEV  ?= $(HOME)/marsdev
BLASTEM  ?= $(shell command -v blastem 2>/dev/null || echo blastem)
CLOWNMDEMU ?= $(shell command -v clownmdemu 2>/dev/null || echo clownmdemu)
MEGALINK ?= $(shell command -v megalink 2>/dev/null || echo megalink)
EDPRO_PORT ?= /dev/everdrive
GDB      ?= $(shell command -v m68k-elf-gdb 2>/dev/null || echo gdb-multiarch)
GDB_PORT ?= 2345

ROM     = out/rom.bin
ROM_ELF = out/rom.elf

# ── Build ─────────────────────────────────────────────────────────────────────
.PHONY: all debug release clean

all: debug

debug:
	@echo "[build] debug ROM (avec symboles)"
	make -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) \
	  EXTRA_DEF="-DDEBUG" \
	  EXTRA_CFLAGS="-g -gdwarf-4 -O0"

release:
	@echo "[build] release ROM"
	make -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) \
	  EXTRA_CFLAGS="-O2"

clean:
	make -f $(GDK)/makefile.gen GDK=$(GDK) MARSDEV=$(MARSDEV) clean
	@rm -f /tmp/kdebug.log /tmp/gdb-proxy.pid

# ── Target A : BlastEm ────────────────────────────────────────────────────────
.PHONY: run-blastem debug-gdb debug-gdb-socket

run-blastem: debug
	@echo "[blastem] lancement sans GDB"
	$(BLASTEM) $(ROM)

# Lance BlastEm en mode debug (pipe) — VS Code s'y connecte ensuite via F5
debug-gdb: debug
	@echo "[blastem] mode GDB pipe — connecte VS Code avec F5 (config BlastEm GDB pipe)"
	@echo "          ou manuellement :"
	@echo "          m68k-elf-gdb -ex 'target remote | $(BLASTEM) $(ROM) -D' $(ROM_ELF)"
	$(BLASTEM) $(ROM) -D &

# Socket mode (fallback Windows / headless)
debug-gdb-socket: debug
	@echo "[blastem] mode GDB socket :1234"
	$(BLASTEM) $(ROM) -D &
	sleep 1
	$(GDB) -ex "file $(ROM_ELF)" \
	       -ex "set architecture m68k:68000" \
	       -ex "target remote :1234"

# ── Target B : ClownMDEmu ─────────────────────────────────────────────────────
.PHONY: run-clown

run-clown: debug
	@echo "[clownmdemu] lancement avec debug UI ImGui"
	$(CLOWNMDEMU) $(ROM)

# ── Target C : Hardware (ED Pro) ──────────────────────────────────────────────
.PHONY: upload-edpro klog-monitor debug-hardware

upload-edpro: debug
	@echo "[edpro] upload ROM → $(EDPRO_PORT)"
	$(MEGALINK) -p $(EDPRO_PORT) load $(ROM) --run

klog-monitor:
	@echo "[klog] monitoring KDebug sur $(EDPRO_PORT)"
	@scripts/kdebug-monitor.sh $(EDPRO_PORT)

debug-hardware: upload-edpro
	@echo "[gdb] lancement proxy RSP pour ED Pro"
	@python3 scripts/gdb-proxy.py --port $(EDPRO_PORT) --gdb-port $(GDB_PORT) &
	@echo "PID proxy: $$!" > /tmp/gdb-proxy.pid
	@echo "Connecte VS Code : F5 (config ED Pro GDB hardware)"

# ── Utilitaires ───────────────────────────────────────────────────────────────
.PHONY: info rebuild-blastem rebuild-clown rebuild-sgdk

info:
	@echo "Megadrive Studio — environnement"
	@echo "  GDK        : $(GDK)"
	@echo "  MARSDEV    : $(MARSDEV)"
	@echo "  BlastEm    : $(BLASTEM) ($(shell $(BLASTEM) --version 2>&1 | head -1 || echo n/a))"
	@echo "  ClownMDEmu : $(CLOWNMDEMU)"
	@echo "  megalink   : $(MEGALINK)"
	@echo "  GDB        : $(GDB)"
	@echo "  ED Pro     : $(EDPRO_PORT)"

rebuild-blastem:
	@echo "[rebuild] BlastEm depuis source"
	@cd $$HOME/.local/megadrive-studio/src/blastem && \
	  git pull && make -j$$(nproc) CFLAGS="-O2 -g"

rebuild-clown:
	@echo "[rebuild] ClownMDEmu depuis source"
	@cd $$HOME/.local/megadrive-studio/src/clownmdemu && \
	  git pull --recurse-submodules && \
	  cmake --build build -j$$(nproc)

rebuild-sgdk:
	@echo "[rebuild] SGDK lib"
	@make -f $(GDK)/makelib.gen GDK=$(GDK) MARSDEV=$(MARSDEV)
