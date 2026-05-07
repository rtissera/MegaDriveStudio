# M5 — Mega Everdrive Pro hardware target

Status: design doc. No code shipped under `mds-mcp/src/target/edpro/` yet (a parallel
agent is scaffolding it). Citations verified against live sources unless marked
`[SPECULATION]`. Companion to `/tmp/mds-m5-research.md` (full research) and
`memory/m5_edpro_research.md` (distilled findings).

---

## 1. Goal and scope

M5 ships the EdPro hardware target behind the same 26-tool MCP surface as the
M2 emulator target. One CLI flag — `--target edpro --port /dev/everdrive` —
picks the backend; every layer above the transport is target-agnostic.

In scope: USB transport (CDC serial, 4-byte command framing, 1024-byte ACK
chunking); ROM upload via `MEM_WR`; `HOST_RST`; KDebug-over-USB ROM-side
`kprintf` shim that goes through the FIFO at `$A130D0`; 68k debug stub
(hybrid `TRAP #1` BP + T-bit step) speaking RSP into the FIFO; a `MockUsb`
so 12/15 implementation tasks unit-test without hardware.

Out of scope: mass-flash / FPGA reload (`FPG_USB`, `FLA_WR`); SD file I/O
(`F_FOPN` etc.); service-mode UI; Mega Everdrive X3/X5/X7 (different
protocol); hardware watchpoints (68000 has no comparator — degrade to polled
WP during T-bit step); state save / rewind (hw can't).

---

## 2. Hardware target overview

The Mega Everdrive Pro is a flash cart by Krikzz. Block diagram of the parts
that matter to M5:

```
   USB-CDC
  (host PC) <--ttyACM0--> [cart MCU]  <----PI bus----> [16 MB PSRAM] -- 68k bus --> Mega Drive
                              |                         (ROM area)
                              |                        [SRAM 512 KB]
                              |                        [BRAM 512 KB]
                              |                        [CFG / FIFO / MAP regs]
                              +---------- on-cart FIFO ($A130D0..$A130D4
                                          on the 68k bus)
```

Properties driving the M5 design:

- The 68k sees PSRAM as ROM at `$000000-$3FFFFF` (mappable to 16 MB via SSF).
  PSRAM is RAM-electrical: the host rewrites any "ROM" word at runtime via
  `MEM_WR`, which is what makes `TRAP #1` software BPs work in ROM code.
- The MCU is the only thing that talks USB. The 68k reaches it through a
  128-byte hardware FIFO at `$A130D0/D2/D4`.
- SSF mapper enabled by writing `0x2A` to `$A130F1` at boot. SGDK does this
  when `MODULE_EVERDRIVE` is wired (Pro auto-enable [SPECULATION]; X7 needs
  explicit enable).
- Two cart modes: **service** (boot menu) and **app** (user ROM). M5 only
  operates in app mode; mode-switch USB re-enum is to be avoided during
  debug.

---

## 3. Address map

### 3.1 68k-side cart registers (`$A130xx`)

All values verified against
[krikzz/mega-ed-pub edio-mega/everdrive.h](https://github.com/krikzz/mega-ed-pub/blob/master/edio-mega/everdrive.h)
and the
[ED Pro extended-SSF doc](https://krikzz.com/pub/support/mega-everdrive/pro-series/extended-ssf.txt).

| Addr      | Name           | Width | Direction | Semantics                                                            |
|-----------|----------------|-------|-----------|----------------------------------------------------------------------|
| `$A130D0` | `REG_FIFO_DATA`| u16   | r/w       | Host-cart mailbox. Read pulls one byte; write pushes one byte.       |
| `$A130D2` | `REG_FIFO_STAT`| u16   | r         | Bit 15 (`FIFO_CPU_RXF`) = CPU has data to read. Bits 0..10 = byte count available (`FIFO_RXF_MSK = 0x7FF`). |
| `$A130D4` | `REG_SYS_STAT` | u16   | r         | System status (USB connected, mode flags). Bit semantics TBD.        |
| `$A130F1` | `SSF_CTRL`     | u8    | w         | CTRL0 of extended-SSF (krikzz `extended-ssf.txt`). Bit `W` enables 68k-side PSRAM writes; mapper enable. |

> **Retraction (per fact-check C21/C23):** earlier drafts of this doc
> listed `$A130E2` as `SSF_BANK_E`. That is **wrong**. Per Plutiedev's
> `/a130xx-usage` allocation map, `$A130D0-$A130E7` is **EverDrive
> private space, undocumented**. The actual SSF mapper bank registers
> live at `$A130F0-$A130FE` (word) / odd-byte-mirror `$A130F1, F3, ...,
> FF` per `extended-ssf.txt`. Do NOT touch `$A130E2`; its semantics
> are not in any primary source.

### 3.2 Host-side PI bus address space (used as `addr` arg of `MEM_RD`/`MEM_WR`)

| Addr        | Region | Size   | Notes                                            |
|-------------|--------|--------|--------------------------------------------------|
| `0x0000000` | ROM    | 16 MB  | PSRAM, writable. Where the user ROM lives.       |
| `0x1000000` | SRAM   | 512 KB | Cart save SRAM.                                  |
| `0x1080000` | BRAM   | 512 KB | Battery RAM.                                     |
| `0x1800000` | CFG    | —      | System config. ACK gating threshold for `MEM_WR`.|
| `0x1810000` | FIFO   | —      | MCU↔68k mailbox (mirrors `$A130D0` from MCU side).|
| `0x1830000` | MAP    | —      | Mapper regs.                                     |

`MEM_WR` to addresses `< $1800000` (i.e. ROM/SRAM/BRAM) **must** ACK-gate every
1024-byte chunk; CFG-area writes are unthrottled.

---

## 4. Host protocol

### 4.1 Wire framing

Every host→cart command is a 4-byte preamble followed by an opcode-specific
payload:

```
byte 0: '+'         = 0x2B          preamble
byte 1: ~'+'        = 0xD4          preamble inverted (MCU validates)
byte 2: CMD                          8-bit opcode
byte 3: ~CMD                         opcode inverted (MCU validates)
byte 4..: payload, big-endian for u16/u32
```

Corrupted preamble = silent drop on the cart side. There is no length prefix at
the frame layer; each opcode knows its own arg size.

Status reply framing (from `CMD_STATUS = 0x10`): cart returns 2 bytes,
`0xA5 status_code`. `0xA5` is `STATUS_KEY`; older firmwares use a different key
constant we do not target.

### 4.2 Opcode table

Verified against
[krikzz/mega-ed-pub edio-mega/everdrive.c](https://github.com/krikzz/mega-ed-pub/blob/master/edio-mega/everdrive.c)
and
[ricky26/megalink-rs src/lib.rs](https://github.com/ricky26/megalink-rs/blob/master/src/lib.rs).

| Op   | Name        | Used in M5? | Payload                                |
|------|-------------|-------------|----------------------------------------|
| 0x10 | STATUS      | yes         | none → reply `A5 status`               |
| 0x11 | GET_MODE    | yes (init)  | none → reply mode byte                 |
| 0x12 | IO_RST      | yes         | none                                   |
| 0x16 | FLA_RD      | no          | flash read (out of scope)              |
| 0x17 | FLA_WR      | no          | flash write (out of scope)             |
| 0x19 | MEM_RD      | yes         | `addr:u32 len:u32` → `data[len]`       |
| 0x1A | MEM_WR      | yes         | `addr:u32 len:u32 ack:u8 data[len]`    |
| 0x1B | MEM_SET     | optional    | `addr:u32 len:u32 val:u8`              |
| 0x1D | MEM_CRC     | optional    | `addr:u32 len:u32` → `crc:u32`         |
| 0x1E | FPG_USB     | no          | FPGA reload                            |
| 0x22 | USB_WR      | yes         | `len:u16 data[len]` (cart→host stream) |
| 0x23 | FIFO_WR     | yes         | MCU buffer write                       |
| 0x29 | HOST_RST    | yes         | `mode:u8` (off/soft/hard)              |
| 0xC9 | F_FOPN      | no          | SD file I/O                            |
| 0xCA-CF | F_*       | no          | SD file I/O                            |
| 0xF1 | RUN_APP     | rare        | leave service mode                     |

### 4.3 ROM upload sample byte sequence

Uploading a 4 KB ROM block to PSRAM (PI-bus offset `0x000000`):

```
+      ~+    CMD   ~CMD  | addr (u32 BE) | len (u32 BE)  | ack-mode
0x2B   0xD4  0x1A  0xE5  | 00 00 00 00   | 00 00 10 00   | AA
                                                          (one-time mode byte)

[wait 1 ack byte from cart, expect 0x00 = OK] | data[0..1024]
[wait 1 ack byte from cart, expect 0x00 = OK] | data[1024..2048]
[wait 1 ack byte from cart, expect 0x00 = OK] | data[2048..3072]
[wait 1 ack byte from cart, expect 0x00 = OK] | data[3072..4096]
```

Per fact-check C11 (krikzz `ed_cmd_mem_wr` + megalink-rs `tx_ack`):

- `0xAA` is the ONE-TIME **mode byte** in the MEM_WR header that requests
  ack-gating. It is sent ONCE for the whole transfer when
  `addr < PI_CFG_BASE` (0x180_0000), i.e. for ROM/SRAM/BRAM writes.
- The per-chunk ACK from the cart is **1 byte where 0x00 = OK and any
  non-zero value is an error code**. This ack arrives BEFORE each 1 KiB
  chunk is transmitted by the host.

CFG-area uploads (`addr >= 0x180_0000`) pass `ack-mode=0x00` and stream
straight through, no per-chunk ACKs.

### 4.3.1 Halt-before-write rule

Per fact-check C20: krikzz's documented pattern is `HOST_RST(Soft)` →
`MEM_WR` → `HOST_RST(Off)`. Issuing `MEM_WR` while the 68k is running
is undocumented and unverified — the MCU may stall the bus or corrupt
PSRAM. **Always halt the CPU first.** This applies to vector-table
patching at `$24`/`$84` too.

### 4.4 Cart→host streaming (`USB_WR`)

`USB_WR` (`0x22`) is unusual: it is the **only** opcode the user ROM emits
to push data back to the host. The cart MCU treats the 68k's writes to
`$A130D0` as opaque bytes and forwards them to the host serial when the user
ROM emits a properly-framed `USB_WR` command into the FIFO. Whether the MCU
also forwards raw FIFO bytes without an explicit `USB_WR` wrapper is
[SPECULATION] — needs scope check.

For M5 we play it safe: the ROM-side KDebug shim emits the full
`+ ~+ 0x22 ~0x22 len data` frame into the FIFO. This matches what
`ed_cmd_usb_wr` does on the MCU side.

> **Retraction (per fact-check C25):** earlier drafts referred to a
> `megalink monitor` / `megalink-rs monitor` subcommand to consume this
> stream on the host. **No such command exists** in megalink-rs (verified
> against `src/bin/megalink.rs`, subcommands: SetMode, Reset, Recover,
> Run, LoadFPGA). The host-side `USB_WR` consumer is to be implemented
> in mds-mcp itself — see §8 task 12 (`kdebug_monitor` MCP tool).

---

## 5. 68k stub design

### 5.1 Deployment model: host-uploaded blob, code in PSRAM, data in work RAM

The stub is **not** linked into the user's SGDK ROM. It is a standalone
position-fixed 68k binary blob (`mds-stub-68k/mdsstub.bin`, ~2 KB after
the M5.4b shrink) that the host (`mds-mcp`) uploads at debug-attach time
using `MEM_WR`. The user ROM has zero awareness the stub exists.

**Crucial address-space distinction (per fact-check C13/C14/C26):**
`MEM_WR addr` operates on the cart **PI-bus** address space, NOT the
68k bus. Mapping:

| PI base | region | 68k visible at |
|---------|--------|----------------|
| `0x000_0000` | ROM (16 MB PSRAM) | `$000000-$3FFFFF` (cart-mapped ROM) |
| `0x100_0000` | SRAM   | (via mapper) |
| `0x108_0000` | BRAM   | (via mapper) |
| `0x180_0000` | CFG    | n/a (MCU-side) |
| `0x181_0000` | FIFO   | n/a (MCU-side; mirrors `$A130D0`) |
| `0x183_0000` | MAP    | n/a (MCU-side) |

There is **no PI-bus alias for MD work RAM** (`$FF0000-$FFFFFF`). The
host therefore CANNOT push state to work RAM via MEM_WR; the only path
to work RAM is via the on-cart stub speaking RSP `M`.

Layout this implies:

- **Code (text + rodata)** lives in cart PSRAM at `PiBusAddr(0x300000)`.
  The 68k sees this as cart-mapped ROM at `$300000`. The 3 MB mark sits
  beyond typical 1-2 MB user ROM images, leaving room for the user ROM
  at PSRAM `0x000000+`. Free real estate — doesn't eat work RAM.
- **Data (BSS)** lives in MD work RAM at `$FFEE00..$FFEFFF` (512 bytes).
  Sits in a no-man's-land between SGDK heap (typically below `$FFD000`)
  and SGDK stack (`$FFFFFE` growing down). User ROMs MUST NOT touch
  this region during a debug session. Trade-off acknowledged: 0.8% of
  work RAM eaten by debug.
- **Vector table** at `$0000-$03FF` lives in PSRAM (because 68k `$0..$3FFFFF`
  is mapped onto PSRAM `0x000000+`). The host writes vectors `$24` /
  `$84` via `MEM_WR PiBusAddr(0x24)` / `PiBusAddr(0x84)`.

Because BSS is referenced by ABSOLUTE 68k addresses (no relocation) and
the host has no way to pre-zero it, the stub zero-initialises BSS
itself on the first exception entry.

The previous M5.4 design uploaded the stub to `$FF8000` via MEM_WR.
That was wrong on two counts: (a) `MEM_WR addr=$FF8000` lands at PSRAM
PI-offset `0xFF8000`, not in 68k work RAM at all; (b) even if the host
had a path to work RAM, putting code there would race with SGDK's heap
and stack. The current design relocates code to PSRAM.

### 5.2 Constraints

- 68000 strict — no MOVEC, no VBR, no comparators. Software
  breakpoints (`TRAP #1`) and T-bit stepping are the only debug
  primitives.
- Cleanroom MIT. Built on top of
  [mborgerson/gdbstub](https://github.com/mborgerson/gdbstub) (MIT) for
  packet framing / checksum / escape patterns. The m68k-specific glue
  (entry asm, register save/restore, BP patch table) is written from
  scratch against the public m68k programmer's reference. **Do not**
  link crossbridge `gdb-7.3/gdb/m68k-stub.c` (GPLv2) anywhere in this
  tree.
- No libc, no `memcpy`/`memset`, no SGDK runtime. The stub is the
  *running* ROM's debugger; the two share no code.

### 5.3 Hybrid TRAP #1 BP + T-bit step

| Mechanism              | Trigger           | Use case             | Cost                |
|------------------------|-------------------|----------------------|---------------------|
| `TRAP #1` (`0x4E41`)   | exec at patched PC| breakpoints          | one-time patch      |
| T-bit set in `SR`      | every instruction | single-step          | ~10× slowdown       |
| Polled WP during step  | every instruction | data watchpoint      | only during step    |

Vector layout (rewritten by the host at debug-attach):

```
$0024  Trace exception   -> __stub_trace_entry    ; T-bit step
$0084  TRAP #1           -> __stub_trap1_entry    ; SW BPs
```

The two entry-point addresses are emitted in a 16-byte header at the
start of the blob (offsets 4 and 8). The host parses this header and
writes the four-byte entry addresses verbatim into vectors `$24` and
`$84`. No JMP-thunk synthesis required — on m68k the vector value
**is** the target PC.

### 5.4 Blob layout

Code at PI-bus `$300000` (PSRAM, 68k-visible at `$300000`). BSS at 68k
`$FFEE00` (work RAM, NOT in the binary blob — zero-initialised by the
stub on first exception entry).

```
PSRAM @ 0x300000  header (16 B):
                    +0x00  u32  MAGIC = 'MDST' (0x4D445354)
                    +0x04  u32  entry_trace      = $3006BC (current build)
                    +0x08  u32  entry_trap1      = $3006D8 (current build)
                    +0x0C  u32  reserved         = 0
PSRAM @ 0x300010  .text + .rodata (~1.95 KB)
PSRAM @ 0x3007B0  __stub_end

WorkRAM @ $FFEE00  __bss_start
                   g_init_cookie  (4 B)   — first-call zero-init latch
                   rsp_payload    (190 B) — decoded RSP payload buffer
WorkRAM @ $FFEEC4  mds_regs[18]   (72 B)  — exception register save
WorkRAM @ $FFEF0C  __bss_end       (= 268 B used; 244 B slack to $FFEFFF)
```

Total flat-bin size: ~2.0 KB (text only — BSS is not materialised).
After "easy wins" shrink:
- BP table dropped (host owns it via `stub_sync.rs`; stub never sees Z0/z0).
- RLE `*N` decode dropped (host's rsp.rs never emits RLE).
- Escape pass on encode dropped (stub-side payloads never contain # $ } *).
- Outbound frame buffer moved to supervisor stack (kilobytes free).
- Single decoded-payload buffer @ 190 B (matches advertised PacketSize).
- `bp.{c,h}` and `rsp.{c,h}` files folded into `stub.c`.

GDB register layout for `g` / `G` on plain m68k (no FPU): 18 longs =
72 bytes, order `D0..D7 A0..A6 USP SR PC`. Matches
`gdb -ex 'set arch m68k:68000'`.

### 5.5 RSP-over-FIFO

Stub speaks raw RSP bytes into `$A130D0`; host serial sees the RSP
stream verbatim. `scripts/gdb-proxy.py` already does the TCP↔serial
passthrough for external GDB sessions.

Whether the stub also needs to wrap each outbound RSP packet in a
`USB_WR` envelope (MCU forwarding rules — §4.4) is open question
§10.Q1; the current stub writes raw RSP into the FIFO, and we'll add
the envelope only if hardware bring-up shows it's needed.

`qSupported` reply: `PacketSize=190;swbreak+;qXfer:features:read-`.
Stub never emits acks; the host's first `+` byte is swallowed during
packet sync. `QStartNoAckMode` returns `OK` immediately.

### 5.6 Host connect sequence

End-to-end attach flow, all driven from `mds-mcp` (per fact-check C20
halt-first rule):

1. `HOST_RST(Soft)` — halts the 68k.
2. `MEM_WR` `mdsstub.bin` to `PiBusAddr(0x300000)` (PSRAM). Per-chunk
   ack-gating active because `addr < PI_CFG_BASE`.
3. Read header bytes [4..12] from the blob → two u32 entry addresses.
4. `MEM_WR` 4 bytes (`entry_trace`) to `PiBusAddr(0x000024)`.
5. `MEM_WR` 4 bytes (`entry_trap1`) to `PiBusAddr(0x000084)`.
6. (Optional) plant a BP at user `main` via host-side `stub_sync` BP table.
7. `HOST_RST(Off)` — releases CPU; user ROM runs from reset.
8. Wait for first stop reply, then `qSupported` / `QStartNoAckMode`.

The stub does **no** initialisation other than zeroing its own BSS on
first entry. There is no `mds_stub_init()` — the host configures
everything before allowing the CPU to execute its first user
instruction.

### 5.7 VDP MMIO access from the stub (M5.7)

CRAM, VSRAM, and VRAM are not memory-mapped on the 68k bus: the only
access path is to write a 32-bit "address-set" command to the VDP control
port at `$C00004`, then loop word-reads from the data port at `$C00000`
(each read auto-advances the VDP's internal address counter).

Encoding (per Plutiedev "VDP Ports" + SGDK `~/mars/m68k-elf/src/vdp.c`):

```
cmd = ((A & 0x3FFF) << 16) | ((A >> 14) & 0x03) | CD
```

with `CD = 0x00` for VRAM_READ, `0x20` for CRAM_READ, `0x10` for
VSRAM_READ. Stub helpers (`vdp_set_addr`, `vdp_read_word`) wrap this
into a typed routine.

VDP registers `$00..$17` are **write-only** on hardware — there is no
MMIO read path. The stub does NOT attempt to fake reg readback. Tools
that need register state (`get_sprites` SAT base, `screenshot` plane A/B
addrs + scroll regs, etc.) are stubbed at NOT_SUPPORTED until M5.8 lands
a host-side VDP shadow (populated at attach time).

Custom monitor commands added in M5.7 (handled by the stub's `q`-packet
dispatcher; reply payloads are hex-encoded raw bytes):

| Packet                          | Reply                                | Wired tool         |
|---------------------------------|--------------------------------------|--------------------|
| `qMdsCram`                      | 256 hex chars (128 bytes of CRAM)    | `get_palettes`     |
| `qMdsVsram`                     | 160 hex chars (80 bytes of VSRAM)    | (M5.8 reserve)     |
| `qMdsVdpStatus`                 | 4 hex chars (status word at `$C00004` read) | (M5.8 reserve) |
| `qMdsVram:<addr>,<len>`         | `2 * min(len, 128)` hex chars        | `dump_tile`        |

`qMdsVram` silently truncates `len` above 128 bytes; the host side
(`StubSync::read_vram`) and the const `VRAM_CHUNK_MAX` codify the cap so
larger reads are chunked by the caller. All response buffers live on the
supervisor stack inside the handler — no BSS impact (BSS stays at 268 B
in the 512 B reserve).

---

## 6. Module layout

Proposed (parallel agent will scaffold). Nothing under `mds-mcp/src/target/`
is finalised yet.

```
mds-mcp/src/target/edpro/
  mod.rs           — Target impl, owns the transport, bridges to RSP
  usb.rs           — Transport trait + MockUsb + SerialUsb (serialport crate)
  framing.rs       — 4-byte cmd framing: + ~+ CMD ~CMD encode/decode
  proto.rs         — typed opcode wrappers: status(), mem_rd(), mem_wr_acked(),
                     usb_wr_recv(), host_rst()
  rsp.rs           — gdb RSP codec: packet build/parse, checksum, $..#xx,
                     escape rules, ack
  stub_sync.rs     — host-side BP table + step FSM mirror; arbitrates
                     Z0/z0/c/s mds-mcp tools onto stub commands
mds-mcp/tests/
  edpro_golden.rs  — replay captured frames against MockUsb
  rsp_codec.rs     — pure-function unit tests for rsp.rs

mds-stub-68k/        — standalone 68k blob, target m68k-elf-gcc (NOT Rust)
  Makefile         — m68k-elf-gcc -m68000 -Os -nostdlib -ffreestanding
                     -T mdsstub.ld; objcopy -O binary
  mdsstub.ld       — linker script: .stub section at PSRAM $300000;
                     .bss section at work RAM $FFEE00 (NOLOAD)
  src/
    entry.s        — 16-byte header + Trace / TRAP #1 entry thunks
    save_regs.s    — exception-frame register save / restore + mds_regs
    stub.c         — single-file RSP dispatcher (BP table dropped — host
                     owns it; rsp/bp folded inline; cleanroom on
                     mborgerson/gdbstub MIT)
    usb.{c,h}      — $A130D0/D2 FIFO read/write
  README.md        — load addr, entry convention, host connect sequence
  → mdsstub.bin    — flat 68k binary, host-uploadable via MEM_WR
  → mdsstub.elf    — same content with symbols (debugging only)
```

**Why C, not Rust:** m68k has no upstream Rust tier-1/2 target. Custom
target JSON + nightly `build-std` works but flaky; `mborgerson/gdbstub`
reference is C anyway → cleanroom port w/o language barrier; `m68k-elf-gcc`
already in marsdev bundle (used by SGDK); smaller binary (no `core` bloat);
hand-rolled buffers cheap when no allocator.

**Why standalone blob, not a static library:** see §5.1. The user ROM
has no idea the stub exists; the host installs it via `MEM_WR` against
work RAM + the vector slots in PSRAM. No linker fragment, no `init()`
call, no public C API.

---

## 7. MCP tool mapping

All 26 tools currently exposed by `mds-mcp/src/tools/mod.rs`. Column meanings:

- **emu** = implemented on the libretro emulator target (M2, shipped).
- **edpro** = M5 plan: `rsp` (translates to GDB RSP packet), `cmd` (uses an
  EdPro USB opcode), `hw-only` (no emulator equivalent), or
  `not_supported_on_target`.

| Tool                          | emu | edpro M5 plan                          | Notes                                   |
|-------------------------------|-----|----------------------------------------|-----------------------------------------|
| `mega_get_status`             |  ✓  | always available                       | reports `connected`, `port`, `rom_loaded` |
| `mega_load_rom`               |  ✓  | `cmd` MEM_WR + HOST_RST                | uploads to PSRAM, then hard reset       |
| `mega_unload_rom`             |  ✓  | `cmd` HOST_RST(off)                    | parks 68k                               |
| `mega_pause`                  |  ✓  | `rsp` Ctrl-C (0x03 in stream)          | needs IRQ injection mechanism — see §10 |
| `mega_resume`                 |  ✓  | `rsp` `c` / `vCont;c`                  |                                         |
| `mega_step_frame`             |  ✓  | `not_supported_on_target`              | "frame" is an emulator concept          |
| `mega_step_instruction`       |  ✓  | `rsp` `s` / `vCont;s` (T-bit)          |                                         |
| `mega_continue`               |  ✓  | `rsp` `c`                              | alias for resume after BP halt          |
| `mega_set_breakpoint`         |  ✓  | `rsp` `Z0,addr,2`                      | exec only; r/w/access degrade           |
| `mega_clear_breakpoint`       |  ✓  | `rsp` `z0,addr,2`                      |                                         |
| `mega_list_breakpoints`       |  ✓  | host-side mirror of stub bp_table      | no RSP packet, read shadow              |
| `mega_read_memory`            |  ✓  | `rsp` `m addr,len`                     | RAM/PSRAM/IO; VDP via stub helper       |
| `mega_write_memory`           |  ✓  | `rsp` `M addr,len:hex`                 | RAM only; PSRAM write goes via MEM_WR   |
| `mega_get_68k_registers`      |  ✓  | `rsp` `g`                              | 18 longs                                |
| `mega_get_z80_registers`      |  ✓  | `not_supported_on_target`              | stub doesn't pause Z80; future work     |
| `mega_get_vdp_registers`      |  ✓  | `rsp` + stub helper reads `$C00004`    | shadow regs at `$FFFD00..` per SGDK     |
| `mega_get_palettes`           |  ✓  | `rsp` `qMdsCram` (M5.7)                | 128 raw CRAM bytes; 9-bit BGR decode on host |
| `mega_get_sprites`            |  ✓  | blocked on VDP reg shadow (M5.8)       | needs SAT base from VDP REG_05 — write-only on hw |
| `mega_dump_tile`              |  ✓  | `rsp` `qMdsVram:idx*32,20` (M5.7)      | 32 bytes / tile (4bpp 8x8)              |
| `mega_screenshot`             |  ✓  | blocked on VDP reg shadow (M5.8)       | needs plane A/B addr + scroll regs (write-only on hw) |
| `mega_save_state`             |  ✓  | `not_supported_on_target`              | hw has no savestate                     |
| `mega_load_state`             |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_set_state`        |  ✓  | `not_supported_on_target`              | physical pad only on hw                 |
| `mega_input_press`            |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_release`          |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_get_state`        |  ✓  | `cmd` MEM_RD `$A10003` (`hw-only`)     | reads pad-1 port directly               |

Counts (post-M5.7): 16 tools mapped to RSP/cmd, 9 `not_supported_on_target`
(2 of those — `get_sprites` and `screenshot` — flip on once the M5.8 VDP
register shadow is reachable), 1 always available. The non-supported tools
return a structured `{ "error": "not_supported_on_target", "target": "edpro" }`
response so the IDE can branch UI without parsing strings.

---

## 8. Hardware-free implementation plan

Order matters: each task lists its dependencies. **HW?** = "needs the cart in
hand to validate". Items 1–12 + 14–15 are testable against `MockUsb` only;
13 is the lone gating item.

| #   | Task                                                       | Files (create / modify)                                    | Depends on | HW? |
|-----|------------------------------------------------------------|------------------------------------------------------------|------------|-----|
|  1  | `EdProTransport` trait                                     | `mds-mcp/src/target/edpro/usb.rs` (new)                    | —          | no  |
|  2  | `MockUsb` golden-frame transport                           | `mds-mcp/src/target/edpro/usb.rs`                          | 1          | no  |
|  3  | 4-byte cmd framing encoder/decoder                         | `mds-mcp/src/target/edpro/framing.rs` (new)                | —          | no  |
|  4  | Typed opcode wrappers (status, mem_rd, mem_wr_acked, etc.) | `mds-mcp/src/target/edpro/proto.rs` (new)                  | 1, 3       | no  |
|  5  | RSP codec (packet, checksum, escape, ack)                  | `mds-mcp/src/target/edpro/rsp.rs` (new)                    | —          | no  |
|  6  | Tool-to-RSP dispatcher (`g`/`G`/`m`/`M`/`c`/`s`/`Z0`/`z0`) | `mds-mcp/src/target/edpro/mod.rs`, `mds-mcp/src/tools/mod.rs` (modify) | 4, 5 | no  |
|  7  | Host-side BP table mirror, atomic patch/restore protocol   | `mds-mcp/src/target/edpro/stub_sync.rs` (new)              | 6          | no  |
|  8  | T-bit step state machine (single-shot trace expected)      | `mds-mcp/src/target/edpro/stub_sync.rs`                    | 6          | no  |
|  9  | 68k stub binary blob (m68k-elf-gcc)                        | `mds-stub-68k/Makefile`, `src/{stub,rsp,usb,bp}.c`, `entry.s`, `mdsstub.ld` | — | no  |
| 10  | Host-side blob deployer (MEM_WR upload + vector patch)     | `mds-mcp/src/target/edpro/{stub_blob,mod}.rs`              | 4, 9       | partial (real-USB only on hw) |
| 11  | KDebug-over-USB shim (replaces SGDK `KDebug_Alert`)        | `mds-stub-68k/src/kdebug.s` (new), SGDK link patch         | 9          | partial (SGDK weak-link Q) |
| 12  | Host `kdebug_monitor` MCP tool                             | `mds-mcp/src/tools/mod.rs` (modify)                        | 4          | no  |
| 13  | E2E smoke test (upload, kprintf, BP hit, single-step)      | `mds-mcp/tests/edpro_e2e.rs` (new)                         | 1–12       | **yes** |
| 14  | Pause-via-host: design IRQ7 trigger from MCU writes        | `mds-stub-68k/src/entry.s`, host `pause` impl              | 9          | yes (mechanism unknown — §10) |
| 15  | Polled watchpoint (during T-bit step)                      | `mds-stub-68k/src/bp.c`, `stub_sync.rs`                    | 7, 8       | partial (perf tuning) |
| 16  | Refactor `scripts/gdb-proxy.py` into mds-mcp tool surface  | `mds-mcp/src/target/edpro/proxy.rs` (new)                  | 5          | no  |
| **M5.7 ✓** | **VDP/CRAM/VSRAM/VRAM helpers in stub + host RSP dispatch** | **`mds-stub-68k/src/stub.c` (qMds* handlers); `mds-mcp/src/target/edpro/{rsp,stub_sync,mod}.rs`** | **5, 9** | **no** |

The CLAUDE.md `Notes Mega Everdrive Pro` section already references
`scripts/gdb-proxy.py` and `scripts/stub/`; (16) folds the Python proxy into
the Rust process so we don't ship two RSP implementations.

---

## 9. Testing strategy

### 9.1 MockUsb golden frames

`MockUsb` implements `EdProTransport` over a `Vec<u8>` of expected outbound
bytes + a queue of canned inbound replies. Each test sets up expected
frames (from captured `megalink-rs --debug` traces or hand-built per spec),
queues replies, runs the call (e.g. `proto::mem_wr_acked`, `rsp::handle_g`),
and asserts the outbound stream matches and the inbound queue drained.
Captures under `mds-mcp/tests/fixtures/edpro/*.bin`.

### 9.2 Replay harness

`SerialUsb` `record` mode writes every byte both ways to a `.bin` file. CI
replays those against `MockUsb`. Regress real-firmware behaviour without
attaching hardware to CI runners.

### 9.3 Stub cross-build CI

`mds-stub-68k` built in CI with `m68k-elf-gcc` (already in
`docker/Dockerfile`). Outputs `.a` + vector-patched `.bin`, checked for
size budget (< 4 KB) and disassembled to validate vector addresses. No
emulation in CI — that's M2.

### 9.4 Missing hardware E2E gap

Item 13 (upload → halt at `main` → step → kprintf round-trip) is
hardware-only. Tracked `xfail` on CI; merge gate is human-in-the-loop
with the cart attached.

---

## 10. Open questions

Only hardware can answer these. Each will block or reshape the indicated
item from §8.

1. **Does writing `$A130D0` from the 68k auto-forward to USB, or must the
   ROM emit the full `+ ~+ 0x22 ~0x22 len data` frame itself?**
   `ed_cmd_usb_wr` in `everdrive.c` sends the full frame, so we plan for
   that. But the cart→host direction may already have a "anything queued
   in FIFO is pushed to USB" path. Affects task 11 (KDebug shim size).
   `[SPECULATION]`
2. **Can the host inject a 68k-visible IRQ?** Needed for `mega_pause` (§7).
   No documented mechanism. Possibilities: (a) MCU writes a magic pattern
   to FIFO and stub polls it in VBLANK; (b) MCU asserts `INT` pin via an
   undocumented register; (c) bridge writes that trigger Bus Error. Affects
   task 14. `[SPECULATION]`
3. **Does SSF protect bit `P` (`$A130F1`) need to be set by the user ROM
   before we can `MEM_WR`-patch the ROM area at runtime for software BPs?**
   `edio-mega` sample sets it once at boot. Affects task 7 timing budget.
4. **`KDebug_Alert` weak-link viability**: the symbol from `SGDK/src/kdebug.s`
   has no `.weak` directive, so naive override at link time fails. Either
   (a) patch SGDK with a `.weak`, (b) bypass `kprintf` and ship our own,
   (c) preprocessor `#define KDebug_Alert mds_klog`. Affects task 11.
5. **USB re-enumeration during debug session**: `megalink-rs` retries 100×
   when the device disappears, suggesting at least mode-switch causes USB
   re-enum. Does normal app-mode debug ever trigger it? If yes, the
   transport must reconnect transparently. Affects task 1.
6. **Latency**: TRAP #1 round-trip target is < 5 ms. Real number unknown
   until measured. Affects task 13 acceptance criteria.

---

## 11. References

External (fetched 2026-05-07):

| Source | License | Use |
|--------|---------|-----|
| [krikzz/mega-ed-pub](https://github.com/krikzz/mega-ed-pub) `edio-mega/everdrive.{h,c}` | none stated — contact Krikzz | opcode table, `ed_cmd_mem_wr`, `ed_cmd_usb_wr` |
| [ricky26/megalink-rs](https://github.com/ricky26/megalink-rs) | not declared — file issue before vendoring | Rust `EverdriveSerial`, framing |
| [rhargreaves/mega-drive-usb-link](https://github.com/rhargreaves/mega-drive-usb-link) | MIT (verify) | X7 vs Pro quirks, throughput |
| [ED Pro extended-SSF doc](https://krikzz.com/pub/support/mega-everdrive/pro-series/extended-ssf.txt) | doc | `$A130xx` semantics |
| [ED Pro user manual](https://krikzz.com/pub/support/mega-everdrive/pro-series/mega-ed-pro-manual.pdf) | doc | mode switching |
| [SGDK `src/kdebug.s`](https://github.com/Stephane-D/SGDK/blob/master/src/kdebug.s) | MIT | proves `KDebug_Alert` writes `$C00004` (emu-only) |
| [mborgerson/gdbstub](https://github.com/mborgerson/gdbstub) | MIT | **cleanroom base for stub** |
| [crossbridge `m68k-stub.c`](https://github.com/adobe-flash/crossbridge/blob/master/gdb-7.3/gdb/m68k-stub.c) | GPLv2 | **reference only, do not link** |
| [Embecosm RSP howto EAN4](https://www.embecosm.com/appnotes/ean4/embecosm-howto-rsp-server-ean4-issue-2.html) | doc | minimal RSP packet set |
| [GDB Remote Protocol manual](https://sourceware.org/gdb/current/onlinedocs/gdb.html/Remote-Protocol.html) | doc | spec |
| [SpritesMind GDB-on-MD page](https://gendev.spritesmind.net/page-gdb.html) | doc | gateway 503 at fetch — kept for record |
| [SpritesMind: Mega Everdrive USB thread](https://gendev.spritesmind.net/forum/viewtopic.php?t=2488) | doc | community protocol notes |
| [SpritesMind: KLog over mega-usb](https://gendev.spritesmind.net/forum/viewtopic.php?t=3164) | doc | KDebug-USB precedent |

In-repo (HEAD `3d9401a`):

- `mds-mcp/src/target/edpro.rs:11-23` — corrected register addresses.
- `mds-mcp/src/target/mod.rs:21-72` — `TargetKind`, `EdProConfig`.
- `mds-mcp/src/tools/mod.rs:206-674` — 26 tool entry points; each calls
  `block_on_edpro("…")` for hardware short-circuit. M5 replaces that helper
  with real RSP / `cmd` dispatch.
- `scripts/gdb-proxy.py:1-30` — TCP↔serial passthrough (wire-correct;
  doc-comment at L7 still mentions wrong `$A130E2` — reworded as part of
  task 16).
- `scripts/kdebug-monitor.sh` — superseded by `kdebug_monitor` MCP tool
  (task 12). Note: there is **no** `megalink monitor` subcommand in
  megalink-rs (per fact-check C25); the host-side USB_WR consumer must
  be implemented in mds-mcp itself.
- `CLAUDE.md` "Notes Mega Everdrive Pro" — high-level orientation; the
  `$A130E2` mention there is being fixed separately.
