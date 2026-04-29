# Megadrive Studio

Environnement de développement complet pour homebrew **Sega Mega Drive / Genesis**.

```
SGDK 2.x + MARSDEV + BlastEm (GDB) + ClownMDEmu (VDP debug) + Mega Everdrive Pro
```

## Installation rapide

```bash
git clone https://github.com/TON_USER/megadrive-studio.git
cd megadrive-studio
chmod +x setup.sh scripts/*.sh
./setup.sh
```

Puis ouvre le dossier dans VS Code.

## Targets de debug

| Target | Outil | Utilisation |
|--------|-------|-------------|
| **A — BlastEm** | F5 "▶ BlastEm GDB (pipe)" | Breakpoints C, step/next, watch variables |
| **B — ClownMDEmu** | Task "Run: ClownMDEmu" | VRAM viewer, sprite debugger, registres VDP/YM2612 |
| **C — Hardware** | `make upload-edpro` + F5 "▶ ED Pro GDB" | Validation cycle-exact, KDebug USB |

## Commandes principales

```bash
make debug          # Build ROM avec symboles
make release        # Build ROM release
make run-blastem    # Lancer BlastEm
make run-clown      # Lancer ClownMDEmu (VDP debug UI)
make upload-edpro   # Flash ROM sur Mega Everdrive Pro
make klog-monitor   # Écoute KDebug USB (ED Pro)
make info           # Affiche l'environnement configuré
```

## Variables d'environnement

```bash
GDK=/opt/sgdk              # chemin SGDK
MARSDEV=/opt/marsdev       # chemin MARSDEV (toolchain m68k)
EDPRO_PORT=/dev/everdrive  # port USB Mega Everdrive Pro
ANTHROPIC_API_KEY=sk-...   # pour l'assistant Claude (optionnel)
```

## CI/CD GitHub Actions

| Workflow | Déclencheur | Action |
|----------|-------------|--------|
| `build.yml` | Push sur main/dev | Build ROM debug + release, smoke test BlastEm, lint |
| `release.yml` | Tag `vX.Y.Z` | Build release + GitHub Release avec ROM |
| `toolchain-cache.yml` | Manuel / hebdo | Pre-build BlastEm pour le cache CI |

## Structure

```
src/          Sources C du homebrew
res/          Ressources SGDK (images, sons, polices)
out/          Build artifacts (rom.bin, rom.elf) — gitignored
.vscode/      Config VS Code (tasks, launch, settings, extensions)
.github/      GitHub Actions CI/CD
scripts/      Outils build/debug/upload
docker/       Dockerfile dev container
```

## Debug KDebug / kprintf

```c
#ifdef DEBUG
  KDebug_Alert("Valeur : %d", ma_variable);
#endif
```

- **BlastEm** : affiché dans le terminal où BlastEm tourne
- **ClownMDEmu** : affiché dans la fenêtre "Debug Log" ImGui
- **Hardware ED Pro** : `make klog-monitor` pour écouter via USB

## Extension Claude

L'assistant Claude est disponible :
- **Sidebar VS Code** : extension `anthropic.claude-vscode`
- **Task "Claude: Explain last build error"** : analyse automatique des erreurs gcc avec l'API
- **Task "Claude: Analyze KDebug log"** : analyse les logs KDebug pour détecter des patterns

## Licences

- SGDK : MIT
- BlastEm : BSD-style
- ClownMDEmu : AGPLv3
- megalink-rs : Apache 2.0
- Ce projet template : MIT
