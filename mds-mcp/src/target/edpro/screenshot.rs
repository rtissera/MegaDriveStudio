// SPDX-License-Identifier: MIT
//! M5.8b — host-side VDP frame decoder for the EdPro hardware target.
//!
//! VDP regs `$00..$17` are write-only, but SGDK keeps a software shadow
//! (`regValues[19]`) plus a handful of addresses (`bga_addr`, `bgb_addr`,
//! `slist_addr`, `window_addr`, `hscroll`) in MD work RAM. M5.8a parses
//! those addresses out of the debug ELF; this module reads them — plus
//! CRAM, VSRAM and the full 64 KiB of VRAM — through the existing
//! `StubSync` wire commands and synthesises a 320×224 (or 256×224) RGB8
//! frame, encoded as PNG.
//!
//! No new RSP commands; no stub change. The whole pipeline is `read_memory`
//! + `read_cram` + `read_vsram` + `read_vram` (chunked at 128 B).
//!
//! ## v1 compromises (documented per the M5.8b task spec)
//!
//! - **Window plane skipped.** Only Plane A + Plane B + sprites + backdrop
//!   are composited. Window typically draws status bars / score text;
//!   acceptable to omit for a debug screenshot.
//! - **Shadow / highlight effect skipped.** Reg 12 bit 3 ignored.
//! - **Interlaced mode skipped.** Reg 12 bits 1-2 ignored; we always render
//!   non-interlaced.
//! - **Per-cell HScroll falls back to per-line.** Reg 11 bits 0-1 = 2 is
//!   treated as 3 (reads the same table at finer granularity).
//! - **No 80-sprite-per-frame limit.** We follow the link list to its end
//!   without enforcing the hardware cap.
//! - **No sprite-per-line clipping** (20 H32 / 16 H40). Accepted overdraw.
//! - **Always 224 lines NTSC.** PAL 240 not handled.
//!
//! ## VDP reference
//!
//! - Resolution: reg 12 bit 0 (RS0) → H40 320 px / H32 256 px. Always 224
//!   NTSC.
//! - Display enable: reg 1 bit 6. If 0, render solid-black PNG.
//! - Backdrop: reg 7 bits 0-5 = `(palette<<4)|entry`.
//! - Plane size (reg 16): bits 0-1 HSIZE, bits 4-5 VSIZE. {0,1,3} ⇒
//!   {32,64,128} cells.
//! - Plane base addrs: read from work RAM (each is a u16 BE VRAM offset).
//! - Nametable cell (BE u16): bit 15 priority, bits 13-14 palette,
//!   bit 12 vflip, bit 11 hflip, bits 0-10 tile index. Tile bytes =
//!   `tile_index * 32`.
//! - Tile (4bpp 8×8 = 32 B): each byte = 2 px (high nibble first).
//!   Color 0 = transparent.
//! - CRAM word (BE): `0000 BBB0 GGG0 RRR0`. Component LUT
//!   `[0,52,87,116,144,172,206,255]` (the canonical 8-step ramp).
//! - HScroll mode (reg 11 bits 0-1): 0 = whole-screen, 2 = per-cell,
//!   3 = per-line. Table at `hscroll`. Whole-screen = 4 B; per-line =
//!   224×4 B = 896 B. We treat 2 as 3.
//! - VScroll mode (reg 11 bit 2): 0 = full, 1 = per-2-cell. VSRAM 80 B =
//!   40 cells × u16 BE.
//! - Sprite list at `slist_addr` (max 80 × 8 B). Walk via link byte
//!   until 0 / safety limit. Sprite priority bit 7 of the attr word's
//!   high byte; planes/sprites composited per priority order.
//!
//! Pixel order per line (back to front):
//! `backdrop → low B → low A → low S → high B → high A → high S`.
//! First non-transparent at correct priority wins. (Sprites strictly
//! above same-priority planes.)

use anyhow::{bail, Context, Result};
use tracing::info;

