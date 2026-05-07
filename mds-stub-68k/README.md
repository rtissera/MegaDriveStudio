# mds-stub-68k

On-cart Mega Drive 68000 debug stub for the Mega Everdrive Pro target.
Speaks GDB Remote Serial Protocol (RSP) into the cart USB FIFO at
`$A130D0`. The host (Megadrive Studio MCP server) is the GDB-side peer.

License: MIT (cleanroom on
[mborgerson/gdbstub](https://github.com/mborgerson/gdbstub)). **Do not**
link `crossbridge gdb-7.3/gdb/m68k-stub.c` (GPLv2) against any binary in
this tree.

## What this is

A standalone, position-fixed 68k binary blob (`mdsstub.bin`, ~2 KB) that
the host uploads to cart PSRAM at debug-attach time. The user's SGDK ROM
does **not** know the stub exists — there is no library to link, no
header to include, no `init()` to call. Vectors are patched by the host
via `MEM_WR`; the user game runs unmodified until something hits a
software breakpoint or a single-step.

## Address-space facts (per fact-check C13/C14/C26)

The original M5.4 design uploaded the stub to `$FF8000` via `MEM_WR`.
Per `/tmp/mds-edpro-factcheck.md` C13/C14/C26 that is wrong: `MEM_WR addr`
is the cart **PI-bus** address space, NOT the 68k bus, and there is no
documented PI-bus alias for MD work RAM. Writing `addr=$FF8000` lands
at PSRAM offset `0xFF8000`, not in 68k work RAM at all.

The current design splits the stub:

- **Code (text + rodata)** at PI-bus `$300000` (PSRAM, mapped to 68k
  `$300000`). Free real estate beyond typical user ROM sizes.
- **Data (BSS)** at 68k `$FFEE00..$FFEFFF` (512 bytes). Reserved
  no-man's-land between SGDK heap and stack. Not in the binary blob —
  the stub zero-initialises BSS itself on first exception entry.
- **Vectors** at PI-bus `$24` / `$84` (PSRAM-mapped on the 68k side).

## Build

```sh
make MARSDEV=$HOME/mars
```

Outputs:

| File          | What it is                                              |
|---------------|---------------------------------------------------------|
| `mdsstub.bin` | Flat binary the host uploads via `MEM_WR` to `PiBusAddr(0x300000)`. Code only — BSS is not materialised. |
| `mdsstub.elf` | Same content with symbols. Use `m68k-elf-objdump -d mdsstub.elf` to inspect. |
| `mdsstub.map` | Linker map. Useful for confirming `__stub_base`, `__stub_end`, `__bss_start`/`__bss_end`. |

## Load addresses

| Region | Address (68k bus) | Size | Notes |
|--------|-------------------|------|-------|
| code   | `$300000` (= PI-bus `$300000`) | ~2 KB | cart PSRAM; uploaded by host |
| BSS    | `$FFEE00`         | 512 B (192 used) | MD work RAM; zero-init on first entry |
| vec $24| `$000024` (= PI-bus `$24`) | 4 B | Trace exception vector |
| vec $84| `$000084` (= PI-bus `$84`) | 4 B | TRAP #1 exception vector |

If a future user ROM exceeds 3 MB the host can re-link with a higher
PSRAM offset; touch the `psram_stub` ORIGIN in `mdsstub.ld` and the
`STUB_LOAD_ADDR` constant in `mds-mcp/src/target/edpro/stub_blob.rs`.

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
matching the linker output). Because the binary is linked at `$300000`,
those fields read e.g. `00 30 06 BC` and `00 30 06 D8` for the current
build.

The host writes those four bytes verbatim into vector slots `$0024`
(Trace) and `$0084` (TRAP #1) via `MEM_WR PiBusAddr(0x24)` /
`PiBusAddr(0x84)`. On exception, the 68000 reads the long at the
vector and jumps directly there — no JMP-thunk instruction required;
the vector value itself **is** the target PC.

## Host connect sequence

The handshake from "user is running normally" to "host has GDB control"
is entirely host-driven (per fact-check C20 halt-first rule):

1. **Halt the CPU.** Host sends `HOST_RST(Soft)` (`0x29`, mode 1).
   The 68k stops. The cart MCU is still alive on the USB.

2. **Upload the stub blob.** Host `MEM_WR`s `mdsstub.bin` to
   `PiBusAddr(0x300000)`. Per-chunk ack-gating active because
   `addr < PI_CFG_BASE` (`0x180_0000`).

3. **Read entry-point header from the blob** the host just uploaded
   (or, equivalently, from the local `.bin` file before sending). The
   four bytes at offset 4 are `entry_trace`; the four at offset 8 are
   `entry_trap1`.

4. **Patch vectors.** Two more `MEM_WR`s, each 4 bytes:
   - `MEM_WR` to `PiBusAddr(0x24)` of the four bytes from `header[4..8]`.
   - `MEM_WR` to `PiBusAddr(0x84)` of the four bytes from `header[8..12]`.

   The vectors live in PSRAM (cart "ROM"), which is the same medium
   as the stub code — both reachable from the host.

5. **(Optional) plant a breakpoint at the user's `main()`** so the next
   step lands cleanly. Host issues an RSP `Z0,addr,2` once it's wired
   up — the host owns the BP table and patches the original word with
   `TRAP #1` (`0x4E41`) via PSRAM `MEM_WR`.

6. **Release the CPU.** Host sends `HOST_RST(Off)` (mode 0). The user
   ROM starts running from scratch (or from wherever the soft-reset
   left it).

7. **Wait for the first stop reply.** The next time a TRAP #1 (or trace
   exception) fires, the stub will send a framed RSP packet
   (`$T05swbreak:;#xx` or similar). The host's `StubSync::handshake`
   then issues `qSupported` + `QStartNoAckMode` and the regular RSP
   loop takes over.

The stub itself does no initialisation other than zeroing its own BSS
on first call — the user ROM has zero awareness of the stub, and the
stub assumes the host has set up everything (vectors, etc.) before
allowing the CPU to execute the first user instruction.

## What the stub handles

Once installed, the stub responds to the standard GDB RSP packets:

- `g` / `G`: read / write the 18-long m68k register block
  (D0..D7, A0..A6, USP, SR, PC).
- `m` / `M`: read / write 68k memory.
- `c` / `s`: continue / single-step (clears or sets the SR T-bit).
- `?`: query halt reason. Replies `T05`.
- `q…` / `Q…`: `qSupported`, `QStartNoAckMode`, `qAttached`.
- `k` / `D`: kill / detach (no-op on hardware, returns `OK`).

**Software breakpoints are HOST-SIDE.** The stub does NOT handle `Z0` /
`z0`; the host's `StubSync::BreakpointTable` owns the table and patches
PSRAM via plain `M` (writing `TRAP #1` = `0x4E41` for set, restoring
the saved word for clear). When a TRAP #1 fires, the stub rolls PC
back by 2 (the `TRAP #1` is one word) and lands the host on the BP
address. The host is responsible for restoring the original opcode
before the next `c` or `s`.

PacketSize advertised is 190 bytes. Wire format is byte-compatible with
`mds-mcp/src/target/edpro/rsp.rs`.

## Custom VDP queries (M5.7)

CRAM, VSRAM, and VRAM are not memory-mapped on the 68k bus, so a plain
`m addr,len` won't reach them. The stub adds four custom monitor
commands under the `qMds<Name>` namespace. Reply payloads are
hex-encoded raw bytes (`parse_hex_bytes`-compatible) except for
`qMdsVdpStatus`, which is 4 raw hex digits.

| Packet                  | Reply (raw bytes after host hex-decode)              | Notes                                       |
|-------------------------|------------------------------------------------------|---------------------------------------------|
| `qMdsCram`              | 128 bytes (64 9-bit BGR colour entries)              | always reads CRAM addr 0..63                |
| `qMdsVsram`             | 80 bytes (40 vertical-scroll word entries)           | always reads VSRAM addr 0..39               |
| `qMdsVdpStatus`         | 2 bytes (status word from `$C00004` read, big-endian) | vblank/hblank/sprite collision/dma busy    |
| `qMdsVram:<a>,<l>`      | up to 128 bytes from VRAM addr `a`                   | `l > 128` is silently truncated; odd `l` rounded up |

Implementation: each handler writes the 32-bit address-set command to
`$C00004`, then loops word reads from `$C00000`. Encoding follows
SGDK's `vdp.c` and Plutiedev's "VDP Ports" reference:

```
cmd = ((A & 0x3FFF) << 16) | ((A >> 14) & 0x03) | CD
   CD = 0x00 (VRAM_READ) | 0x20 (CRAM_READ) | 0x10 (VSRAM_READ)
```

VDP registers `$00..$17` are **write-only on hardware** (no MMIO
readback path), so the stub does not attempt to expose them — tools
that need register state will pick them up from a host-side reg shadow
in M5.8.

Outbound buffers for these handlers live on the supervisor stack
(kilobytes free below `$FFFFFE`); BSS is unchanged at 268 B.

## Footprint

```sh
make info
```

Current build (with `-Os -m68000`):

| Region        | Size                       |
|---------------|----------------------------|
| text + rodata | ~2.7 KB (in cart PSRAM)    |
| BSS           | 268 B used / 512 B reserved (in MD work RAM) |
| **flat .bin** | **~2.7 KB** (text only — BSS not materialised) |

History:
- **M5.4b "easy wins" shrink:** BP table dropped (host-owned), RLE
  expansion dropped, escape encode dropped, single decoded-payload
  buffer in BSS, outbound encode buffer on supervisor stack. The
  in-binary BSS stayed the same on disk because BSS is a NOLOAD
  section. Footprint: text=1968 B / BSS=268 B.
- **M5.7 VDP helpers:** added four `qMds*` handlers (CRAM/VSRAM/VDP
  status / VRAM chunk) plus a stack-buffered `send_hex_framed`
  variant for replies up to 256 hex chars. text=2740 B / BSS=268 B.

## Internals

| File             | Role                                                   |
|------------------|--------------------------------------------------------|
| `src/entry.s`    | 16-byte blob header + Trace / TRAP #1 entry thunks     |
| `src/save_regs.s`| `mds_save_regs` / `mds_restore_regs` (D0-D7/A0-A7/SR/PC, exception-frame aware), `mds_regs[18]` BSS reservation |
| `src/stub.c`     | Single-file RSP dispatcher: hex helpers, packet codec, handlers, `mds_stub_enter_handler`, BSS first-entry zero-init |
| `src/usb.c/.h`   | Polled FIFO I/O against `$A130D0/D2`                   |
| `mdsstub.ld`     | Linker script: `.stub` section at PSRAM `$300000`; `.bss` at work RAM `$FFEE00` (NOLOAD) |

## Caveats / open questions

- **Pause from a running CPU**: not yet possible. Needs an MCU→68k IRQ
  injection mechanism (architecture doc §10 Q2). Currently the host can
  only stop the CPU at `HOST_RST` boundaries or at preset breakpoints.
- **Watchpoints**: not implemented. Plan is polled-during-T-bit-step.
- **USB envelope vs. raw FIFO**: open question. The stub currently
  writes raw RSP bytes into the FIFO (no `+ ~+ 0x22 ~0x22 len ...`
  framing). If real hardware turns out to require the envelope, wrap
  `usb_send_buf` accordingly.
- **First-run latency**: untested on real hardware.
- **Halt-before-write**: per fact-check C20, krikzz pattern is always
  HOST_RST(Soft) before MEM_WR. Concurrent MEM_WR while CPU runs is
  undocumented and unverified.
- **`$FFEE00..$FFEFFF`**: stub-reserved during a debug session. User
  ROMs MUST NOT use this region. Trade-off: 0.8% of work RAM eaten.

## Pre-existing M5.4 stub: what changed

| Before                                    | Now                                       |
|-------------------------------------------|-------------------------------------------|
| Code+BSS uploaded to `$FF8000` via `MEM_WR` (broken: PI-bus `$FF8000` is PSRAM offset, not 68k work RAM) | Code at PSRAM `$300000`, BSS at work RAM `$FFEE00` (zero-init on first call) |
| BP table mirrored in stub (`bp.{c,h}`)    | BP table host-owned only (`stub_sync.rs`) |
| Z0/z0 packet handlers                     | Dropped — host uses plain `M` to patch    |
| Three 1 KB BSS buffers (rx_raw + payload + out) | One 190 B BSS buffer + supervisor-stack encode |
| RLE `*N` decode                           | Dropped (host's rsp.rs never emits RLE)   |
| Escape pass on encode                     | Dropped (stub payloads never have # $ } *)|
| Separate `rsp.{c,h}`                      | Folded inline in `stub.c`                 |
| text=3412 / BSS=3404 / blob=6816          | text≈1968 / BSS=268 / blob≈1968           |
