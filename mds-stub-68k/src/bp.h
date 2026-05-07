// SPDX-License-Identifier: MIT
//
// bp.h — software breakpoint table for the on-cart 68k stub.
// Patches the target opcode with TRAP #1 (0x4E41) and remembers the
// original word so we can restore it on `z0` (clear).
//
// Hardware note: 68000 has no I-cache, so a write through the bus is
// immediately visible to the prefetch unit on the next fetch cycle. No
// cache-flush needed. PSRAM under the EdPro lets us write "ROM" addresses
// at runtime; the host pre-flips the cart's SSF write-protect at session
// start (see docs/02-m5-architecture.md §10 Q3).

#ifndef MDS_BP_H
#define MDS_BP_H

#include <stdint.h>

#define MDS_BP_MAX 32

#define MDS_TRAP1_OPCODE 0x4E41u  // m68k `TRAP #1`

int  mds_bp_set(uint32_t addr);
int  mds_bp_clear(uint32_t addr);
int  mds_bp_lookup(uint32_t addr, uint16_t *orig_out);

// Walk callback used by the stub when entering a TRAP #1 frame: restore
// the original opcode at `pc - 2` (TRAP #1 is a 1-word instruction so the
// saved PC points just past it). Returns 0 if a BP was found+restored.
int  mds_bp_restore_at(uint32_t pc);

// Re-arm a BP after the host single-stepped past it (called from the
// continue-after-bp path in stub.c).
int  mds_bp_rearm(uint32_t addr);

#endif  // MDS_BP_H
