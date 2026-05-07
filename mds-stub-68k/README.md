# mds-stub-68k

On-cart Mega Drive 68000 debug stub for the Mega Everdrive Pro target.
Speaks GDB Remote Serial Protocol (RSP) into the cart USB FIFO at
`$A130D0`. The host (Megadrive Studio MCP server) is the GDB-side peer.

License: MIT (cleanroom on
[mborgerson/gdbstub](https://github.com/mborgerson/gdbstub)). **Do not**
link `crossbridge gdb-7.3/gdb/m68k-stub.c` (GPLv2) against any binary in
this tree.

## What this is

A standalone, position-fixed 68k binary blob (`mdsstub.bin`, ~7 KB) that
the host uploads to work RAM at debug-attach time. The user's SGDK ROM
does **not** know the stub exists — there is no library to link, no
header to include, no `init()` to call. Vectors are patched by the host
via `MEM_WR`; the user game runs unmodified until something hits a
software breakpoint or a single-step.

The previous M5.4 design assumed the stub was a static library
(`libmdsstub.a`) linked into the user ROM with a runtime patch to
"$FF0024" — but plain 68000 has no VBR and never reads vectors from RAM,
so that was wrong. EdPro Pro, however, has SDRAM behind the cart "ROM"
window: the cart-side MCU can `MEM_WR` anywhere in 68k space, including
the actual vectors at `$0024` / `$0084`. This deployment model leans on
that.

## Build

```sh
make MARSDEV=$HOME/mars
```

Outputs:

| File          | What it is                                              |
|---------------|---------------------------------------------------------|
| `mdsstub.bin` | Flat binary the host uploads via `MEM_WR`. Position-fixed at `$FF8000`. Includes pre-zeroed BSS — one upload covers code + data + scratch. |
| `mdsstub.elf` | Same content with symbols. Use `m68k-elf-objdump -d mdsstub.elf` to inspect. |
| `mdsstub.map` | Linker map. Useful for confirming `__stub_base`, `__stub_end`, BSS layout. |

## Load address

`$FF8000` (low end of "upper" work RAM).

Mega Drive work RAM is `$FF0000-$FFFFFF` (64 KiB), aliased four times.
SGDK conventions:

- `.bss` and the heap grow up from low addresses (typically below
  `$FF7000` for non-trivial games).
- The supervisor stack starts at `$FFFFFE` and grows down. SGDK's
  default reserves ~2 KiB at the top.

`$FF8000` sits in the middle, leaving:

- ~32 KiB of slack below for SGDK's heap to grow into.
- ~24 KiB above for SGDK's stack to grow down into before colliding
  with our `__stub_end` at ~`$FF9AA0`.

A 7-KiB stub at this address has been the safest bet historically and
matches the spec in `docs/02-m5-architecture.md` §5.3. If a future user
ROM is unusually heap-hungry the host can override the load address by
re-linking the stub against a different `LOAD_ADDR` in `Makefile` +
`mdsstub.ld`.

## Entry-point convention

The first 16 bytes of the blob are a header the host parses to find the
two exception entry points without having to compute offsets:

```
offset  size  field
0x00    u32   MAGIC      = 0x4D445354 ('MDST')
0x04    u32   entry_trace — install at vector $0024
0x08    u32   entry_trap1 — install at vector $0084
0x0C    u32   reserved   = 0
```

Both entry-point fields are absolute 68k addresses (big-endian on disk,
matching the linker output). Because the binary is linked at `$FF8000`,
those fields read e.g. `00 FF 8C 4C` and `00 FF 8C 68` for the current
build.

The host writes those four bytes verbatim into vector slots `$0024`
(Trace) and `$0084` (TRAP #1). On exception, the 68000 reads the long at
the vector and jumps directly there — no JMP-thunk instruction required;
the vector value itself **is** the target PC.

## Host connect sequence

The handshake from "user is running normally" to "host has GDB control"
is entirely host-driven:

1. **Halt the CPU.** Host sends `HOST_RST` (`0x29`) with mode = soft.
   The 68k stops. The cart MCU is still alive on the USB.

2. **Upload the stub blob.** Host `MEM_WR`s `mdsstub.bin` to its load
   address (default `$FF8000`). Stream all bytes — the binary contains
   pre-zeroed BSS so the stub's static buffers are clean on first
   exception.

3. **Read entry-point header from the blob** the host just uploaded
   (or, equivalently, from the local `.bin` file before sending). The
   four bytes at offset 4 are `entry_trace`; the four at offset 8 are
   `entry_trap1`.

4. **Patch vectors.** Two more `MEM_WR`s, each 4 bytes:
   - `MEM_WR` to `$00000024` of the four bytes from `header[4..8]`.
   - `MEM_WR` to `$00000084` of the four bytes from `header[8..12]`.

   The vector words are in cart "ROM" — but EdPro PSRAM is RAM-electrical,
   so `MEM_WR` against `$24` and `$84` works exactly like writing any
   other RAM location.

5. **(Optional) plant a breakpoint at the user's `main()`** so the next
   step lands cleanly. Host issues an RSP `Z0,addr,2` once it's wired up
   (M5.x).

6. **Release the CPU.** Two paths:
   - If the host halted the CPU at step 1 with a soft-reset, send
     `HOST_RST` with mode = off (`0x00`) to let the user ROM start
     running from scratch.
   - If the host attached to a running session and used some other
     pause mechanism, just stop blocking — the CPU was running the
     whole time and will hit the next breakpoint as scheduled.

7. **Wait for the first stop reply.** The next time a TRAP #1 (or trace
   exception, if the user explicitly stepped) fires, the stub will send
   a framed RSP packet (`$T05swbreak:;#xx` or similar). The host's
   `StubSync::handshake` then issues `qSupported` + `QStartNoAckMode`
   and the regular RSP loop takes over.

The stub itself does **no** initialisation. There is no equivalent of
`mds_stub_init()` — the user ROM has zero awareness of the stub, and
the stub assumes the host has set up everything (vectors, SSF write
protect, etc.) before allowing the CPU to execute the first user
instruction.

## What the stub handles

Once installed, the stub responds to the standard GDB RSP packets:

- `g` / `G`: read / write the 18-long m68k register block
  (D0..D7, A0..A6, USP, SR, PC).
- `m` / `M`: read / write 68k memory.
- `Z0,addr,2` / `z0,addr,2`: set / clear software breakpoint
  (writes `TRAP #1` = `0x4E41`, remembers the original word).
- `c` / `s`: continue / single-step (clears or sets the SR T-bit).
- `?`: query halt reason. Replies `T05`.
- `q…` / `Q…`: `qSupported`, `QStartNoAckMode`, `qC`, `qAttached`.
- `k` / `D`: kill / detach (no-op on hardware, returns `OK`).

PacketSize advertised is 400 bytes. Wire format is byte-compatible with
`mds-mcp/src/target/edpro/rsp.rs` — fixtures under
`mds-mcp/tests/fixtures/edpro/` are the source of truth.

## Footprint

```sh
make info
```

Current build (with `-Os -m68000`):

| Region        | Size       |
|---------------|------------|
| text + rodata | ~3.4 KB    |
| BSS           | ~3.4 KB    |
| **flat .bin** | **~6.8 KB** (text + zeroed BSS, single MEM_WR upload) |

BSS dominates because the stub keeps two ~1 KB RSP buffers + a 32-entry
breakpoint table in fixed-size arrays (no `malloc`).

## Internals

| File             | Role                                                   |
|------------------|--------------------------------------------------------|
| `src/entry.s`    | 16-byte blob header + Trace / TRAP #1 entry thunks     |
| `src/save_regs.s`| `mds_save_regs` / `mds_restore_regs` (D0-D7/A0-A7/SR/PC, exception-frame aware) |
| `src/stub.c`     | RSP dispatcher, packet handlers, `mds_stub_enter_handler` |
| `src/rsp.c/.h`   | RSP codec — wire-compatible with host `rsp.rs`         |
| `src/usb.c/.h`   | Polled FIFO I/O against `$A130D0/D2`                   |
| `src/bp.c/.h`    | Local 32-entry breakpoint table                        |
| `mdsstub.ld`     | Linker script: single `.stub` section at `$FF8000`     |

## Caveats / open questions

- **Pause from a running CPU**: not yet possible. Needs an MCU→68k IRQ
  injection mechanism (architecture doc §10 Q2). Currently the host can
  only stop the CPU at `HOST_RST` boundaries or at preset breakpoints.
- **Watchpoints**: not implemented. Plan is polled-during-T-bit-step
  (M5.5b). The host's `StubSync` already has the FSM hook.
- **USB envelope vs. raw FIFO**: open question §10 Q1 in the
  architecture doc. The stub currently writes raw RSP bytes into the
  FIFO (no `+~+ 0x22 ~0x22 len ...` framing). If real hardware turns
  out to require the envelope, wrap `usb_send_buf` accordingly — the
  RSP layer doesn't care.
- **First-run latency**: untested on real hardware. M5.5b smoke test
  will measure round-trip time for `g` and revisit the spin-wait in
  `usb.c` if needed.

## Pre-existing M5.4 stub: what changed

| Before                                    | Now                                       |
|-------------------------------------------|-------------------------------------------|
| Static lib `libmdsstub.a`, linked into ROM| Standalone `mdsstub.bin` uploaded by host |
| `mds_stub_init()` called from user `main` | No public C API; host is the initializer  |
| `link.ld.frag` for vector overrides       | Host writes vectors via `MEM_WR`          |
| Runtime patch to `$FF0024` (broken: 68k has no VBR) | Host patches actual vectors at `$24`, `$84` in PSRAM |
| `mds_stub.h` public header                | None — user ROM never sees the stub       |
