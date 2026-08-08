//! PNG decoding.
//!
//! Covers the non-interlaced subset: bit depths 1 to 16 and colour types 0
//! (grey), 2 (truecolour), 3 (palette), 4 (grey+alpha) and 6 (truecolour+
//! alpha). Sixteen-bit samples keep only their high byte, which is all a
//! 24-bit framebuffer can show, and anything with an alpha channel is
//! composited over white — [`Image`] has nowhere to put transparency, and
//! white loses less of a typical web graphic than black would.
//!
//! Adam7 interlacing is rejected rather than approximated: a wrongly
//! de-interlaced image is worse than no image, because it looks like a decoder
//! bug somewhere else.
//!
//! Chunk CRCs are not verified. A single flipped bit would otherwise cost the
//! whole picture, and every byte the decoder reads is bounds-checked anyway,
//! so a corrupt chunk can only produce wrong pixels, never a wrong access.

use alloc::vec::Vec;

use crate::color::Color;

use super::{inflate, Image, MAX_PIXELS};

pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct Header {
    width: usize,
    height: usize,
    depth: u8,
    colour: u8,
    channels: usize,
}

fn be_u32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(off..off.checked_add(4)?)?.try_into().ok()?))
}

impl Header {
    fn parse(data: &[u8]) -> Option<Header> {
        let width = be_u32(data, 0)? as usize;
        let height = be_u32(data, 4)? as usize;
        let depth = *data.get(8)?;
        let colour = *data.get(9)?;
        let compression = *data.get(10)?;
        let filter = *data.get(11)?;
        let interlace = *data.get(12)?;

        if width == 0 || height == 0 {
            return None;
        }
        if width.checked_mul(height)? > MAX_PIXELS {
            return None;
        }
        // Only one compression and one filter method have ever been defined,
        // and Adam7 we decline.
        if compression != 0 || filter != 0 || interlace != 0 {
            return None;
        }

        let channels = match (colour, depth) {
            (0, 1 | 2 | 4 | 8 | 16) => 1,
            (2, 8 | 16) => 3,
            (3, 1 | 2 | 4 | 8) => 1,
            (4, 8 | 16) => 2,
            (6, 8 | 16) => 4,
            _ => return None,
        };

        Some(Header { width, height, depth, colour, channels })
    }
}

pub fn decode(bytes: &[u8]) -> Option<Image> {
    if bytes.get(..8)? != SIGNATURE {
        return None;
    }

    let mut header: Option<Header> = None;
    let mut palette: Vec<Color> = Vec::new();
    let mut palette_alpha: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut off = 8usize;

    loop {
        let len = be_u32(bytes, off)? as usize;
        let kind = bytes.get(off.checked_add(4)?..off.checked_add(8)?)?;
        let start = off.checked_add(8)?;
        let end = start.checked_add(len)?;
        let data = bytes.get(start..end)?;

        match kind {
            b"IHDR" => header = Some(Header::parse(data)?),
            b"PLTE" => {
                if len % 3 != 0 || len > 256 * 3 {
                    return None;
                }
                palette = data.chunks_exact(3).map(|c| Color::rgb(c[0], c[1], c[2])).collect();
            }
            // For palette images tRNS is one alpha byte per entry. For the
            // other colour types it names a single transparent colour, which
            // we ignore: compositing it over white would silently rewrite
            // pixels the author meant to keep.
            b"tRNS" => palette_alpha = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            // Walking off the end without meeting IEND falls out of the loop
            // as `None`. A partly downloaded PNG is therefore no image rather
            // than most of one, which is the honest answer when we cannot know
            // how many scanlines were meant to follow.
            b"IEND" => break,
            _ => {}
        }

        // Step over the four CRC bytes as well.
        off = end.checked_add(4)?;
    }

    let header = header?;
    if header.colour == 3 && palette.is_empty() {
        return None;
    }

    let bits_per_px = header.channels.checked_mul(header.depth as usize)?;
    let stride = header.width.checked_mul(bits_per_px)?.checked_add(7)? / 8;
    // Sub, Average and Paeth look back one pixel, rounded up to whole bytes.
    let back = (bits_per_px / 8).max(1);
    let raw_len = stride.checked_add(1)?.checked_mul(header.height)?;

    let mut raw = inflate::zlib(&idat)?;
    if raw.len() < raw_len {
        return None;
    }

    unfilter(&mut raw, stride, header.height, back)?;

    let mut pixels = Vec::with_capacity(header.width.checked_mul(header.height)?);
    for y in 0..header.height {
        let row_start = y.checked_mul(stride.checked_add(1)?)?.checked_add(1)?;
        let row = raw.get(row_start..row_start.checked_add(stride)?)?;
        for x in 0..header.width {
            pixels.push(pixel(&header, row, x, &palette, &palette_alpha)?);
        }
    }

    Some(Image { width: header.width, height: header.height, pixels })
}

