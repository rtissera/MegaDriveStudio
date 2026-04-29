# mds-mcp

MCP server for **Megadrive Studio** — bridges libretro emulators (clownmdemu) and Mega Drive hardware (Mega Everdrive Pro) to MCP-aware clients (Claude Desktop, the VS Code extension, `mcp-inspector`).

This is the **M1 scaffold**. Three tools live, transport is stdio, the actual emulator loop is stubbed. See [`CHANGELOG.md`](CHANGELOG.md) and `/tmp/mds-phase2-research/05-plan.md` for the full milestone plan.

## Architecture

```
client (Claude / VS Code)  ──stdio JSON-RPC──▶  mds-mcp
                                                  │
                                                  ├── EmulatorActor (M1 stub; M2 = libra frame loop)
                                                  └── FFI ──▶ vendor/libra ──▶ clownmdemu_libretro.so (M2)
```

## Build

```bash
# from repo root, first time only:
git submodule update --init --recursive

# then:
cd mds-mcp
cargo build --release
```

If `vendor/libra/include/libra.h` is missing, `build.rs` will emit a `cargo:warning` and produce stub bindings — `cargo check` stays green so you can iterate on Rust without the C side ready. The actual link will fail with `cannot find -llibra` until libra is built; that's expected during scaffold.

To build libra (parallel work, not in this crate):

```bash
cd vendor/libra && cmake -S . -B build && cmake --build build
```

To override the libra library name (e.g. `liblibra_static.a`):

```bash
LIBRA_LIB_NAME=libra_static cargo build --release
```

## Run

```bash
./target/release/mds-mcp
```

The binary speaks MCP over stdio. Logs go to stderr (stdout is reserved for JSON-RPC frames — anything else there breaks the protocol).

```bash
RUST_LOG=debug ./target/release/mds-mcp 2> mds.log
```

## Tools (M1)

| Tool | Args | Returns |
|---|---|---|
| `mega_load_rom` | `path: string` | `{ ok, size, crc32, header_name }` |
| `mega_pause` | — | `{ ok, frame }` |
| `mega_read_memory` | `space: "ram"\|"vram"\|"cram"\|"vsram"\|"rom"\|"z80"\|"saveram"`, `addr: u32`, `length: u32` | `{ addr, length, space, data: base64 }` |

For M1, only `space="rom"` is wired. Other spaces return a clear "not implemented in M1" error.

## Wire into Claude Desktop

Add to `~/.config/Claude/claude_desktop_config.json` (or the OS equivalent):

```json
{
  "mcpServers": {
    "mds-mcp": {
      "command": "/absolute/path/to/megadrive-studio/mds-mcp/target/release/mds-mcp",
      "args": [],
      "env": {
        "RUST_LOG": "info"
      }
    }
  }
}
```

Restart Claude Desktop, then ask the model: *"Use mega_load_rom to open out/rom.bin and tell me the header name."*

## Test with mcp-inspector

```bash
npx @modelcontextprotocol/inspector ./target/release/mds-mcp
```

This launches a web UI on `http://localhost:5173` where you can inspect the tool list, fire calls manually, and watch the JSON-RPC frames.

## License

MIT — see [`LICENSE`](LICENSE).