use super::elf_parse::SgdkSymbols;
use super::stub_sync::{StubSync, VRAM_CHUNK_MAX};
use super::usb::UsbTransport;

/// Canonical 3-bit-component → 8-bit ramp used by every Mega Drive
/// VDP-decode reference (gens, blastem, picodrive). Linear-ish 0..255.
const COMPONENT_LUT: [u8; 8] = [0, 52, 87, 116, 144, 172, 206, 255];

/// Fixed render height (NTSC). PAL 240 deliberately not handled in v1.
const FRAME_H: usize = 224;

/// Decoded representation of one nametable cell entry (16-bit BE word).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NametableCell {
    /// Bit 15 — high-priority pixel.
    pub priority: bool,
    /// Bits 13-14 — palette index 0..3.
    pub palette: u8,
    /// Bit 12 — vertical flip.
    pub vflip: bool,
    /// Bit 11 — horizontal flip.
    pub hflip: bool,
    /// Bits 0-10 — tile index. Multiply by 32 for VRAM byte offset.
    pub tile_index: u16,
}

/// Decoded representation of one 8-byte sprite entry from the SAT.
///
/// Layout (per Plutiedev / Sega VDP docs):
/// - Byte 0..1 (BE): Y position. Low 10 bits used; 0x80 = top of screen.
/// - Byte 2: bits 0-1 = HSIZE (in cells, 1..4), bits 2-3 = VSIZE.
/// - Byte 3: link byte (0 → end of list).
/// - Byte 4..5 (BE): attr word — bit 15 priority, bits 13-14 palette,
///   bit 12 vflip, bit 11 hflip, bits 0-10 tile index.
/// - Byte 6..7 (BE): X position. Low 9 bits used; 0x80 = left of screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteEntry {
    /// Raw Y from the SAT — subtract 0x80 for screen-space.
    pub y_raw: u16,
    /// Width in cells (1..=4).
    pub w_cells: u8,
    /// Height in cells (1..=4).
    pub h_cells: u8,
    /// Index of the next sprite in the linked list. 0 ⇒ end.
    pub link: u8,
    /// Bit 15 of the attr word.
    pub priority: bool,
    /// Bits 13-14.
    pub palette: u8,
    /// Bit 12.
    pub vflip: bool,
    /// Bit 11.
    pub hflip: bool,
    /// Bits 0-10.
    pub tile_index: u16,
    /// Raw X — subtract 0x80 for screen-space.
    pub x_raw: u16,
}

// ---- pure decoders -------------------------------------------------------

/// Decode 64 CRAM colours (128 B little-endian-on-wire... actually big-endian
/// MD-side, the read_cram path delivers MD bytes verbatim). Result is
/// `[[R,G,B]; 64]` in sRGB8.
pub fn decode_cram(cram: &[u8; 128]) -> [[u8; 3]; 64] {
    let mut out = [[0u8; 3]; 64];
    for i in 0..64 {
        let word = u16::from_be_bytes([cram[i * 2], cram[i * 2 + 1]]);
        // 0000 BBB0 GGG0 RRR0
        let r = ((word >> 1) & 0x7) as usize;
        let g = ((word >> 5) & 0x7) as usize;
        let b = ((word >> 9) & 0x7) as usize;
        out[i] = [COMPONENT_LUT[r], COMPONENT_LUT[g], COMPONENT_LUT[b]];
    }
    out
}

/// Decode a single nametable cell from its 16-bit big-endian word.
pub fn decode_nametable_entry(word: u16) -> NametableCell {
    NametableCell {
        priority: (word & 0x8000) != 0,
        palette: ((word >> 13) & 0x3) as u8,
        vflip: (word & 0x1000) != 0,
        hflip: (word & 0x0800) != 0,
        tile_index: word & 0x07FF,
    }
}

