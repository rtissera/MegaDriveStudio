# Changelog

All notable changes to `mds-mcp` are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/), versioning is [SemVer](https://semver.org/).

## 0.1.0 — Initial M1 scaffold

- Project scaffold: Cargo crate, MIT license, SPDX headers.
- `rmcp` (1.5) stdio transport wired with structured logging on stderr.
- Three MCP tools live: `mega_load_rom`, `mega_pause`, `mega_read_memory`.
- `mega_load_rom` validates the Mega Drive header magic at offset 0x100 and reports size + CRC-32 + in-header game name.
- `mega_read_memory` implements the `rom` space only; `ram`/`vram`/`cram`/`vsram`/`z80`/`saveram` return a clear "not implemented in M1" error and unblock tests in M2.
- `EmulatorActor` stub holds the ROM bytes in-process. The libra frame loop is intentionally not started — M1 prints `stub: would run emulator thread` and exits cleanly when stdin closes.
- `build.rs` integrates `bindgen` against `../vendor/libra/include/libra.h`. If the header is absent (parallel agent has not initialised submodules), it emits `cargo:warning` and writes a stub `bindings.rs` so `cargo check` stays green; the link will fail later with a clear message, which is the intended scaffold behaviour.
