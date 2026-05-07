// SPDX-License-Identifier: MIT
//
// bp.h — software breakpoint table for the on-cart 68k stub.
//
// The host side (`mds-mcp/src/target/edpro/stub_sync.rs`) already maintains
// its own BP table and patches PSRAM via MEM_WR / RSP `M`. This local copy
// exists so that a TRAP #1 hit can quickly look up "is the next instruction
// I need to single-step over a breakpoint we own, or just user code?" and
// because the stub is the one that physically rolls PC back to the trap
// site after restoring the original opcode.
//
// Hardware note: 68000 has no I-cache, so a write through the bus is
// immediately visible to the prefetch unit on the next fetch cycle.

#ifndef MDS_BP_H
#define MDS_BP_H

#include <stdint.h>

#define MDS_BP_MAX 32

#define MDS_TRAP1_OPCODE 0x4E41u  // m68k `TRAP #1`

int  mds_bp_set(uint32_t addr);
int  mds_bp_clear(uint32_t addr);
int  mds_bp_lookup(uint32_t addr, uint16_t *orig_out);

// After TRAP #1, the saved PC points just past the trap word (TRAP #1 is
// a 1-word instruction, so the bp address is `pc - 2`). Restores the
// original opcode there. Returns 0 if a BP was found+restored.
int  mds_bp_restore_at(uint32_t pc);

// Re-arm a BP after the host single-stepped past it.
int  mds_bp_rearm(uint32_t addr);

#endif  // MDS_BP_H
