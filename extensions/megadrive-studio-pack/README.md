# Megadrive Studio (Extension Pack)

One-click setup of every VS Code extension you need for Sega Mega Drive / Genesis homebrew development with the [Megadrive Studio](https://github.com/rtissera/MegaDriveStudio) toolchain (SGDK + MARSDEV + BlastEm + ClownMDEmu + Mega Everdrive Pro).

Install this single pack and VS Code will pull in all dependencies automatically.

## Install

From the command line:

```bash
code --install-extension megadrive-studio.megadrive-studio-pack
```

Or via the VS Code Marketplace: search for **Megadrive Studio**.

## What's bundled

| Extension | Purpose |
|-----------|---------|
| `zerasul.genesis-code` | SGDK project scaffolding, snippets, and rescomp helpers for Mega Drive / Genesis homebrew. |
| `webfreak.debug` | Native debug adapter (GDB) — used to attach to BlastEm and the Mega Everdrive Pro stub. |
| `ms-vscode.cpptools` | C/C++ language support, IntelliSense, and debugging primitives. |
| `ms-vscode.cpptools-extension-pack` | Companion C/C++ tooling (CMake Tools, themes, etc.). |
| `ms-vscode.makefile-tools` | Makefile language support and integrated build targets for `make debug` / `make release`. |
| `EditorConfig.EditorConfig` | Honor the repo's `.editorconfig` so indentation and line endings stay consistent. |
| `streetsidesoftware.code-spell-checker` | Spellchecker for comments, docs, and commit messages. |
| `GitHub.vscode-github-actions` | Author and inspect the CI workflows under `.github/`. |
| `eamodio.gitlens` | Inline blame, history, and rich Git tooling. |
| `13xforever.language-x86-64-assembly` | Generic assembly syntax highlighting (used here for 68000 `.s` / `.asm` listings). |
| `anthropic.claude-vscode` | Claude Code in-editor integration — pairs with the `scripts/claude-build-assist.py` skills system. |

## Roadmap

- Add a proper `icon.png` (currently a TODO — placeholder referenced by `package.json`).
- Publish to the Marketplace under the `megadrive-studio` publisher.

## License

MIT — see [LICENSE](./LICENSE).

## Links

- Main repository: https://github.com/rtissera/MegaDriveStudio
- Issues / feedback: https://github.com/rtissera/MegaDriveStudio/issues
