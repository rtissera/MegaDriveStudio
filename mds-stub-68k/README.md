# mds-stub-68k

On-cart Mega Drive 68000 debug stub for the Mega Everdrive Pro target.
The host (Megadrive Studio MCP server) talks GDB Remote Serial Protocol
into the cart USB FIFO at `$A130D0`; this library is the 68k side.

License: MIT (cleanroom on
[mborgerson/gdbstub](https://github.com/mborgerson/gdbstub)). **Do not**
link `crossbridge gdb-7.3/gdb/m68k-stub.c` (GPLv2) against your ROM.

## Build

```
make MARSDEV=$HOME/mars
```

Produces `libmdsstub.a`.

## Integration with SGDK

In your SGDK project's `Makefile`, after including `$(GDK)/makefile.gen`:

```make
LIBS    += -L$(MDS_STUB_DIR) -lmdsstub
LDFLAGS += -T$(MDS_STUB_DIR)/link.ld.frag
```

In `main()`:

```c
#include <genesis.h>
#include "mds_stub.h"

int main(void) {
    mds_stub_init();        // first thing — installs Trace/TRAP #1 vectors
    // ... rest of your game ...
    return 0;
}
```

The stub is designed to install itself by patching the RAM-shadowed vector
table at `$FF0000` (SGDK 2.x relocates the vector base there in
`boot/sega.s`). For bare-metal / non-SGDK projects, use the link-time
approach in `link.ld.frag`.

## Footprint

- `~3-4 KB` text (Os, m68000)
- 32-entry breakpoint table = 256 bytes
- 18 longs of saved registers = 72 bytes
- 2× 1024-byte RSP buffers = 2048 bytes
- Total RAM = ~2.5 KB; fits easily in the upper end of work RAM.

## Performance impact

- When not stopped at a breakpoint: zero. The stub only runs in the Trace
  / TRAP #1 vectors, which are unreachable until the host arms one.
- When stopped: blocks on USB. Host-side gdb-proxy is the bottleneck.
- During T-bit single-step: ~10× slowdown (every instruction takes a
  full Trace exception round-trip).

## Public API

```c
void mds_stub_init(void);                 // call once at boot
static inline void mds_stub_break(void);  // inline `trap #1` — manual halt
```

## Wire protocol

Speaks GDB Remote Serial Protocol verbatim into `$A130D0`. The host
companion code in `mds-mcp/src/target/edpro/rsp.rs` is the matching
encoder/decoder; see `docs/02-m5-architecture.md` §5 for the full design.

Supported packets: `g G m M c s ? Z0 z0 qSupported QStartNoAckMode qC
qAttached k D`. PacketSize advertised: 400 bytes.

## TODO M5.5+

- USB_WR envelope wrapper around outbound RSP frames if hardware needs it
  (open question §10.Q1 in the architecture doc).
- Watchpoints — polled during T-bit step, host-driven.
- Pause from host — needs IRQ injection mechanism (§10.Q2).
- KDebug-over-USB shim (separate task M5.4b).
