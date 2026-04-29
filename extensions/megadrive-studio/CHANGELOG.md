# Changelog

All notable changes to the Megadrive Studio VS Code extension.

## 0.1.0 — Initial release

- Command palette: build debug/release, run BlastEm, run ClownMDEmu,
  debug BlastEm + GDB, upload to Mega Everdrive Pro, show ROM info
- Status bar item showing `out/rom.bin` size, with click-through to the ROM info panel
- ROM header parser (system, copyright, names, serial, checksum, I/O support,
  ROM/RAM range, region, SHA-256) shown in a webview
- Explorer tree view "Mega Drive ROM" with size / region / system / serial / SHA
- Filesystem watcher on `**/out/rom.bin` keeping status bar and tree view live
- Settings for SGDK / MARSDEV / BlastEm / ClownMDEmu / Mega Everdrive Pro paths
- "Mega Drive" activity bar container scaffold