/// Decode an 8-byte SAT sprite entry.
pub fn decode_sprite_entry(b: &[u8; 8]) -> SpriteEntry {
    let y = u16::from_be_bytes([b[0], b[1]]) & 0x03FF;
    let size = b[2];
    let link = b[3] & 0x7F;
    let attr = u16::from_be_bytes([b[4], b[5]]);
    let x = u16::from_be_bytes([b[6], b[7]]) & 0x01FF;
    SpriteEntry {
        y_raw: y,
        w_cells: ((size >> 2) & 0x3) + 1,
        h_cells: (size & 0x3) + 1,
        link,
        priority: (attr & 0x8000) != 0,
        palette: ((attr >> 13) & 0x3) as u8,
        vflip: (attr & 0x1000) != 0,
        hflip: (attr & 0x0800) != 0,
        tile_index: attr & 0x07FF,
        x_raw: x,
    }
}

// ---- VDP state ----------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct VdpState {
    display_enabled: bool,
    h40: bool,
    plane_w_cells: u32, // 32 / 64 / 128
    plane_h_cells: u32,
    backdrop_idx: u8, // 0..63
    /// HScroll mode: 0=whole, 1=invalid (treat as whole), 2=per-cell, 3=per-line.
    hscroll_mode: u8,
    /// VScroll mode: false=full, true=per-2-cell.
    vscroll_per_cell: bool,
}

fn decode_plane_dim(code: u8) -> u32 {
    match code & 0x3 {
        0 => 32,
        1 => 64,
        3 => 128,
        _ => 32, // 2 is invalid; spec says behaviour is undefined.
    }
}

fn parse_vdp_state(reg: &[u8; 19]) -> VdpState {
    let r1 = reg[1];
    let r7 = reg[7];
    let r11 = reg[11];
    let r12 = reg[12];
    let r16 = reg[16];
    VdpState {
        display_enabled: (r1 & 0x40) != 0,
        h40: (r12 & 0x01) != 0,
        plane_w_cells: decode_plane_dim(r16 & 0x3),
        plane_h_cells: decode_plane_dim((r16 >> 4) & 0x3),
        backdrop_idx: r7 & 0x3F,
        hscroll_mode: r11 & 0x3,
        vscroll_per_cell: (r11 & 0x4) != 0,
    }
}

// ---- VRAM accessors -----------------------------------------------------

/// Read a 4bpp tile (32 B) from VRAM, apply h/v flip, decode each pixel as
/// a 4-bit palette-relative index. Returns 64 indices in row-major order.
fn decode_tile(vram: &[u8; 0x10000], tile_index: u16, hflip: bool, vflip: bool) -> [u8; 64] {
    let base = (tile_index as usize) * 32;
    let mut px = [0u8; 64];
    for row in 0..8usize {
        let src_row = if vflip { 7 - row } else { row };
        let row_off = base + src_row * 4;
        for col in 0..8usize {
            let src_col = if hflip { 7 - col } else { col };
            let byte = vram[row_off + (src_col >> 1)];
            // pixels 0/2/4/6 = high nibble; 1/3/5/7 = low nibble.
            let nib = if (src_col & 1) == 0 {
                (byte >> 4) & 0x0F
            } else {
                byte & 0x0F
            };
            px[row * 8 + col] = nib;
        }
    }
    px
}

fn nametable_word_at(
    vram: &[u8; 0x10000],
    base: u32,
    plane_w: u32,
    plane_h: u32,
    cx: u32,
    cy: u32,
) -> u16 {
    let cx = cx % plane_w;
    let cy = cy % plane_h;
    let off = (base as usize) + ((cy * plane_w + cx) as usize) * 2;
    let off = off & 0xFFFF;
    u16::from_be_bytes([vram[off], vram[off.wrapping_add(1) & 0xFFFF]])
}

// ---- compositor --------------------------------------------------------

/// Per-pixel result of the plane / sprite layers. `None` ⇒ transparent.
/// Stores the absolute 0..63 CRAM index already resolved.
type LayerLine = Vec<Option<u8>>;

