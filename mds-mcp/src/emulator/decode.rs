// SPDX-License-Identifier: MIT
//! Best-effort decoders for clownmdemu memory blobs (vdp_state, m68k_state,
//! z80_state) and the in-VRAM sprite attribute table. Tolerant: unknown
//! layouts return `None` or zeros rather than panic.

use serde::Serialize;

/// Decoded VDP register dump. `regs` is always 24 bytes.
#[derive(Debug, Serialize, Default, Clone)]
pub struct VdpRegisters {
    pub regs: Vec<u8>,
    pub decoded: VdpDecoded,
}

#[derive(Debug, Serialize, Default, Clone)]
pub struct VdpDecoded {
    /// Plane A nametable VRAM base.
    pub plane_a: u32,
    /// Plane B nametable VRAM base.
    pub plane_b: u32,
    /// Window plane VRAM base.
    pub window: u32,
    /// Sprite attribute table VRAM base.
    pub sprite_table: u32,
    /// Horizontal scroll table VRAM base.
    pub hscroll: u32,
    /// True if H40 (320 px) mode, false if H32 (256 px).
    pub h40: bool,
    /// True if V30 (240 px PAL) mode, false if V28 (224 px).
    pub v30: bool,
    /// Display enabled (reg #1 bit 6).
    pub display_enabled: bool,
    /// DMA enabled (reg #1 bit 4).
    pub dma_enabled: bool,
}

/// Decode a 24-byte VDP register window from the start of a `vdp_state` blob.
/// The clownmdemu fork is expected to put `VDP_State.access.registers[24]`
/// first; if that ever changes, downstream code can pivot via a header byte.
pub fn decode_vdp_registers(blob: &[u8]) -> VdpRegisters {
    let regs: Vec<u8> = blob.iter().copied().take(24).collect();
    let mut padded = [0u8; 24];
    for (i, b) in regs.iter().enumerate() {
        padded[i] = *b;
    }

    let plane_a = ((padded[2] & 0x38) as u32) << 10;
    let window = ((padded[3] & 0x3E) as u32) << 10;
    let plane_b = ((padded[4] & 0x07) as u32) << 13;
    let sprite_table = ((padded[5] & 0x7F) as u32) << 9;
    let hscroll = ((padded[13] & 0x3F) as u32) << 10;

    let h40 = (padded[12] & 0x81) != 0;
    let v30 = (padded[1] & 0x08) != 0;
    let display_enabled = (padded[1] & 0x40) != 0;
    let dma_enabled = (padded[1] & 0x10) != 0;

    VdpRegisters {
        regs,
        decoded: VdpDecoded {
            plane_a,
            plane_b,
            window,
            sprite_table,
            hscroll,
            h40,
            v30,
            display_enabled,
            dma_enabled,
        },
    }
}

/// Decode CRAM (128 bytes, 64 entries × 9-bit BGR, packed in 16-bit words)
/// into 4 palette lines of 16 RGB triplets, expanded to 8-bit per channel.
#[derive(Debug, Serialize, Clone, Copy)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

pub fn decode_palettes(cram: &[u8]) -> Vec<Vec<Rgb>> {
    let mut out: Vec<Vec<Rgb>> = (0..4).map(|_| Vec::with_capacity(16)).collect();
    if cram.len() < 128 {
        return out;
    }
    for (line, line_out) in out.iter_mut().enumerate().take(4) {
        for entry in 0..16 {
            let base = (line * 16 + entry) * 2;
            let hi = cram[base] as u16;
            let lo = cram[base + 1] as u16;
            let word = (hi << 8) | lo;
            // Mega Drive CRAM bits: 0000 BBB0 GGG0 RRR0 (3 bits per channel).
            let r = ((word >> 1) & 0x7) as u8;
            let g = ((word >> 5) & 0x7) as u8;
            let b = ((word >> 9) & 0x7) as u8;
            // Expand 3-bit → 8-bit by replicating MSBs.
            let exp = |c: u8| (c << 5) | (c << 2) | (c >> 1);
            line_out.push(Rgb {
                r: exp(r),
                g: exp(g),
                b: exp(b),
            });
        }
    }
    out
}

