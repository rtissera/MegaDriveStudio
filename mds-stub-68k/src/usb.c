// SPDX-License-Identifier: MIT
//
// usb.c — polled FIFO at $A130D0/D2.
//
// We do NOT touch SSF_CTRL ($A130F1) here — the host has already enabled
// SSF + SSF write-protect-clear before uploading the stub blob, so the
// MCU side is talking and the FIFO is ready. Re-enabling from inside the
// stub would race with whatever cart-side state the host just configured.

#include "usb.h"

int mds_usb_rx_ready(void) {
    uint16_t s = *MDS_REG_FIFO_STAT;
    if (s & MDS_FIFO_RXF_MASK) return 1;
    if (s & MDS_FIFO_RX_COUNT_MASK) return 1;
    return 0;
}

uint8_t mds_usb_read_byte(void) {
    while (!mds_usb_rx_ready()) {
        // Spin. No yield — interrupts are disabled inside the exception
        // context. The host is the only thing that drives RX.
    }
    // Low byte of the 16-bit data port carries the payload byte.
    return (uint8_t)(*MDS_REG_FIFO_DATA & 0xFFu);
}

void mds_usb_write_byte(uint8_t b) {
    // The cart->host FIFO drains continuously. No documented overflow bit.
    // If the host is wedged we'd hang here; M5.x will add a software
    // watchdog once we can measure real latency.
    *MDS_REG_FIFO_DATA = (uint16_t)b;
}
