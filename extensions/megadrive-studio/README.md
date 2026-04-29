# Megadrive Studio

VS Code extension for Sega Mega Drive / Genesis homebrew development.

Companion to the [Megadrive Studio](https://github.com/megadrive-studio/megadrive-studio) workspace,
which bundles SGDK, MARSDEV, BlastEm, ClownMDEmu and Mega Everdrive Pro tooling.

## Features

- Command palette entries for build/run/debug/upload (delegates to `.vscode/tasks.json`)
- Status bar showing current `out/rom.bin` size, refreshes on rebuild
- ROM header inspector (system, copyright, region, serial, checksum, ROM/RAM ranges, SHA-256)
- "Mega Drive ROM" tree view in the Explorer side bar
- Settings for SGDK / MARSDEV / BlastEm / ClownMDEmu / Mega Everdrive Pro paths

## Commands

| Command | Action |
|---|---|
| `MD: Build Debug ROM`              | Runs the `Build: Debug ROM` task (`make debug`) |
| `MD: Build Release ROM`            | Runs `make release` |
| `MD: Run in BlastEm`               | Boots `out/rom.bin` in BlastEm |
| `MD: Run in ClownMDEmu`            | Boots `out/rom.bin` in ClownMDEmu with VDP debug UI |
| `MD: Debug with BlastEm + GDB`     | Starts BlastEm in GDB pipe mode |
| `MD: Upload to Mega Everdrive Pro` | Uploads ROM via `megalink-rs` |
| `MD: Show ROM Header Info`         | Opens a webview with the parsed Mega Drive header |

## Requirements

This extension does **not** ship a toolchain. You need:

- SGDK (≥ 2.x)
- MARSDEV (`m68k-elf-gcc`, `m68k-elf-gdb`)
- BlastEm (built from Mercurial Pavone repo, GDB stub enabled)
- ClownMDEmu (frontend build with Dear ImGui)
- Optional: `megalink-rs` for Mega Everdrive Pro hardware

The Megadrive Studio workspace ships a `setup.sh` that installs all of the above.

## Settings

| Key | Description |
|---|---|
| `megadriveStudio.gdkPath`       | Override `$GDK` |
| `megadriveStudio.marsdevPath`   | Override `$MARSDEV` |
| `megadriveStudio.blastemPath`   | Path to `blastem` binary |
| `megadriveStudio.clownmdemuPath`| Path to `clownmdemu-frontend` |
| `megadriveStudio.edproPort`     | USB port for Mega Everdrive Pro (e.g. `/dev/ttyACM0`) |

## Screenshots

_TODO: status bar / ROM info panel / tree view._

## License

MIT
