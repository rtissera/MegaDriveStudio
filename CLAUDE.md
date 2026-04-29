# Megadrive Studio — CLAUDE.md

## Contexte du projet

Environnement de développement complet pour homebrew Sega Mega Drive / Genesis.
Toolchain : SGDK + MARSDEV + BlastEm (GDB) + ClownMDEmu (VDP debug) + Mega Everdrive Pro (hardware).

## Architecture des cibles de debug

```
TARGET A : BlastEm  → GDB pipe → m68k-elf-gdb → VS Code DAP
TARGET B : ClownMDEmu → Dear ImGui debug UI (VRAM, sprites, registres)
TARGET C : Hardware  → Mega SG + Mega Everdrive Pro → megalink-rs → KDebug / GDB stub
```

## Commandes Claude Code fréquentes

### Setup initial (première fois)
```bash
./setup.sh          # installe tout (BlastEm, ClownMDEmu, MARSDEV, extensions VS Code)
```

### Build
```bash
make debug          # ROM avec symboles de debug (-g -gdwarf-4)
make release        # ROM release
make clean
```

### Lancer les émulateurs
```bash
make run-blastem    # BlastEm seul (sans GDB)
make run-clown      # ClownMDEmu avec debug UI
make debug-gdb      # BlastEm + GDB (attendre connexion VS Code F5)
```

### Hardware ED Pro
```bash
make upload-edpro   # upload ROM via megalink-rs
make klog-monitor   # écoute KDebug via USB
```

## Structure du projet homebrew

```
src/          C sources
res/          ressources SGDK (.png, .vgm, etc.)
out/          build artifacts (rom.bin, rom.elf, *.o)
.vscode/      config VS Code (tasks, launch, settings)
scripts/      outils build/debug
.github/      CI/CD GitHub Actions
docker/       Dockerfile dev container
```

## Variables d'environnement importantes

```bash
GDK=/opt/sgdk            # ou ~/sgdk
MARSDEV=/opt/marsdev     # ou ~/marsdev
BLASTEM=/opt/blastem/blastem
CLOWNMDEMU=/opt/clownmdemu/clownmdemu-frontend
EDPRO_PORT=/dev/ttyACM0  # port USB Mega Everdrive Pro
```

## Notes debug BlastEm + GDB

- Repo officiel : `hg clone https://www.retrodev.com/repos/blastem` (Mercurial, Michael Pavone)
- Build : `make -j$(nproc) CFLAGS="-O2 -g" CPU_FLAGS=""`
- BlastEm pipe mode (Linux) : `target remote | blastem rom.bin -D`
- BlastEm socket mode : lancer `blastem rom.bin -D`, puis `target remote :1234`
- `set substitute-path /sgdk $GDK` pour remapper les sources SGDK
- KDebug s'affiche dans le terminal où tourne BlastEm (stderr)

## Skills Claude (scripts/claude-skills/)

Fichiers de contexte expert injectés automatiquement dans le prompt Claude.
Sélection auto selon le contenu analysé (erreurs build, log KDebug...).

| Skill | Contenu | Triggers |
|-------|---------|----------|
| `m68k.md` | instruction set M68000, registres, cycles, patterns | fichiers .s, registres D0-D7, exceptions |
| `z80.md` | Z80 MD, bus arbitration, drivers son | YM2612, XGM, BUSREQ |
| `sgdk.md` | API SGDK 2.x, rescomp, build flags | genesis.h, VDP_, SPR_, makefile.gen |
| `megadrive.md` | hardware MD, carte mémoire, VDP, timing | VRAM, C00000, A130xx |

### Utilisation

```bash
# Analyse erreurs build (auto-détection skills + auto-détection tokens)
make debug 2>&1 | python3 scripts/claude-build-assist.py

# Mode caveman : réponse ≤5 lignes, pas de markdown, 256 tokens max
make debug 2>&1 | python3 scripts/claude-build-assist.py --caveman

# Forcer des skills spécifiques
python3 scripts/claude-build-assist.py --mode klog --file /tmp/kdebug.log --skills sgdk,megadrive

# Analyser output ASM 68k
python3 scripts/claude-build-assist.py --mode asm --file out/rom.asm

# Voir skills disponibles
python3 scripts/claude-build-assist.py --mode skills-info

# Sans skills (prompt minimal, moins de tokens)
make debug 2>&1 | python3 scripts/claude-build-assist.py --no-skills --caveman
```

## Notes ClownMDEmu

- Pas de GDB — utiliser pour inspection visuelle VDP uniquement
- KDebug affiché dans la fenêtre ImGui "Debug Log"
- VRAM/CRAM/VSRAM exportables en fichier binaire depuis les menus

## Notes Mega Everdrive Pro

- Protocol USB différent du X7 (wrapping de commandes)
- Outil : megalink-rs (`cargo install megalink`)
- KDebug hardware : ROM doit écrire via $A130E2 (SSF mapper USB register)
- GDB stub hardware : custom 68k stub (voir scripts/stub/) + proxy RSP (scripts/gdb-proxy.py)

## Conventions de code

- SGDK 2.x — utiliser les nouvelles API (XGM2, etc.)
- Debug flags : `-g -gdwarf-4 -O0` en debug, `-O2` en release
- ROM header : activer SSF mapper si ROM > 512KB OU si debug ED Pro nécessaire
