// SPDX-License-Identifier: MIT
//! Best-effort decoders for the clownmdemu memory blobs (`vdp_state`,
//! `m68k_state`) and the in-VRAM sprite attribute table.
//!
//! These are intentionally tolerant: when the blob layout is unknown (the
//! libretro core fork's wire format may evolve), decoders return `None` or
//! whatever default they can rather than panicking. The MCP tools surface
//! the partial information.

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
