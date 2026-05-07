// SPDX-License-Identifier: MIT
//
// usb.h — Mega Everdrive Pro USB FIFO at $A130D0/D2/D4.
// Polled byte-at-a-time I/O. No DMA, no IRQs, no SGDK.

#ifndef MDS_USB_H
#define MDS_USB_H

#include <stdint.h>

// Cart register addresses (see docs/02-m5-architecture.md §3.1).
#define MDS_REG_FIFO_DATA  ((volatile uint16_t *)0xA130D0)
#define MDS_REG_FIFO_STAT  ((volatile uint16_t *)0xA130D2)
#define MDS_REG_SYS_STAT   ((volatile uint16_t *)0xA130D4)
#define MDS_REG_SSF_CTRL   ((volatile uint8_t  *)0xA130F1)

// FIFO_STAT bit semantics, per krikzz/mega-ed-pub edio-mega/everdrive.h:
//   bit 15  FIFO_CPU_RXF — set when the cart MCU has data the 68k can read.
//   bits 0..10  byte count available in the cart->68k FIFO (mask 0x7FF).
#define MDS_FIFO_RXF_MASK      0x8000u
#define MDS_FIFO_RX_COUNT_MASK 0x07FFu

uint8_t mds_usb_read_byte(void);   // blocks until at least one rx byte
void    mds_usb_write_byte(uint8_t b);
int     mds_usb_rx_ready(void);    // non-blocking poll, 1 if rx avail else 0

#endif  // MDS_USB_H
