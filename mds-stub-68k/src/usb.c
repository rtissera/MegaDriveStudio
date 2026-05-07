// SPDX-License-Identifier: MIT
//
// usb.c — polled FIFO at $A130D0/D2.
// Cleanroom; no copied code from gdbstub or crossbridge.

#include "usb.h"

void mds_usb_init(void) {
    // Enable extended-SSF mapper so the 68k can talk to the cart MCU at all.
    // Writing 0x2A to $A130F1 once at boot is the documented unlock sequence
    // (krikzz/mega-ed-pub edio-mega/everdrive.c::ed_io_init). On Pro it may
    // already be enabled by the boot menu; the write is idempotent.
    *MDS_REG_SSF_CTRL = 0x2A;
}

int mds_usb_rx_ready(void) {
    uint16_t s = *MDS_REG_FIFO_STAT;
    // Either FIFO_CPU_RXF flag set OR the byte-count field is non-zero.
    if (s & MDS_FIFO_RXF_MASK) return 1;
    if (s & MDS_FIFO_RX_COUNT_MASK) return 1;
    return 0;
}

uint8_t mds_usb_read_byte(void) {
    while (!mds_usb_rx_ready()) {
        // Spin. No yield — interrupts are off in the stub context.
    }
    // Low byte of the 16-bit data port carries the payload byte.
    return (uint8_t)(*MDS_REG_FIFO_DATA & 0xFF);
}

void mds_usb_write_byte(uint8_t b) {
    // The 128-byte cart->host FIFO drains continuously; the write is
    // effectively non-blocking under normal conditions. If the host is wedged
    // we do still want to *not* livelock — but there is no hardware overflow
    // bit documented, so we simply write. M5.x will add a software watchdog
    // counter once we can measure real latency.
    *MDS_REG_FIFO_DATA = (uint16_t)b;
}