/// Reverse the per-scanline filters in place, leaving each row's filter byte
/// where it is so the row offsets stay easy to compute.
fn unfilter(raw: &mut [u8], stride: usize, height: usize, back: usize) -> Option<()> {
    let row_len = stride.checked_add(1)?;
    for y in 0..height {
        let start = y.checked_mul(row_len)?;
        let filter = *raw.get(start)?;
        let row = start.checked_add(1)?;
        if row.checked_add(stride)? > raw.len() {
            return None;
        }
        // Row zero behaves as if the row above it were all zeroes, which is
        // exactly what Up and Paeth want.
        let prev = start.checked_sub(row_len).map(|p| p + 1);

        for i in 0..stride {
            let a = if i >= back { raw[row + i - back] } else { 0 };
            let b = prev.map_or(0, |p| raw[p + i]);
            let c = match prev {
                Some(p) if i >= back => raw[p + i - back],
                _ => 0,
            };
            let x = raw[row + i];
            raw[row + i] = match filter {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return None,
            };
        }
    }
    Some(())
}

/// The PNG Paeth predictor: whichever of left, above and above-left is
/// closest to their linear combination.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let pa = (p - a as i16).abs();
    let pb = (p - b as i16).abs();
    let pc = (p - c as i16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// One sample from an unfiltered scanline, reduced to eight bits.
fn sample(row: &[u8], depth: u8, index: usize) -> Option<u8> {
    match depth {
        16 => row.get(index.checked_mul(2)?).copied(),
        8 => row.get(index).copied(),
        1 | 2 | 4 => {
            let per_byte = 8 / depth as usize;
            let byte = *row.get(index / per_byte)?;
            let shift = 8 - depth as usize * (index % per_byte + 1);
            Some((byte >> shift) & ((1u8 << depth) - 1))
        }
        _ => None,
    }
}

/// Spread a sub-byte sample over the full 0-255 range, so 4-bit white comes
/// out as 255 rather than 15.
fn expand(v: u8, depth: u8) -> u8 {
    match depth {
        1 => v * 255,
        2 => v * 85,
        4 => v * 17,
        _ => v,
    }
}

fn pixel(h: &Header, row: &[u8], x: usize, palette: &[Color], alpha: &[u8]) -> Option<Color> {
    let base = x.checked_mul(h.channels)?;
    match h.colour {
        0 => {
            let v = expand(sample(row, h.depth, x)?, h.depth);
            Some(Color::gray(v))
        }
        2 => Some(Color::rgb(
            sample(row, h.depth, base)?,
            sample(row, h.depth, base + 1)?,
            sample(row, h.depth, base + 2)?,
        )),
        3 => {
            let index = sample(row, h.depth, x)? as usize;
            let colour = *palette.get(index)?;
            let a = alpha.get(index).copied().unwrap_or(255);
            Some(colour.over(Color::WHITE, a))
        }
        4 => {
            let v = expand(sample(row, h.depth, base)?, h.depth);
            let a = expand(sample(row, h.depth, base + 1)?, h.depth);
            Some(Color::gray(v).over(Color::WHITE, a))
        }
        _ => {
            let c = Color::rgb(
                sample(row, h.depth, base)?,
                sample(row, h.depth, base + 1)?,
                sample(row, h.depth, base + 2)?,
            );
            Some(c.over(Color::WHITE, sample(row, h.depth, base + 3)?))
        }
    }
}