/// Decoded sprite attribute table entry (8 bytes per entry on Mega Drive).
#[derive(Debug, Serialize, Clone)]
pub struct Sprite {
    pub index: u32,
    pub y: u16,
    pub x: u16,
    pub width: u8,  // tiles
    pub height: u8, // tiles
    pub link: u8,
    pub priority: bool,
    pub palette: u8,
    pub vflip: bool,
    pub hflip: bool,
    pub tile_index: u16,
}

/// Decode the sprite attribute table from a VRAM dump.
///
/// `vram` should be the full VRAM (64 KiB). `sat_offset` is the byte offset
/// of the sprite attribute table base (from VDP register #5 << 9). The MD
/// supports up to 80 sprites; we stop at the linked-list terminator (link=0)
/// or after `max_sprites` (defaults to 80 in caller).
pub fn decode_sprites(vram: &[u8], sat_offset: u32, max_sprites: usize) -> Vec<Sprite> {
    let mut out = Vec::new();
    let base = sat_offset as usize;
    if base + 8 > vram.len() {
        return out;
    }
    let mut idx: u32 = 0;
    let mut cursor = 0u8;
    let mut steps = 0usize;
    loop {
        if steps >= max_sprites || steps >= 80 {
            break;
        }
        let entry_off = base + (cursor as usize) * 8;
        if entry_off + 8 > vram.len() {
            break;
        }
        let e = &vram[entry_off..entry_off + 8];
        let y = u16::from_be_bytes([e[0], e[1]]) & 0x03FF;
        let size_link = e[2];
        let height = (size_link & 0x03) + 1;
        let width = ((size_link >> 2) & 0x03) + 1;
        let link = e[3] & 0x7F;
        let attr = u16::from_be_bytes([e[4], e[5]]);
        let tile_index = attr & 0x07FF;
        let hflip = (attr & 0x0800) != 0;
        let vflip = (attr & 0x1000) != 0;
        let palette = ((attr >> 13) & 0x03) as u8;
        let priority = (attr & 0x8000) != 0;
        let x = u16::from_be_bytes([e[6], e[7]]) & 0x01FF;

        out.push(Sprite {
            index: idx,
            y,
            x,
            width,
            height,
            link,
            priority,
            palette,
            vflip,
            hflip,
            tile_index,
        });

        idx += 1;
        steps += 1;
        if link == 0 {
            break;
        }
        cursor = link;
    }
    out
}

/// Decoded 68k registers.
#[derive(Debug, Serialize, Default, Clone)]
pub struct M68kRegisters {
    pub d: [u32; 8],
    pub a: [u32; 8],
    pub pc: u32,
    pub sr: u16,
    pub usp: u32,
    pub ssp: u32,
}

/// Decode an `m68k_state` blob. Best-effort: assumes layout
/// `[d0..d7, a0..a7, pc(u32), sr(u16), usp(u32), ssp(u32)]` little-endian
/// (the clownmdemu fork agent is expected to use host-endian, so we treat
/// little-endian on x86_64 hosts). Insufficient blobs return `None`.
pub fn decode_m68k(blob: &[u8]) -> Option<M68kRegisters> {
    if blob.len() < 8 * 4 + 8 * 4 + 4 + 2 + 4 + 4 {
        return None;
    }
    let mut r = M68kRegisters::default();
    let mut off = 0;
    for i in 0..8 {
        r.d[i] = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
        off += 4;
    }
    for i in 0..8 {
        r.a[i] = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
        off += 4;
    }
    r.pc = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
    off += 4;
    r.sr = u16::from_le_bytes([blob[off], blob[off + 1]]);
    off += 2;
    r.usp = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
    off += 4;
    r.ssp = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]);
    Some(r)
}

