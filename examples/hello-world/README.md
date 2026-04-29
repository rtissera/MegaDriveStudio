# Hello World — Megadrive Studio example project

A self-contained SGDK 2.x homebrew skeleton you can poke at, debug, and rebuild
without leaving the IDE. The project ships with a **prebuilt ROM at
`out/rom.bin`** so the live debug views work even if you don't have Docker
installed yet.

This folder is what `./start.sh` opens by default when you launch the
**Megadrive Studio** Linux bundle.

---

## 1. Quickstart (5 steps)

1. **Launch the bundle:** run `./start.sh` from the unpacked
   `megadrive-studio/` folder. VSCodium opens onto **this** workspace
   (`examples/hello-world/`).
2. **Sega activity bar:** look at the left sidebar — there is a Mega Drive
   icon. Click it. You should see panes for **VRAM**, **CRAM**, **Sprites**,
   **VDP Regs**, **68k Regs**, **Memory** and **Breakpoints**. They start
   empty, which is normal until a ROM is loaded.
3. **Load `out/rom.bin`:** the extension auto-spawns the `mds-mcp` server on
   activation. Loading the ROM happens through the MCP tool `mega_load_rom`.
   - From any MCP-capable client (Claude Desktop, the VS Code Claude
     extension, etc.) connected to `sse://127.0.0.1:28765`, call
     `mega_load_rom` with `path = ${workspaceFolder}/out/rom.bin`.
   - Or interact with the Sega activity bar's load action when running the
     extension — `mds-mcp` exposes the same call.
4. **Watch it come alive:** within ~250 ms the panes refresh. VRAM shows the
   tile bank, CRAM the active palette, Sprites the OAM table, the registers
   show live 68k state. The default refresh rate is 4 Hz; tune it with the
   `--ui-refresh-hz` flag on `mds-mcp`.
5. **Debug it:** set a breakpoint by address (Sega: Toggle Breakpoint at
   Cursor on a `.s`/`.c` line, or add one from the Breakpoints pane), step
   instructions with **F11**, continue with **F5**. Take screenshots with
   `mega_screenshot` (returns a PNG of the live framebuffer).

---

## 2. What you can do right now

The Megadrive Studio extension and `mds-mcp` server expose the following
features against any valid Mega Drive ROM:

- **VRAM viewer** — live tile decode, all 2048 tiles, click to jump to a
  byte offset.
- **CRAM viewer** — 64-entry palette, 4 lines × 16 colours.
- **Sprite viewer** — full OAM table, position / tile / palette / priority /
  size.
- **VDP registers** — all 24 registers, decoded.
- **68k registers** — D0–D7, A0–A7, PC, SR, SSP, USP.
- **Memory hex editor** — read & poke any of `RAM`, `VRAM`, `CRAM`, `VSRAM`.
- **Plane viewer** — Plane A / Plane B / Window with tile-by-tile resolution.
- **Breakpoints by address** — wired through the libretro core's debug API
  (M4.1).
- **Step instruction (F11)** / **Continue (F5)**.
- **`mega_screenshot`** — returns a PNG of the live framebuffer.
- **Live updates** throttled at 4 Hz by default
  (`mds-mcp --ui-refresh-hz <n>` to change).

The full MCP tool surface is documented in the project's main README; from
within VSCodium, hover over a tool name in the Claude extension's tool list.

---

## 3. Rebuild from source (optional, requires Docker)

The committed `out/rom.bin` is a snapshot of the SGDK build of `src/main.c`.
To rebuild:

1. **Install Docker** if you haven't:
   <https://docs.docker.com/get-docker/>. The bundle does **not** ship
   Docker — it is too large (~500 MB) to embed.
2. Open the Command Palette (Ctrl+Shift+P) → **Tasks: Run Build Task**, or
   press **Ctrl+Shift+B**.