#[allow(clippy::too_many_arguments)]
fn render_plane_line(
    vram: &[u8; 0x10000],
    base: u32,
    state: &VdpState,
    width: usize,
    line: usize,
    hscroll: i32,
    vscroll: i32,
    want_priority: bool,
) -> LayerLine {
    let mut out = vec![None; width];
    let plane_w = state.plane_w_cells;
    let plane_h_px = (state.plane_h_cells * 8) as i32;
    // World y: line + vscroll, mod plane height.
    let wy = ((line as i32 + vscroll).rem_euclid(plane_h_px)) as u32;
    let cy = wy / 8;
    let row_in_tile = (wy % 8) as usize;

    for (x, slot) in out.iter_mut().enumerate().take(width) {
        let wx = ((x as i32 - hscroll).rem_euclid(plane_w as i32 * 8)) as u32;
        let cx = wx / 8;
        let col_in_tile = (wx % 8) as usize;
        let word = nametable_word_at(vram, base, plane_w, state.plane_h_cells, cx, cy);
        let cell = decode_nametable_entry(word);
        if cell.priority != want_priority {
            continue;
        }
        // Decode just the one pixel (cheaper than a full tile decode here).
        let src_row = if cell.vflip { 7 - row_in_tile } else { row_in_tile };
        let src_col = if cell.hflip { 7 - col_in_tile } else { col_in_tile };
        let tile_base = (cell.tile_index as usize) * 32;
        let byte = vram[(tile_base + src_row * 4 + (src_col >> 1)) & 0xFFFF];
        let nib = if (src_col & 1) == 0 {
            (byte >> 4) & 0x0F
        } else {
            byte & 0x0F
        };
        if nib != 0 {
            *slot = Some(cell.palette * 16 + nib);
        }
    }
    out
}

fn read_hscroll(
    vram: &[u8; 0x10000],
    table_base: u32,
    state: &VdpState,
    line: usize,
) -> (i32, i32) {
    // Each entry: 4 bytes (A then B, each BE u16). 10-bit signed-ish; we
    // sign-extend the low 10 bits.
    let entry_off = match state.hscroll_mode {
        0 | 1 => 0,                // whole-screen
        _ => (line as u32) * 4,    // per-line (and per-cell falls back to per-line)
    };
    let off = ((table_base + entry_off) & 0xFFFF) as usize;
    let a = u16::from_be_bytes([vram[off], vram[(off + 1) & 0xFFFF]]) & 0x03FF;
    let b = u16::from_be_bytes([vram[(off + 2) & 0xFFFF], vram[(off + 3) & 0xFFFF]]) & 0x03FF;
    // VDP scroll values are subtracted from the screen X to get world X.
    // "+ shifts plane right". Sign-extend 10-bit.
    fn sx(v: u16) -> i32 {
        let v = v as i32;
        if v & 0x200 != 0 { v - 0x400 } else { v }
    }
    (sx(a), sx(b))
}

fn read_vscroll(vsram: &[u8; 80], col: u32, state: &VdpState) -> (i32, i32) {
    // VSRAM is 40 entries × u16 BE; entry i has plane A at i*2*2, B at i*2*2+2.
    // Wait — VSRAM word layout per docs: word 0 = A col 0, word 1 = B col 0,
    // word 2 = A col 1, word 3 = B col 1, etc. So: A at (col*2)*2, B at (col*2+1)*2.
    let a_off = if state.vscroll_per_cell { (col / 2 * 4) as usize } else { 0 };
    let b_off = if state.vscroll_per_cell { (col / 2 * 4 + 2) as usize } else { 2 };
    let a_off = a_off.min(78);
    let b_off = b_off.min(78);
    let a = u16::from_be_bytes([vsram[a_off], vsram[a_off + 1]]) & 0x07FF;
    let b = u16::from_be_bytes([vsram[b_off], vsram[b_off + 1]]) & 0x07FF;
    (a as i32, b as i32)
}

