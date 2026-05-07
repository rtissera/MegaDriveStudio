// SPDX-License-Identifier: MIT
//
// usb.h — Mega Everdrive Pro USB FIFO at $A130D0/D2/D4.
// Polled byte-at-a-time I/O. No DMA, no IRQs.

#ifndef MDS_USB_H
#define MDS_USB_H

#include <stdint.h>

// Cart register addresses (see docs/02-m5-architecture.md §3.1).
#define MDS_REG_FIFO_DATA  ((volatile uint16_t *)0xA130D0)
#define MDS_REG_FIFO_STAT  ((volatile uint16_t *)0xA130D2)
#define MDS_REG_SYS_STAT   ((volatile uint16_t *)0xA130D4)
#define MDS_REG_SSF_CTRL   ((volatile uint8_t  *)0xA130F1)

// FIFO_STAT bit semantics, per krikzz/mega-ed-pub/edio-mega/everdrive.h
// + krikzz extended-SSF.txt:
//   bit 15  FIFO_CPU_RXF   — set when the cart MCU has data the 68k can read
//                            (cart->68k direction).
//   bits 0..10  byte count available in cart->68k FIFO (mask 0x7FF).
// TX-side readiness is implicit: the MCU's host-bound FIFO is large (>=128
// bytes per docs §3.1) and a stalled write means the host serial is jammed,
// which is unrecoverable without intervention. We use a generous spin-wait
// budget that the host-side gdb-proxy treats as a session timeout.
//
// TODO M5.x: confirm TX-ready bit position against krikzz/mega-ed-pub once
// hardware tests run. Current implementation conservatively writes
// unconditionally — the FIFO at $A130D0 absorbs writes and the MCU drains
// asynchronously.
#define MDS_FIFO_RXF_BIT       15
#define MDS_FIFO_RXF_MASK      0x8000
#define MDS_FIFO_RX_COUNT_MASK 0x07FF

void    mds_usb_init(void);
uint8_t mds_usb_read_byte(void);   // blocks until at least one rx byte
void    mds_usb_write_byte(uint8_t b);
int     mds_usb_rx_ready(void);    // non-blocking poll, 1 if rx avail else 0

#endif  // MDS_USB_H