3. The default task **Build ROM (Docker SGDK)** runs `zerasul/sgdk:2.00` in
   a one-shot container. It mounts this folder at `/src`, runs
   `make -f $GDK/makefile.gen EXTRA_CFLAGS="-O2"`, and drops the result at
   `out/rom.bin`.
4. The debug variant **Build ROM (Debug, Docker SGDK)** adds `-g -gdwarf-4
   -O0` and `-DDEBUG`, useful for symbols + KDebug logging.

If Docker is missing, the task fails with a non-zero exit code; the
`docker: command not found` message in the task output is your cue. Install
Docker and rerun.

The same `zerasul/sgdk:2.00` image is what CI uses to produce the
release-tagged ROM, so local and CI builds match bit-for-bit (modulo
`ROM_VERSION`).

---

## 4. Use without the bundle

If you cloned the `MegaDriveStudio` git repo directly (no bundle):

1. Build `mds-mcp` and the libretro core:
   ```bash
   git submodule update --init --recursive
   cd vendor/clownmdemu-libretro && make -j"$(nproc)" && cd ../..
   cd mds-mcp && cargo build --release && cd ..
   ```
2. Open this folder in your editor.
3. Override `megadriveStudio.mdsMcpBinary` in your User settings — point it
   at the just-built `mds-mcp/target/release/mds-mcp`, or leave it empty so
   the extension falls back to `$PATH` / `$workspace/mds-mcp/target/release/mds-mcp`.
4. The extension's `mcpAutoSpawn` (default true) takes care of the rest.

---

## 5. Troubleshooting

- **No Sega activity bar icon.** The extensions weren't installed. Check
  the Extensions view (Ctrl+Shift+X) — `Megadrive Studio` and `Megadrive
  Studio Pack` should be present and enabled. From inside the bundle
  they are pre-installed under `data/extensions/`; if missing, reinstall
  via the Command Palette → `Extensions: Install from VSIX...` and pick the
  `.vsix` files (or just reuse the bundle).
- **Activity bar shows but panes are empty / "MCP not connected".** The
  `mds-mcp` server didn't spawn or didn't bind. Check the extension log
  (Output panel → **Megadrive Studio**) and `data/user/logs/`. Verify the
  binary path:
  ```bash
  ls -la "$BUNDLE_DIR/bin/mds-mcp"
  "$BUNDLE_DIR/bin/mds-mcp" --help
  ```
  Then confirm the `megadriveStudio.mdsMcpBinary` setting in this
  workspace's `.vscode/settings.json` resolves to the right place
  (it's relative to `${workspaceFolder}` — for the bundle layout
  that's `examples/hello-world/../../bin/mds-mcp`).
- **ROM doesn't load.** Verify the file is a valid Mega Drive image:
  ```bash
  file out/rom.bin
  # → Sega Mega Drive / Genesis ROM image: "..."
  xxd -s 0x100 -l 16 out/rom.bin
  # → 5345 4741 …  ("SEGA MEGA DRIVE ")
  ```
  If header offset 0x100 doesn't read `SEGA MEGA DRIVE` / `SEGA GENESIS`,
  the ROM is malformed. The bundled `out/rom.bin` is verified at release
  time — re-extract the bundle if it got corrupted.
- **F5 does nothing.** F5 is bound to `megadriveStudio.continue`, which
  only runs the emulator forward when a ROM is loaded and execution is
  halted. Load a ROM first.
- **Build task: `docker: command not found`.** Install Docker
  (<https://docs.docker.com/get-docker/>) and re-run the task.
- **Build task: permission denied on Docker socket.** Add your user to
  the `docker` group: `sudo usermod -aG docker $USER`, then log out/in.

---

## 6. License

This example project (`src/main.c`, `Makefile`, `.vscode/*`, this README)
is MIT-licensed under the Megadrive Studio project. See `LICENSES/` in the
bundle root, or `mds-mcp/LICENSE` in the source repo.