fn render_sprites_line(
    vram: &[u8; 0x10000],
    sat_base: u32,
    width: usize,
    line: usize,
    want_priority: bool,
) -> LayerLine {
    let mut out: LayerLine = vec![None; width];
    // Walk linked list from sprite 0. Hard cap at 80 entries to avoid
    // pathological cycles in malformed SATs.
    let mut idx: u8 = 0;
    let mut visited = [false; 80];
    for _step in 0..80 {
        if visited[idx as usize] {
            break;
        }
        visited[idx as usize] = true;
        let entry_off = ((sat_base as usize) + (idx as usize) * 8) & 0xFFFF;
        let mut bytes = [0u8; 8];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = vram[(entry_off + i) & 0xFFFF];
        }
        let s = decode_sprite_entry(&bytes);

        // Screen-space top-left:
        let sy = s.y_raw as i32 - 0x80;
        let sx_screen = s.x_raw as i32 - 0x80;
        let h_px = (s.h_cells as i32) * 8;
        let w_px = (s.w_cells as i32) * 8;

        if (line as i32) >= sy && (line as i32) < sy + h_px && s.priority == want_priority {
            // Row inside sprite, in tile-row coords.
            let row = (line as i32) - sy;
            let row = if s.vflip { h_px - 1 - row } else { row };
            let tile_row = (row / 8) as u32;
            let row_in_tile = (row % 8) as usize;

            for col in 0..w_px {
                let screen_x = sx_screen + col;
                if screen_x < 0 || screen_x >= width as i32 {
                    continue;
                }
                if out[screen_x as usize].is_some() {
                    continue; // earlier sprite already drew here
                }
                let lcol = if s.hflip { w_px - 1 - col } else { col };
                let tile_col = (lcol / 8) as u32;
                let col_in_tile = (lcol % 8) as usize;
                // Sprite tile order is column-major within the rectangle:
                // tile (tx, ty) = base + tx * h_cells + ty.
                let tile_off =
                    (s.tile_index as u32) + tile_col * (s.h_cells as u32) + tile_row;
                let tile_base = (tile_off as usize * 32) & 0xFFFF;
                let src_byte = vram[(tile_base + row_in_tile * 4 + (col_in_tile >> 1)) & 0xFFFF];
                let nib = if (col_in_tile & 1) == 0 {
                    (src_byte >> 4) & 0x0F
                } else {
                    src_byte & 0x0F
                };
                if nib != 0 {
                    out[screen_x as usize] = Some(s.palette * 16 + nib);
                }
            }
        }

        if s.link == 0 {
            break;
        }
        idx = s.link;
    }
    out
}

// ---- driver ------------------------------------------------------------

/// Read the full 64 KiB of VRAM via the stub's chunked qMdsVram path.
async fn read_full_vram<T: UsbTransport>(sync: &mut StubSync<T>) -> Result<Box<[u8; 0x10000]>> {
    let total: u32 = 0x1_0000;
    let chunks = total.div_ceil(VRAM_CHUNK_MAX);
    info!(chunks = chunks, "vram bulk read: {chunks} chunks");
    let mut buf = vec![0u8; total as usize];
    let mut off: u32 = 0;
    while off < total {
        let len = (total - off).min(VRAM_CHUNK_MAX);
        let chunk = sync
            .read_vram(off, len)
            .await
            .map_err(|e| anyhow::anyhow!("read_vram@{:#x}+{}: {e}", off, len))?;
        if chunk.len() as u32 != len {
            bail!(
                "read_vram@{:#x}: short chunk ({} of {})",
                off,
                chunk.len(),
                len
            );
        }
        buf[off as usize..(off + len) as usize].copy_from_slice(&chunk);
        off += len;
    }
    let arr: Box<[u8; 0x10000]> = buf
        .into_boxed_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("vram size mismatch"))?;
    Ok(arr)
}

