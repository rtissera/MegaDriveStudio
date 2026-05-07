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
| `$A130F1` | `SSF_CTRL`     | u8    | w         | SSF mapper enable. Write `0x2A` once at boot to enable extended-SSF. |
| `$A130E2` | `SSF_BANK_E`   | u8    | w         | SSF mapper bank register. **Not USB.** Old project memory had this   |
|           |                |       |           | wrong; do not use for I/O.                                           |

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

Uploading a 4 KB ROM block to PSRAM offset `0x000000`:

```
+      ~+    CMD   ~CMD  | addr (u32 BE) | len (u32 BE)  | ack
0x2B   0xD4  0x1A  0xE5  | 00 00 00 00   | 00 00 10 00   | AA
data[0..1024]   <-- wait for 1 byte ACK from cart
data[1024..2048] <-- wait for 1 byte ACK
data[2048..3072] <-- wait for 1 byte ACK
data[3072..4096] <-- wait for 1 byte ACK
```

`ack=0xAA` enables per-1024-byte gating because `addr < 0x1800000`. CFG-area
uploads (`addr >= 0x1800000`) pass `ack=0x00` and stream straight through.

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

---

## 5. 68k stub design

### 5.1 Constraints

68000 has no VBR (added in 68010). Vectors live at `$000000-$0003FF`, in
PSRAM-as-ROM. Cannot relocate at runtime; at link time we point them at code
that lives in 68k work RAM (`$FF0000-$FFFFFF`, copied there by SGDK init).
No on-CPU comparator either — software BPs and T-bit stepping are the only
options.

### 5.2 Hybrid TRAP #1 BP + T-bit step

| Mechanism              | Trigger           | Use case             | Cost                |
|------------------------|-------------------|----------------------|---------------------|
| `TRAP #1` (`0x4E41`)   | exec at patched PC| breakpoints          | one-time patch      |
| T-bit set in `SR`      | every instruction | single-step          | ~10× slowdown       |
| Polled WP during step  | every instruction | data watchpoint      | only during step    |

Vectors patched at link time in the user ROM image:

```
$0024  Trace exception   -> stub_trace        ; T-bit step
$0084  TRAP #1           -> stub_breakpoint   ; SW BPs
$00C4  Level-7 IRQ       -> stub_break        ; pause [SPECULATION]
```

### 5.3 RAM layout

Stub installed in the top of work RAM at boot (SGDK `_start` calls
`mds_stub_init` before `main`):

```
$FF8000   stub code      (~4 KB, position-independent)
$FF8FFC   stub stack top (grows down to ~$FF8C00)
$FF9000   bp_table[]     (32 entries × 8 bytes: addr u32 + orig_word u16 + flags u16)
$FF9100   reg_save       (D0..D7 A0..A7 SR PC = 18 longs = 72 bytes)
$FF9148   rsp_buf_in     (512 bytes)
$FF9348   rsp_buf_out    (512 bytes)
$FF9548   step_state     (FSM byte: idle/single/cont)
```

GDB register layout for `g`/`G` on plain m68k (no FPU): 18 longs = 72 bytes,
order `D0 D1..D7 A0..A7 SR PC`. Match `gdb -ex 'set arch m68k:68000'`.

### 5.4 RSP-over-FIFO

Stub speaks raw RSP bytes into `$A130D0`; host serial sees the RSP stream
verbatim; `scripts/gdb-proxy.py` already does the TCP↔serial passthrough.
The stub still wraps each outbound RSP packet in a `USB_WR` envelope
(MCU only forwards `USB_WR` payloads — §4.4), but the RSP layer itself
doesn't see the framing.

`qSupported` reply: `PacketSize=400;qXfer:features:read-`. Acks (`+`/`-`)
enabled.

### 5.5 License