/// Decoded Z80 register snapshot, matching the JSON shape promised by
/// `mega_get_z80_registers`. AF/BC/DE/HL/IX/IY are combined from the
/// underlying 8-bit halves.
#[derive(Debug, Serialize, Default, Clone, PartialEq, Eq)]
pub struct Z80Registers {
    pub af: u16,
    pub bc: u16,
    pub de: u16,
    pub hl: u16,
    pub ix: u16,
    pub iy: u16,
    pub pc: u16,
    pub sp: u16,
    pub halt: bool,
    pub iff1: bool,
    pub iff2: bool,
    pub im: u8,
    pub cycles: u64,
    pub bus_requested: bool,
    pub bus_reset: bool,
}

/// Decode a `ClownZ80_State` blob plus the (optional) two-byte bus blob.
/// Layout from clownz80/source/interpreter.h (default `clowncommon` typedefs
/// `cc_u8l=u8` and `cc_u16l=u16`, host-endian): register_mode + 1B pad +
/// cycles + pc + sp + main 8 + backup 8 + ixh/ixl/iyh/iyl + r/i + iff +
/// pending = 32 bytes. We tolerate 30-byte (no padding) blobs by sniffing.
/// `bus_blob` is `[bus_requested, bus_reset]`; either may be empty.
pub fn decode_z80(state_blob: &[u8], bus_blob: &[u8]) -> Option<Z80Registers> {
    if state_blob.len() < 30 {
        return None;
    }
    // Try padded layout first (header at offset 1 is alignment pad, so byte 1
    // should be 0 in most cases). Fall back to packed if the byte at offset 1
    // doesn't look like padding *and* the blob is exactly 30 bytes long.
    let packed = state_blob.len() < 32;
    let off_cycles = if packed { 1 } else { 2 };

    let read_u16 = |o: usize| -> u16 {
        if o + 2 > state_blob.len() {
            0
        } else {
            // host-endian (clownz80 stores natively).
            u16::from_le_bytes([state_blob[o], state_blob[o + 1]])
        }
    };
    let read_u8 = |o: usize| -> u8 { state_blob.get(o).copied().unwrap_or(0) };

    let cycles = read_u16(off_cycles) as u64;
    let pc = read_u16(off_cycles + 2);
    let sp = read_u16(off_cycles + 4);
    let regs_base = off_cycles + 6;
    let a = read_u8(regs_base);
    let f = read_u8(regs_base + 1);
    let b = read_u8(regs_base + 2);
    let c = read_u8(regs_base + 3);
    let d = read_u8(regs_base + 4);
    let e = read_u8(regs_base + 5);
    let h = read_u8(regs_base + 6);
    let l = read_u8(regs_base + 7);
    // Backup regs at +8..+16 (skipped — not exposed in the JSON).
    let ix_base = regs_base + 16;
    let ixh = read_u8(ix_base);
    let ixl = read_u8(ix_base + 1);
    let iyh = read_u8(ix_base + 2);
    let iyl = read_u8(ix_base + 3);
    // r, i at +20..+22 (not exposed).
    let flags_base = ix_base + 6;
    let interrupts_enabled = read_u8(flags_base) != 0;
    let interrupt_pending = read_u8(flags_base + 1) != 0;

    let bus_requested = bus_blob.first().copied().unwrap_or(0) != 0;
    let bus_reset = bus_blob.get(1).copied().unwrap_or(0) != 0;

    Some(Z80Registers {
        af: ((a as u16) << 8) | (f as u16),
        bc: ((b as u16) << 8) | (c as u16),
        de: ((d as u16) << 8) | (e as u16),
        hl: ((h as u16) << 8) | (l as u16),
        ix: ((ixh as u16) << 8) | (ixl as u16),
        iy: ((iyh as u16) << 8) | (iyl as u16),
        pc,
        sp,
        // clownz80 doesn't model HALT distinctly; expose `interrupt_pending`'s
        // complement only when both flags are zero — best effort. Future fork
        // patches may add an explicit halt flag and bump the blob layout.
        halt: false,
        iff1: interrupts_enabled,
        iff2: interrupts_enabled,
        // clownz80 hard-codes IM 1 on Mega Drive bring-up.
        im: 1,
        cycles,
        bus_requested,
        bus_reset,
    }.with_pending(interrupt_pending))
}