async fn read_u16_be<T: UsbTransport>(sync: &mut StubSync<T>, addr: u32) -> Result<u16> {
    let raw = sync
        .read_memory(addr, 2)
        .await
        .map_err(|e| anyhow::anyhow!("read_memory@{:#x}: {e}", addr))?;
    if raw.len() != 2 {
        bail!("read_memory@{:#x}: expected 2 bytes, got {}", addr, raw.len());
    }
    Ok(u16::from_be_bytes([raw[0], raw[1]]))
}

/// Pure rendering core — pulled out so a unit test can exercise it without
/// the async transport dance. Returns an `(rgb_bytes, width)` pair; height
/// is fixed to [`FRAME_H`].
#[allow(clippy::too_many_arguments)]
fn render_rgb(
    reg: &[u8; 19],
    palette: &[[u8; 3]; 64],
    vram: &[u8; 0x10000],
    vsram: &[u8; 80],
    bga: u32,
    bgb: u32,
    slist: u32,
    hscroll_table: u32,
) -> (Vec<u8>, usize) {
    let st = parse_vdp_state(reg);
    let width = if st.h40 { 320 } else { 256 };
    let mut rgb = vec![0u8; width * FRAME_H * 3];

    if !st.display_enabled {
        // Blank frame — still useful as "screen off".
        return (rgb, width);
    }

    let backdrop = palette[st.backdrop_idx as usize];

    for line in 0..FRAME_H {
        let (ha, hb) = read_hscroll(vram, hscroll_table, &st, line);
        // VScroll: VSRAM is column-indexed in cell pairs. We compute per-x
        // inside the loop below (cheap: state.vscroll_per_cell flag).
        let lowb = render_plane_line(vram, bgb, &st, width, line, hb, 0, false);
        let lowa = render_plane_line(vram, bga, &st, width, line, ha, 0, false);
        let lows = render_sprites_line(vram, slist, width, line, false);
        let highb = render_plane_line(vram, bgb, &st, width, line, hb, 0, true);
        let higha = render_plane_line(vram, bga, &st, width, line, ha, 0, true);
        let highs = render_sprites_line(vram, slist, width, line, true);

        // Per-x VScroll: redo plane samples if per-cell mode is set. To
        // keep this simple, we rebuild lowa/lowb/highla/highb per cell-col.
        // For v1 with vscroll_per_cell=false the above already nails it.
        let dst_row = line * width * 3;
        for x in 0..width {
            // VScroll lookup (cheap in non-per-cell mode).
            let (va, vb) = read_vscroll(vsram, (x as u32) / 16, &st);
            // We didn't apply va/vb yet in low/highla/lowb/highb — for v1
            // we trade per-pixel cost for code simplicity. So if VScroll
            // is non-zero, we recompute the relevant plane sample here.
            let resolve = |plane_base: u32, vs: i32, ha_: i32, want_pri: bool| -> Option<u8> {
                let plane_h_px = (st.plane_h_cells * 8) as i32;
                let wy = ((line as i32 + vs).rem_euclid(plane_h_px)) as u32;
                let cy = wy / 8;
                let row_in_tile = (wy % 8) as usize;
                let wx = ((x as i32 - ha_).rem_euclid(st.plane_w_cells as i32 * 8)) as u32;
                let cx = wx / 8;
                let col_in_tile = (wx % 8) as usize;
                let word =
                    nametable_word_at(vram, plane_base, st.plane_w_cells, st.plane_h_cells, cx, cy);
                let cell = decode_nametable_entry(word);
                if cell.priority != want_pri {
                    return None;
                }
                let src_row = if cell.vflip { 7 - row_in_tile } else { row_in_tile };
                let src_col = if cell.hflip { 7 - col_in_tile } else { col_in_tile };
                let tile_base = (cell.tile_index as usize) * 32;
                let byte = vram[(tile_base + src_row * 4 + (src_col >> 1)) & 0xFFFF];
                let nib = if (src_col & 1) == 0 {
                    (byte >> 4) & 0x0F
                } else {
                    byte & 0x0F
                };
                if nib != 0 {
                    Some(cell.palette * 16 + nib)
                } else {
                    None
                }
            };

            let lb = if vb != 0 { resolve(bgb, vb, hb, false) } else { lowb[x] };
            let la = if va != 0 { resolve(bga, va, ha, false) } else { lowa[x] };
            let hb_ = if vb != 0 { resolve(bgb, vb, hb, true) } else { highb[x] };
            let ha_ = if va != 0 { resolve(bga, va, ha, true) } else { higha[x] };

            // Compositing: high sprites > high A > high B > low sprites > low A > low B > backdrop.
            let pal_idx = highs[x]
                .or(ha_)
                .or(hb_)
                .or(lows[x])
                .or(la)
                .or(lb);
            let rgb_px = match pal_idx {
                Some(i) => palette[(i as usize) & 0x3F],
                None => backdrop,
            };
            let off = dst_row + x * 3;
            rgb[off] = rgb_px[0];
            rgb[off + 1] = rgb_px[1];
            rgb[off + 2] = rgb_px[2];
        }
    }

    (rgb, width)
}