**Do not** copy `crossbridge gdb-7.3/gdb/m68k-stub.c` (GPLv2). Linking it into
a user ROM contaminates the ROM. M5 stub is **cleanroom** built on top of
[mborgerson/gdbstub](https://github.com/mborgerson/gdbstub) (MIT) — that gives
us packet framing / checksum / escape; the m68k-specific glue (TRAP #1 patch
table, T-bit FSM, exception save/restore in asm) is written from scratch
based on the public m68k programmer's reference and the design table above.
Crossbridge is reference-only.

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

mds-stub-68k/        — separate C library, target m68k-elf-gcc (NOT Rust)
  Makefile         — m68k-elf-gcc -m68000 -Os -nostdlib -ffreestanding
  src/
    stub.c         — RSP dispatcher (cleanroom on mborgerson/gdbstub MIT)
    rsp.c          — RSP packet enc/dec (wire-compatible w/ mds-mcp/rsp.rs)
    usb.c          — $A130D0/D2 FIFO read/write
    bp.c           — patch table, atomic write of TRAP #1 word
    vectors.s      — overrides $24, $84, $C4 (m68k-elf-as)
    save_regs.s    — exception-frame save/restore macro
  include/
    mds_stub.h     — public API: mds_stub_init()
  link.ld.frag     — vector overrides for SGDK linker
  README.md
```

**Why C, not Rust:** m68k has no upstream Rust tier-1/2 target. Custom
target JSON + nightly `build-std` works but flaky; `mborgerson/gdbstub`
reference is C anyway → cleanroom port w/o language barrier; `m68k-elf-gcc`
already in marsdev bundle (used by SGDK); smaller binary (no `core` bloat);
hand-rolled buffers cheap when no allocator.

`mds-stub-68k` builds as a static `libmdsstub.a` linked into the user ROM
by SGDK. User adds `LIBS += -lmdsstub` + `LDFLAGS += -T mds_stub_vectors.ld`
to their Makefile + calls `mds_stub_init()` at start of `main()`. The
vector patcher is a build-time linker fragment that rewrites the four
vector longs in the ROM image before the final `hex2bin`. Runtime vector
patching is impossible on 68000 (no VBR); build-time is the only path.

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
| `mega_get_palettes`           |  ✓  | `rsp` `m` against CRAM via stub helper |                                         |
| `mega_get_sprites`            |  ✓  | `rsp` `m` VRAM sprite table            | sprite list pointer from VDP reg 5      |
| `mega_dump_tile`              |  ✓  | `rsp` `m` VRAM at tile-index × 32      |                                         |
| `mega_screenshot`             |  ✓  | `not_supported_on_target`              | hardware can't snapshot framebuffer     |
| `mega_save_state`             |  ✓  | `not_supported_on_target`              | hw has no savestate                     |
| `mega_load_state`             |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_set_state`        |  ✓  | `not_supported_on_target`              | physical pad only on hw                 |
| `mega_input_press`            |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_release`          |  ✓  | `not_supported_on_target`              |                                         |
| `mega_input_get_state`        |  ✓  | `cmd` MEM_RD `$A10003` (`hw-only`)     | reads pad-1 port directly               |

Counts: 14 tools mapped to RSP/cmd, 11 `not_supported_on_target`, 1 always
available. The 11 non-supported tools return a structured
`{ "error": "not_supported_on_target", "target": "edpro" }` response so the
IDE can branch UI without parsing strings.

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
|  9  | 68k stub C library skeleton (m68k-elf-gcc)                 | `mds-stub-68k/Makefile`, `mds-stub-68k/src/{stub,rsp,usb,bp}.c`, `vectors.s` (new) | — | no  |
| 10  | Vector patcher (linker fragment, build-time)               | `mds-stub-68k/link.ld.frag` (new)                          | 9          | no  |
| 11  | KDebug-over-USB shim (replaces SGDK `KDebug_Alert`)        | `mds-stub-68k/src/kdebug.s` (new), SGDK link patch         | 9          | partial (SGDK weak-link Q) |
| 12  | Host `kdebug_monitor` MCP tool                             | `mds-mcp/src/tools/mod.rs` (modify)                        | 4          | no  |
| 13  | E2E smoke test (upload, kprintf, BP hit, single-step)      | `mds-mcp/tests/edpro_e2e.rs` (new)                         | 1–12       | **yes** |
| 14  | Pause-via-host: design IRQ7 trigger from MCU writes        | `mds-stub-68k/src/vectors.s`, host `pause` impl            | 9          | yes (mechanism unknown — §10) |
| 15  | Polled watchpoint (during T-bit step)                      | `mds-stub-68k/src/bp.c`, `stub_sync.rs`                    | 7, 8       | partial (perf tuning) |
| 16  | Refactor `scripts/gdb-proxy.py` into mds-mcp tool surface  | `mds-mcp/src/target/edpro/proxy.rs` (new)                  | 5          | no  |

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
  (task 12).
- `CLAUDE.md` "Notes Mega Everdrive Pro" — high-level orientation; the
  `$A130E2` mention there is being fixed separately.