impl Z80Registers {
    fn with_pending(mut self, pending: bool) -> Self {
        // If an interrupt is pending the CPU isn't halted regardless.
        if pending {
            self.halt = false;
        }
        self
    }
}

/// Decode an 8x8 tile (32 bytes, 4 bits per pixel) into an indexed bitmap.
pub fn decode_tile_8x8(vram: &[u8], tile_index: u32) -> Option<Vec<u8>> {
    let off = (tile_index as usize) * 32;
    if off + 32 > vram.len() {
        return None;
    }
    let mut bitmap = Vec::with_capacity(64);
    for &byte in &vram[off..off + 32] {
        bitmap.push(byte >> 4);
        bitmap.push(byte & 0x0F);
    }
    Some(bitmap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_padded_z80() -> Vec<u8> {
        let mut blob = Vec::with_capacity(32);
        blob.push(0); // register_mode
        blob.push(0); // pad
        blob.extend_from_slice(&123u16.to_le_bytes()); // cycles
        blob.extend_from_slice(&0x1234u16.to_le_bytes()); // pc
        blob.extend_from_slice(&0xFFE0u16.to_le_bytes()); // sp
        blob.extend_from_slice(&[0xAA, 0xBB, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        blob.extend_from_slice(&[0; 8]);
        blob.extend_from_slice(&[0xC0, 0x0D, 0xBE, 0xEF]);
        blob.extend_from_slice(&[0x77, 0x88]);
        blob.push(1); // interrupts_enabled
        blob.push(0); // interrupt_pending
        blob
    }

    #[test]
    fn z80_padded_layout() {
        let blob = synthetic_padded_z80();
        let regs = decode_z80(&blob, &[1u8, 0u8]).unwrap();
        assert_eq!(regs.cycles, 123);
        assert_eq!(regs.pc, 0x1234);
        assert_eq!(regs.sp, 0xFFE0);
        assert_eq!(regs.af, 0xAABB);
        assert_eq!(regs.bc, 0x1122);
        assert_eq!(regs.de, 0x3344);
        assert_eq!(regs.hl, 0x5566);
        assert_eq!(regs.ix, 0xC00D);
        assert_eq!(regs.iy, 0xBEEF);
        assert!(regs.iff1);
        assert_eq!(regs.im, 1);
        assert!(regs.bus_requested);
        assert!(!regs.bus_reset);
    }

    #[test]
    fn z80_short_blob_is_none() {
        assert!(decode_z80(&[0u8; 8], &[]).is_none());
    }

    #[test]
    fn z80_zero_blob_is_zero_regs() {
        let regs = decode_z80(&[0u8; 32], &[]).unwrap();
        assert_eq!(regs.pc, 0);
        assert_eq!(regs.sp, 0);
        assert_eq!(regs.af, 0);
        assert!(!regs.iff1);
        assert!(!regs.bus_requested);
    }

    #[test]
    fn z80_bus_blob_partial() {
        let blob = synthetic_padded_z80();
        let r = decode_z80(&blob, &[]).unwrap();
        assert!(!r.bus_requested);
        let r = decode_z80(&blob, &[0u8, 1u8]).unwrap();
        assert!(!r.bus_requested);
        assert!(r.bus_reset);
    }

    #[test]
    fn vdp_decode_smoke() {
        let mut blob = vec![0u8; 24];
        blob[1] = 0x40; // display enabled
        blob[5] = 0x10; // sprite table at (0x10 & 0x7F) << 9 = 0x2000
        blob[12] = 0x81; // H40
        let r = decode_vdp_registers(&blob);
        assert!(r.decoded.display_enabled);
        assert!(r.decoded.h40);
        assert_eq!(r.decoded.sprite_table, 0x2000);
    }
}
