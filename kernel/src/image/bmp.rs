//! Tiny BMP decoder for embedded bitmaps.
//!
//! Supports just what we need to display `assets/hera.bmp`:
//! - BITMAPFILEHEADER (14 bytes) with "BM" magic
//! - DIB header (any size ≥ 40; we only read width/height/planes/bpp/compression)
//! - 24-bit or 32-bit uncompressed pixels (BI_RGB) *or*
//!   32-bit BI_BITFIELDS (common for 32bpp BMPs — we ignore the masks and
//!   assume BGRA, which is what Windows/most tools actually emit)
//! - Bottom-up row order (positive height) and top-down (negative height)
//!
//! Pixels are decoded to full 24-bit colour. The framebuffer was grayscale
//! when this decoder was written, which is why it used to flatten everything
//! to luminance; now that [`Color`] carries RGB end to end, images keep it.

use alloc::vec;

use crate::color::Color;

use super::{Image, MAX_PIXELS};

fn le_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn le_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn le_i32(b: &[u8], off: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.len() < 54 || &data[0..2] != b"BM" {
        return None;
    }
    let bits_offset = le_u32(data, 10)? as usize;
    let _dib_size = le_u32(data, 14)?;
    let width = le_i32(data, 18)?;
    let height_signed = le_i32(data, 22)?;
    let planes = le_u16(data, 26)?;
    let bpp = le_u16(data, 28)?;
    let compression = le_u32(data, 30)?;

    if planes != 1 { return None; }
    if width <= 0 { return None; }
    if height_signed == 0 { return None; }
    // Accept BI_RGB (0) always; accept BI_BITFIELDS (3) for 32bpp (we assume BGRA).
    if !(compression == 0 || (compression == 3 && bpp == 32)) {
        return None;
    }
    if !(bpp == 24 || bpp == 32) { return None; }

    let width = width as usize;
    let top_down = height_signed < 0;
    let height = height_signed.unsigned_abs() as usize;

    let bytes_per_px = (bpp / 8) as usize;
    // Every size below is checked: a BMP header carries attacker-controlled
    // 32-bit dimensions, and `width * height` overflowing would let a bogus
    // image slip past the length check and index out of its own buffer.
    let row_raw = width.checked_mul(bytes_per_px)?;
    let row_stride = row_raw.checked_add(3)? & !3;
    let pixels = width.checked_mul(height)?;
    if pixels > MAX_PIXELS {
        return None;
    }

    let needed = bits_offset.checked_add(row_stride.checked_mul(height)?)?;
    if data.len() < needed { return None; }

    let mut out = vec![Color::BLACK; pixels];
    for y in 0..height {
        let src_row = if top_down { y } else { height - 1 - y };
        let row_start = bits_offset + src_row * row_stride;
        let row = &data[row_start..row_start + row_raw];
        let dst_row_start = y * width;
        for x in 0..width {
            let px = &row[x * bytes_per_px..x * bytes_per_px + bytes_per_px];
            // BMP stores pixels as B, G, R (, A).
            out[dst_row_start + x] = Color::rgb(px[2], px[1], px[0]);
        }
    }

    Some(Image {
        width,
        height,
        pixels: out,
    })
}
