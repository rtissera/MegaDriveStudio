// SPDX-License-Identifier: MIT
//
// bp.c — fixed-size breakpoint table. No malloc.

#include "bp.h"

typedef struct {
    uint32_t addr;
    uint16_t orig;
    uint8_t  in_use;
    uint8_t  pad;       // alignment + future flags (e.g. one-shot)
} bp_entry_t;

static bp_entry_t g_bps[MDS_BP_MAX];

static int find_slot(uint32_t addr) {
    for (int i = 0; i < MDS_BP_MAX; ++i) {
        if (g_bps[i].in_use && g_bps[i].addr == addr) return i;
    }
    return -1;
}

static int find_free(void) {
    for (int i = 0; i < MDS_BP_MAX; ++i) {
        if (!g_bps[i].in_use) return i;
    }
    return -1;
}

int mds_bp_set(uint32_t addr) {
    if (find_slot(addr) >= 0) return -1;
    int slot = find_free();
    if (slot < 0) return -1;
    volatile uint16_t *p = (volatile uint16_t *)(uintptr_t)addr;
    g_bps[slot].addr   = addr;
    g_bps[slot].orig   = *p;
    g_bps[slot].in_use = 1;
    *p = MDS_TRAP1_OPCODE;
    return 0;
}

int mds_bp_clear(uint32_t addr) {
    int slot = find_slot(addr);
    if (slot < 0) return -1;
    volatile uint16_t *p = (volatile uint16_t *)(uintptr_t)addr;
    *p = g_bps[slot].orig;
    g_bps[slot].in_use = 0;
    return 0;
}

int mds_bp_lookup(uint32_t addr, uint16_t *orig_out) {
    int slot = find_slot(addr);
    if (slot < 0) return -1;
    if (orig_out) *orig_out = g_bps[slot].orig;
    return 0;
}

int mds_bp_restore_at(uint32_t pc) {
    uint32_t addr = pc - 2u;
    int slot = find_slot(addr);
    if (slot < 0) return -1;
    volatile uint16_t *p = (volatile uint16_t *)(uintptr_t)addr;
    *p = g_bps[slot].orig;
    return 0;
}

int mds_bp_rearm(uint32_t addr) {
    int slot = find_slot(addr);
    if (slot < 0) return -1;
    volatile uint16_t *p = (volatile uint16_t *)(uintptr_t)addr;
    *p = MDS_TRAP1_OPCODE;
    return 0;
}