/// Read every VDP shadow input from hardware, render a 320×224 (or 256×224)
/// RGB frame, encode to PNG. Returns the PNG byte stream.
pub async fn render_frame<T: UsbTransport>(
    sync: &mut StubSync<T>,
    sym: &SgdkSymbols,
) -> Result<Vec<u8>> {
    // a) regValues[19].
    let reg_raw = sync
        .read_memory(sym.reg_values, 19)
        .await
        .map_err(|e| anyhow::anyhow!("read_memory regValues: {e}"))?;
    if reg_raw.len() != 19 {
        bail!("regValues read returned {} bytes (expected 19)", reg_raw.len());
    }
    let mut reg = [0u8; 19];
    reg.copy_from_slice(&reg_raw);

    // b) 5 u16 BE addresses from work RAM.
    let bga = read_u16_be(sync, sym.bga_addr).await.context("bga_addr")? as u32;
    let bgb = read_u16_be(sync, sym.bgb_addr).await.context("bgb_addr")? as u32;
    let slist = read_u16_be(sync, sym.slist_addr).await.context("slist_addr")? as u32;
    let _window = read_u16_be(sync, sym.window_addr).await.context("window_addr")? as u32;
    let hscroll = read_u16_be(sync, sym.hscroll).await.context("hscroll")? as u32;

    // c) CRAM, d) VSRAM.
    let cram = sync
        .read_cram()
        .await
        .map_err(|e| anyhow::anyhow!("read_cram: {e}"))?;
    let vsram = sync
        .read_vsram()
        .await
        .map_err(|e| anyhow::anyhow!("read_vsram: {e}"))?;
    let palette = decode_cram(&cram);

    // e) Full 64 KiB VRAM (heavy — done last).
    let vram = read_full_vram(sync).await?;

    // f-g) Render.
    let (rgb, width) = render_rgb(&reg, &palette, &vram, &vsram, bga, bgb, slist, hscroll);

    // h) PNG encode (RGB8).
    let mut buf = Vec::with_capacity(rgb.len() / 4);
    {
        let mut enc = png::Encoder::new(&mut buf, width as u32, FRAME_H as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc
            .write_header()
            .map_err(|e| anyhow::anyhow!("png header: {e}"))?;
        w.write_image_data(&rgb)
            .map_err(|e| anyhow::anyhow!("png data: {e}"))?;
    }
    Ok(buf)
}

// ---- tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_cram_known_color() {
        // Word $0EEE = 0000 1110 1110 1110 → R=7, G=7, B=7 → white (255,255,255).
        let mut c = [0u8; 128];
        c[0] = 0x0E;
        c[1] = 0xEE;
        // Word at index 1 = $0002 → R=1, G=0, B=0 → R=52.
        c[2] = 0x00;
        c[3] = 0x02;
        // Word at index 2 = $0E00 → R=0, G=0, B=7 → B=255.
        c[4] = 0x0E;
        c[5] = 0x00;
        let pal = decode_cram(&c);
        assert_eq!(pal[0], [255, 255, 255]);
        assert_eq!(pal[1], [52, 0, 0]);
        assert_eq!(pal[2], [0, 0, 255]);
    }

    #[test]
    fn decode_nametable_entry_packs_bits() {
        // priority=1, palette=2, vflip=1, hflip=0, tile=0x123
        let word: u16 = 0x8000 | (2 << 13) | 0x1000 | 0x0123;
        let c = decode_nametable_entry(word);
        assert!(c.priority);
        assert_eq!(c.palette, 2);
        assert!(c.vflip);
        assert!(!c.hflip);
        assert_eq!(c.tile_index, 0x123);
    }

    #[test]
    fn decode_sprite_entry_packs_bits() {
        // y=0x180, size=0x05 (w=2, h=2), link=0x07,
        // attr: priority=1, palette=1, vflip=0, hflip=1, tile=0x055
        // x=0x100.
        let attr: u16 = 0x8000 | (1 << 13) | 0x0800 | 0x0055;
        let mut b = [0u8; 8];
        b[0] = 0x01;
        b[1] = 0x80;
        b[2] = 0x05;
        b[3] = 0x07;
        b[4] = (attr >> 8) as u8;
        b[5] = attr as u8;
        b[6] = 0x01;
        b[7] = 0x00;
        let s = decode_sprite_entry(&b);
        assert_eq!(s.y_raw, 0x180);
        assert_eq!(s.w_cells, 2);
        assert_eq!(s.h_cells, 2);
        assert_eq!(s.link, 7);
        assert!(s.priority);
        assert_eq!(s.palette, 1);
        assert!(!s.vflip);
        assert!(s.hflip);
        assert_eq!(s.tile_index, 0x55);
        assert_eq!(s.x_raw, 0x100);
    }

    #[test]
    fn render_blank_when_display_disabled() {
        // reg[1] bit 6 = 0 → display off → solid black PNG-equivalent (rgb buf
        // all zero), regardless of palette / VRAM contents.
        let reg = [0u8; 19];
        let palette = [[1u8, 2, 3]; 64];
        let vram = Box::new([0xFFu8; 0x10000]);
        let vsram = [0u8; 80];
        let (rgb, w) = render_rgb(&reg, &palette, &vram, &vsram, 0, 0, 0, 0);
        assert_eq!(w, 256); // reg[12] bit 0 = 0 ⇒ H32
        assert_eq!(rgb.len(), 256 * 224 * 3);
        assert!(rgb.iter().all(|&b| b == 0));
    }

    #[test]
    fn render_h40_backdrop_only() {
        // Display on, H40 (320 px), backdrop = palette index 1.
        let mut reg = [0u8; 19];
        reg[1] = 0x40; // display enable
        reg[12] = 0x01; // H40
        reg[7] = 0x01; // backdrop = palette idx 1
        // Plane size 32×32 (default 0/0). Empty VRAM ⇒ every nametable
        // entry = 0 ⇒ tile 0 ⇒ all-zero pixels ⇒ transparent everywhere ⇒
        // backdrop wins on every pixel.
        let mut palette = [[0u8; 3]; 64];
        palette[1] = [11, 22, 33];
        let vram = Box::new([0u8; 0x10000]);
        let vsram = [0u8; 80];
        let (rgb, w) = render_rgb(&reg, &palette, &vram, &vsram, 0, 0, 0, 0);
        assert_eq!(w, 320);
        assert_eq!(rgb.len(), 320 * 224 * 3);
        // First pixel should be the backdrop colour.
        assert_eq!(&rgb[0..3], &[11, 22, 33]);
        // A pixel deep in the frame too.
        let off = (100 * 320 + 200) * 3;
        assert_eq!(&rgb[off..off + 3], &[11, 22, 33]);
    }
}
