// SPDX-License-Identifier: MIT
//! Captured framebuffer + libretro pixel-format conversion to RGBA8 / PNG.
//!
//! libra's video callback delivers frames in one of three retro pixel
//! formats:
//!   0 = XRGB1555  (16 bpp, 1 unused bit)
//!   1 = XRGB8888  (32 bpp, alpha unused)
//!   2 = RGB565    (16 bpp)
//!
//! We keep the latest frame in a `parking_lot::Mutex<Option<Frame>>` and
//! reuse the backing `Vec<u8>` between callbacks to avoid per-frame
//! allocations.

use std::sync::Arc;

use parking_lot::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Xrgb1555,
    Xrgb8888,
    Rgb565,
}

impl PixelFormat {
    pub fn from_libretro(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Xrgb1555),
            1 => Some(Self::Xrgb8888),
            2 => Some(Self::Rgb565),
            _ => None,
        }
    }
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Xrgb8888 => 4,
            Self::Xrgb1555 | Self::Rgb565 => 2,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub pitch: usize,
    pub fmt: PixelFormat,
    pub data: Vec<u8>,
    /// Frame counter at the time of capture.
    pub frame: u64,
}

#[derive(Default, Clone)]
pub struct FrameSlot {
    inner: Arc<Mutex<Option<Frame>>>,
}

impl FrameSlot {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    pub fn store(&self, w: u32, h: u32, pitch: usize, fmt: PixelFormat, src: &[u8], frame: u64) {
        let row_bytes = (w as usize) * fmt.bytes_per_pixel();
        let needed = row_bytes * h as usize;
        let mut g = self.inner.lock();
        let slot = g.get_or_insert_with(|| Frame {
            w,
            h,
            pitch: row_bytes,
            fmt,
            data: Vec::with_capacity(needed),
            frame: 0,
        });
        slot.w = w;
        slot.h = h;
        slot.pitch = row_bytes;
        slot.fmt = fmt;
        slot.frame = frame;
        slot.data.clear();
        slot.data.reserve(needed);
        // Repack row by row, dropping any source padding.
        for y in 0..(h as usize) {
            let src_off = y * pitch;
            let src_end = src_off + row_bytes;
            if src_end > src.len() {
                break;
            }
            slot.data.extend_from_slice(&src[src_off..src_end]);
        }
    }

    pub fn snapshot(&self) -> Option<Frame> {
        self.inner.lock().clone()
    }
}

/// Convert a tightly-packed (no row padding) framebuffer to RGBA8.
pub fn to_rgba8(frame: &Frame) -> Vec<u8> {
    let pixels = (frame.w as usize) * (frame.h as usize);
    let mut out = Vec::with_capacity(pixels * 4);
    match frame.fmt {
        PixelFormat::Xrgb8888 => {
            // Native-endian: B G R X. Swap to R G B A.
            for chunk in frame.data.chunks_exact(4) {
                out.extend_from_slice(&[chunk[2], chunk[1], chunk[0], 0xFF]);
            }
        }
        PixelFormat::Rgb565 => {
            for chunk in frame.data.chunks_exact(2) {
                let v = u16::from_le_bytes([chunk[0], chunk[1]]);
                let r = ((v >> 11) & 0x1F) as u8;
                let g = ((v >> 5) & 0x3F) as u8;
                let b = (v & 0x1F) as u8;
                out.extend_from_slice(&[
                    (r << 3) | (r >> 2),
                    (g << 2) | (g >> 4),
                    (b << 3) | (b >> 2),
                    0xFF,
                ]);
            }
        }
        PixelFormat::Xrgb1555 => {
            for chunk in frame.data.chunks_exact(2) {
                let v = u16::from_le_bytes([chunk[0], chunk[1]]);
                let r = ((v >> 10) & 0x1F) as u8;
                let g = ((v >> 5) & 0x1F) as u8;
                let b = (v & 0x1F) as u8;
                out.extend_from_slice(&[
                    (r << 3) | (r >> 2),
                    (g << 3) | (g >> 2),
                    (b << 3) | (b >> 2),
                    0xFF,
                ]);
            }
        }
    }
    out
}

/// Encode an RGBA8 buffer at `w x h` into a PNG byte stream.
pub fn rgba8_to_png(rgba: &[u8], w: u32, h: u32) -> anyhow::Result<Vec<u8>> {
    if rgba.len() != (w as usize) * (h as usize) * 4 {
        anyhow::bail!("rgba length mismatch: have {}, expect {}", rgba.len(), (w * h * 4));
    }
    let mut buf: Vec<u8> = Vec::with_capacity(rgba.len() / 2);
    {
        let mut enc = png::Encoder::new(&mut buf, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb565_to_png_roundtrip() {
        // 16x8 frame: top half red, bottom half blue, in RGB565.
        let (w, h) = (16u32, 8u32);
        let pitch = (w as usize) * 2;
        let mut data = vec![0u8; pitch * h as usize];
        let red: u16 = 0xF800; // R=31 G=0 B=0
        let blue: u16 = 0x001F; // R=0 G=0 B=31
        for y in 0..(h as usize) {
            let pix = if y < (h as usize) / 2 { red } else { blue };
            for x in 0..(w as usize) {
                let off = y * pitch + x * 2;
                data[off..off + 2].copy_from_slice(&pix.to_le_bytes());
            }
        }
        let frame = Frame {
            w,
            h,
            pitch,
            fmt: PixelFormat::Rgb565,
            data,
            frame: 0,
        };
        let rgba = to_rgba8(&frame);
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        // First pixel red.
        assert_eq!(&rgba[0..4], &[0xFF, 0x00, 0x00, 0xFF]);
        // Last pixel blue.
        let last = rgba.len() - 4;
        assert_eq!(&rgba[last..last + 4], &[0x00, 0x00, 0xFF, 0xFF]);

        let png = rgba8_to_png(&rgba, w, h).expect("png encode");
        // Parse back and check dimensions.
        let dec = png::Decoder::new(png.as_slice());
        let reader = dec.read_info().unwrap();
        let info = reader.info();
        assert_eq!(info.width, w);
        assert_eq!(info.height, h);
    }
}
