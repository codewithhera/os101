//! Baseline sequential JPEG decoding.
//!
//! SOF0 and SOF1 only. Progressive (SOF2), lossless, differential and
//! arithmetic-coded frames are refused at the marker rather than half-decoded,
//! because a progressive scan read as a baseline one produces a plausible
//! looking image made of the wrong numbers.
//!
//! One and three component frames are handled — greyscale, and YCbCr at 4:4:4,
//! 4:2:2 or 4:2:0. Chroma is upsampled by nearest neighbour, which is what the
//! sampling factors literally mean and costs nothing; the smoother
//! reconstruction everyone else uses is not worth the code here.

use alloc::vec;
use alloc::vec::Vec;

use crate::color::Color;

use super::{Image, MAX_PIXELS};

/// Zigzag order: coefficient `k` of the entropy-coded sequence belongs at
/// `ZIGZAG[k]` of the 8x8 block.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

struct Component {
    id: u8,
    /// Horizontal and vertical sampling factors, 1 to 4.
    h: usize,
    v: usize,
    quant: usize,
    dc_table: usize,
    ac_table: usize,
}

struct Frame {
    width: usize,
    height: usize,
    components: Vec<Component>,
    hmax: usize,
    vmax: usize,
}

// ── Huffman ──────────────────────────────────────────────────────────────

/// A JPEG Huffman table in the form Annex F decodes with: for each code
/// length, the first and last code of that length and where its symbols start.
struct HuffTable {
    mincode: [i32; 17],
    /// -1 where no code of that length exists, which no non-negative code can
    /// ever match.
    maxcode: [i32; 17],
    valptr: [usize; 17],
    symbols: Vec<u8>,
}

impl HuffTable {
    fn build(counts: &[u8; 16], symbols: Vec<u8>) -> Option<HuffTable> {
        let mut table = HuffTable {
            mincode: [0; 17],
            maxcode: [-1; 17],
            valptr: [0; 17],
            symbols,
        };
        let mut code: i32 = 0;
        let mut next = 0usize;
        for len in 1..=16 {
            let n = counts[len - 1] as i32;
            if n > 0 {
                table.valptr[len] = next;
                table.mincode[len] = code;
                code += n;
                next += n as usize;
                table.maxcode[len] = code - 1;
            }
            // More codes than the length can hold means the table is
            // over-subscribed and no canonical assignment exists.
            if code > (1 << len) {
                return None;
            }
            code <<= 1;
        }
        if next != table.symbols.len() {
            return None;
        }
        Some(table)
    }

    fn decode(&self, br: &mut BitReader) -> Option<u8> {
        let mut code: i32 = 0;
        for len in 1..=16 {
            code = (code << 1) | br.bit() as i32;
            if self.maxcode[len] >= code {
                let slot = self.valptr[len].checked_add((code - self.mincode[len]) as usize)?;
                return self.symbols.get(slot).copied();
            }
        }
        None
    }
}

// ── Entropy-coded bit stream ─────────────────────────────────────────────

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    count: u32,
    /// Set once a marker or the end of the buffer has been reached.
    spent: bool,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, buf: 0, count: 0, spent: false }
    }

    /// Reads never fail; past the end of the scan they yield zeroes.
    ///
    /// A truncated JPEG is common enough — a half-downloaded page produces one
    /// every time — and showing the rows that did arrive beats showing
    /// nothing. Every loop that consumes bits is bounded by the frame's
    /// dimensions, so zeroes cannot spin one forever.
    fn bit(&mut self) -> u32 {
        if self.count == 0 && !self.fill() {
            return 0;
        }
        self.count -= 1;
        (self.buf >> self.count) & 1
    }

    fn fill(&mut self) -> bool {
        if self.spent {
            return false;
        }
        let Some(&b) = self.data.get(self.pos) else {
            self.spent = true;
            return false;
        };
        if b == 0xFF {
            // 0xFF00 is a stuffed literal 0xFF; anything else is a marker,
            // and markers end the entropy-coded segment.
            if self.data.get(self.pos + 1) != Some(&0x00) {
                self.spent = true;
                return false;
            }
            self.pos += 2;
        } else {
            self.pos += 1;
        }
        self.buf = b as u32;
        self.count = 8;
        true
    }

    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit();
        }
        v
    }

    /// Step over a restart marker, discarding whatever pad bits precede it.
    ///
    /// The marker should sit at the very next byte boundary, but a stream that
    /// has drifted is exactly the case restart markers exist to recover from,
    /// so we scan forward for it instead of insisting.
    fn restart(&mut self) {
        self.buf = 0;
        self.count = 0;
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF {
                let marker = self.data[self.pos + 1];
                if (0xD0..=0xD7).contains(&marker) {
                    self.pos += 2;
                    self.spent = false;
                }
                return;
            }
            self.pos += 1;
        }
    }
}

/// Turn an `n`-bit magnitude into the signed coefficient it encodes.
fn extend(v: u32, n: u32) -> i32 {
    if n == 0 {
        return 0;
    }
    let v = v as i32;
    if v < 1 << (n - 1) {
        v - (1 << n) + 1
    } else {
        v
    }
}

// ── Segment parsing ──────────────────────────────────────────────────────

fn be_u16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(off..off.checked_add(2)?)?.try_into().ok()?))
}

/// Advance to the next marker, stepping over any run of 0xFF fill bytes.
fn next_marker(bytes: &[u8], pos: &mut usize) -> Option<u8> {
    while *bytes.get(*pos)? != 0xFF {
        *pos += 1;
    }
    while *bytes.get(*pos)? == 0xFF {
        *pos += 1;
    }
    let marker = *bytes.get(*pos)?;
    *pos += 1;
    Some(marker)
}

fn parse_sof(seg: &[u8]) -> Option<Frame> {
    // Twelve-bit samples would need wider planes throughout; nothing on the
    // web uses them.
    if *seg.first()? != 8 {
        return None;
    }
    let height = be_u16(seg, 1)? as usize;
    let width = be_u16(seg, 3)? as usize;
    let count = *seg.get(5)? as usize;
    if width == 0 || height == 0 {
        return None;
    }
    if width.checked_mul(height)? > MAX_PIXELS {
        return None;
    }
    if count != 1 && count != 3 {
        return None;
    }

    let mut components = Vec::new();
    for i in 0..count {
        let base = 6 + i * 3;
        let spec = seg.get(base..base + 3)?;
        let h = (spec[1] >> 4) as usize;
        let v = (spec[1] & 0x0F) as usize;
        if h == 0 || h > 4 || v == 0 || v > 4 {
            return None;
        }
        let quant = spec[2] as usize;
        if quant > 3 {
            return None;
        }
        components.push(Component { id: spec[0], h, v, quant, dc_table: 0, ac_table: 0 });
    }

    let hmax = components.iter().map(|c| c.h).max()?;
    let vmax = components.iter().map(|c| c.v).max()?;
    Some(Frame { width, height, components, hmax, vmax })
}

fn parse_dqt(seg: &[u8], tables: &mut [[u16; 64]; 4]) -> Option<()> {
    let mut i = 0;
    while i < seg.len() {
        let spec = *seg.get(i)?;
        i += 1;
        let id = (spec & 0x0F) as usize;
        let table = tables.get_mut(id)?;
        match spec >> 4 {
            0 => {
                let d = seg.get(i..i.checked_add(64)?)?;
                for (slot, &v) in table.iter_mut().zip(d) {
                    *slot = v as u16;
                }
                i += 64;
            }
            1 => {
                let d = seg.get(i..i.checked_add(128)?)?;
                for (k, slot) in table.iter_mut().enumerate() {
                    *slot = u16::from_be_bytes([d[k * 2], d[k * 2 + 1]]);
                }
                i += 128;
            }
            _ => return None,
        }
    }
    Some(())
}

type HuffSlots = [Option<HuffTable>; 4];

fn parse_dht(seg: &[u8], dc: &mut HuffSlots, ac: &mut HuffSlots) -> Option<()> {
    let mut i = 0;
    while i < seg.len() {
        let spec = *seg.get(i)?;
        i += 1;
        let class = spec >> 4;
        let id = (spec & 0x0F) as usize;
        if class > 1 || id > 3 {
            return None;
        }

        let mut counts = [0u8; 16];
        counts.copy_from_slice(seg.get(i..i.checked_add(16)?)?);
        i += 16;
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let symbols = seg.get(i..i.checked_add(total)?)?.to_vec();
        i += total;

        let table = HuffTable::build(&counts, symbols)?;
        let slot = if class == 0 { dc.get_mut(id)? } else { ac.get_mut(id)? };
        *slot = Some(table);
    }
    Some(())
}

/// Attach the scan's table selectors to the frame's components.
///
/// Only a single interleaved scan is supported. Baseline JPEG does permit a
/// frame split into one scan per component, but nothing produces them, and
/// refusing is better than reconstructing two thirds of a picture.
fn parse_sos(seg: &[u8], frame: &mut Frame) -> Option<()> {
    let count = *seg.first()? as usize;
    if count != frame.components.len() {
        return None;
    }
    for i in 0..count {
        let base = 1 + i * 2;
        let spec = seg.get(base..base + 2)?;
        let component = frame.components.iter_mut().find(|c| c.id == spec[0])?;
        component.dc_table = (spec[1] >> 4) as usize;
        component.ac_table = (spec[1] & 0x0F) as usize;
        if component.dc_table > 3 || component.ac_table > 3 {
            return None;
        }
    }
    // Ss, Se, Ah and Al follow. Baseline pins them to 0, 63, 0, 0 but some
    // encoders are careless with them, so they are only read to confirm the
    // header is complete.
    seg.get(1 + count * 2..1 + count * 2 + 3)?;
    Some(())
}

// ── Block decoding ───────────────────────────────────────────────────────

/// Keep dequantised coefficients inside the range the fixed-point IDCT can
/// take without overflowing `i32`. A legal 8-bit frame never exceeds ±2048, so
/// the clamp only ever bites on corrupt data, where it turns an overflow into
/// a merely ugly block.
fn clamp_coefficient(v: i32) -> i32 {
    v.clamp(-(1 << 14), 1 << 14)
}

fn decode_block(
    br: &mut BitReader,
    dc: &HuffTable,
    ac: &HuffTable,
    quant: &[u16; 64],
    predictor: &mut i32,
    out: &mut [i32; 64],
) -> Option<()> {
    out.fill(0);

    let size = dc.decode(br)? as u32;
    if size > 15 {
        return None;
    }
    let diff = extend(br.bits(size), size);
    *predictor = predictor.saturating_add(diff);
    out[0] = clamp_coefficient(predictor.saturating_mul(quant[0] as i32));

    let mut k = 1usize;
    while k < 64 {
        let rs = ac.decode(br)?;
        let run = (rs >> 4) as usize;
        let size = (rs & 0x0F) as u32;
        if size == 0 {
            // 0xF0 skips sixteen zeroes; any other zero size is end-of-block.
            if run != 15 {
                break;
            }
            k += 16;
            continue;
        }
        k += run;
        if k >= 64 {
            return None;
        }
        let value = extend(br.bits(size), size);
        out[ZIGZAG[k]] = clamp_coefficient(value.saturating_mul(quant[k] as i32));
        k += 1;
    }
    Some(())
}

// ── Inverse DCT ──────────────────────────────────────────────────────────

/// One pass of the AAN-derived integer IDCT, constants scaled by 2^12.
/// Keeping it in integers avoids any question of what the FPU is doing in
/// kernel context.
///
/// The arithmetic is 64-bit because the column pass leaves values two bits
/// wider than it received and the row pass then multiplies them by 2^12
/// again: 32 bits is enough for a well-behaved block and not for a hostile
/// one, and `i64` costs nothing on x86-64.
///
/// Returns the four even outputs followed by the four odd ones; the caller
/// pairs them up, because the two passes scale and round differently.
fn idct_1d(s: [i64; 8]) -> [i64; 8] {
    let [s0, s1, s2, s3, s4, s5, s6, s7] = s;
    let p1 = (s2 + s6) * 2217;
    let mut t2 = p1 + s6 * -7567;
    let mut t3 = p1 + s2 * 3135;
    let mut t0 = (s0 + s4) * 4096;
    let mut t1 = (s0 - s4) * 4096;
    let x0 = t0 + t3;
    let x3 = t0 - t3;
    let x1 = t1 + t2;
    let x2 = t1 - t2;

    t0 = s7;
    t1 = s5;
    t2 = s3;
    t3 = s1;
    let p3 = t0 + t2;
    let p4 = t1 + t3;
    let p1 = t0 + t3;
    let p2 = t1 + t2;
    let p5 = (p3 + p4) * 4816;
    t0 *= 1223;
    t1 *= 8410;
    t2 *= 12586;
    t3 *= 6149;
    let p1 = p5 + p1 * -3685;
    let p2 = p5 + p2 * -10497;
    let p3 = p3 * -8034;
    let p4 = p4 * -1597;
    t3 += p1 + p4;
    t2 += p2 + p3;
    t1 += p2 + p4;
    t0 += p1 + p3;

    [x0, x1, x2, x3, t0, t1, t2, t3]
}

fn idct(block: &[i32; 64], out: &mut [u8; 64]) {
    let mut tmp = [0i64; 64];

    for i in 0..8 {
        // A block whose column is nothing but its DC term — the common case
        // in flat areas — collapses to a constant.
        if block[i + 8] == 0
            && block[i + 16] == 0
            && block[i + 24] == 0
            && block[i + 32] == 0
            && block[i + 40] == 0
            && block[i + 48] == 0
            && block[i + 56] == 0
        {
            let dc = block[i] as i64 * 4;
            for row in 0..8 {
                tmp[i + row * 8] = dc;
            }
            continue;
        }

        let [x0, x1, x2, x3, t0, t1, t2, t3] = idct_1d([
            block[i] as i64,
            block[i + 8] as i64,
            block[i + 16] as i64,
            block[i + 24] as i64,
            block[i + 32] as i64,
            block[i + 40] as i64,
            block[i + 48] as i64,
            block[i + 56] as i64,
        ]);
        // Shed the 2^12 the constants added, keeping two bits of headroom for
        // the row pass; the 512 rounds that shift.
        tmp[i] = (x0 + 512 + t3) >> 10;
        tmp[i + 56] = (x0 + 512 - t3) >> 10;
        tmp[i + 8] = (x1 + 512 + t2) >> 10;
        tmp[i + 48] = (x1 + 512 - t2) >> 10;
        tmp[i + 16] = (x2 + 512 + t1) >> 10;
        tmp[i + 40] = (x2 + 512 - t1) >> 10;
        tmp[i + 24] = (x3 + 512 + t0) >> 10;
        tmp[i + 32] = (x3 + 512 - t0) >> 10;
    }

    // 2^12 from the constants, the 2^2 left over from the column pass and 2^3
    // from the two dimensions' 1/√8 make 2^17 to remove; 65536 rounds it and
    // 128 << 17 undoes the encoder's level shift.
    const BIAS: i64 = 65536 + (128 << 17);

    for row in 0..8 {
        let o = row * 8;
        let [x0, x1, x2, x3, t0, t1, t2, t3] = idct_1d([
            tmp[o],
            tmp[o + 1],
            tmp[o + 2],
            tmp[o + 3],
            tmp[o + 4],
            tmp[o + 5],
            tmp[o + 6],
            tmp[o + 7],
        ]);
        out[o] = clamp_u8((x0 + BIAS + t3) >> 17);
        out[o + 7] = clamp_u8((x0 + BIAS - t3) >> 17);
        out[o + 1] = clamp_u8((x1 + BIAS + t2) >> 17);
        out[o + 6] = clamp_u8((x1 + BIAS - t2) >> 17);
        out[o + 2] = clamp_u8((x2 + BIAS + t1) >> 17);
        out[o + 5] = clamp_u8((x2 + BIAS - t1) >> 17);
        out[o + 3] = clamp_u8((x3 + BIAS + t0) >> 17);
        out[o + 4] = clamp_u8((x3 + BIAS - t0) >> 17);
    }
}

fn clamp_u8(v: i64) -> u8 {
    v.clamp(0, 255) as u8
}

// ── Colour ───────────────────────────────────────────────────────────────

/// JFIF YCbCr to RGB, in 16.16 fixed point.
fn ycbcr(y: u8, cb: u8, cr: u8) -> Color {
    let y = y as i64;
    let cb = cb as i64 - 128;
    let cr = cr as i64 - 128;
    Color::rgb(
        clamp_u8(y + ((91881 * cr) >> 16)),
        clamp_u8(y - ((22554 * cb + 46802 * cr) >> 16)),
        clamp_u8(y + ((116130 * cb) >> 16)),
    )
}

// ── Top level ────────────────────────────────────────────────────────────

pub fn decode(bytes: &[u8]) -> Option<Image> {
    if bytes.get(..2)? != [0xFF, 0xD8] {
        return None;
    }

    let mut quant = [[0u16; 64]; 4];
    let mut dc_tables: HuffSlots = [None, None, None, None];
    let mut ac_tables: HuffSlots = [None, None, None, None];
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut pos = 2usize;

    loop {
        let marker = next_marker(bytes, &mut pos)?;
        match marker {
            // Standalone markers carry no length field.
            0x01 | 0xD0..=0xD7 | 0xD8 => continue,
            // End of image before any scan: there is nothing to show.
            0xD9 => return None,
            _ => {}
        }

        let length = be_u16(bytes, pos)? as usize;
        if length < 2 {
            return None;
        }
        let end = pos.checked_add(length)?;
        let seg = bytes.get(pos + 2..end)?;

        match marker {
            0xC0 | 0xC1 => frame = Some(parse_sof(seg)?),
            // Progressive, lossless, differential and arithmetic frames. Half
            // decoding any of them would be worse than admitting defeat.
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xC4 => parse_dht(seg, &mut dc_tables, &mut ac_tables)?,
            0xDB => parse_dqt(seg, &mut quant)?,
            0xDD => restart_interval = be_u16(seg, 0)? as usize,
            0xDA => {
                let mut frame = frame?;
                parse_sos(seg, &mut frame)?;
                return scan(
                    &frame,
                    &quant,
                    &dc_tables,
                    &ac_tables,
                    bytes.get(end..)?,
                    restart_interval,
                );
            }
            _ => {}
        }

        pos = end;
    }
}

fn scan(
    frame: &Frame,
    quant: &[[u16; 64]; 4],
    dc_tables: &HuffSlots,
    ac_tables: &HuffSlots,
    data: &[u8],
    restart_interval: usize,
) -> Option<Image> {
    let mcus_x = frame.width.checked_add(frame.hmax * 8 - 1)? / (frame.hmax * 8);
    let mcus_y = frame.height.checked_add(frame.vmax * 8 - 1)? / (frame.vmax * 8);

    // Each component gets a plane padded out to whole MCUs, so block writes
    // never have to clip.
    let mut planes: Vec<Vec<u8>> = Vec::new();
    let mut sizes: Vec<(usize, usize)> = Vec::new();
    let mut budget = MAX_PIXELS.checked_mul(3)?;
    for component in &frame.components {
        let w = mcus_x.checked_mul(component.h)?.checked_mul(8)?;
        let h = mcus_y.checked_mul(component.v)?.checked_mul(8)?;
        let area = w.checked_mul(h)?;
        budget = budget.checked_sub(area)?;
        planes.push(vec![0u8; area]);
        sizes.push((w, h));
    }

    let mut br = BitReader::new(data);
    let mut predictors = vec![0i32; frame.components.len()];
    let mut coefficients = [0i32; 64];
    let mut block = [0u8; 64];
    let mut since_restart = 0usize;

    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart_interval > 0 && since_restart == restart_interval {
                br.restart();
                predictors.iter_mut().for_each(|p| *p = 0);
                since_restart = 0;
            }

            for (ci, component) in frame.components.iter().enumerate() {
                let dc = dc_tables.get(component.dc_table)?.as_ref()?;
                let ac = ac_tables.get(component.ac_table)?.as_ref()?;
                let q = quant.get(component.quant)?;
                let (plane_w, _) = *sizes.get(ci)?;

                for by in 0..component.v {
                    for bx in 0..component.h {
                        let predictor = predictors.get_mut(ci)?;
                        decode_block(&mut br, dc, ac, q, predictor, &mut coefficients)?;
                        idct(&coefficients, &mut block);

                        let x0 = (mx * component.h + bx) * 8;
                        let y0 = (my * component.v + by) * 8;
                        let plane = planes.get_mut(ci)?;
                        for row in 0..8 {
                            let start = (y0 + row) * plane_w + x0;
                            plane.get_mut(start..start + 8)?
                                .copy_from_slice(block.get(row * 8..row * 8 + 8)?);
                        }
                    }
                }
            }

            since_restart += 1;
        }
    }

    let mut pixels = Vec::with_capacity(frame.width.checked_mul(frame.height)?);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let mut s = [0u8; 3];
            for (ci, component) in frame.components.iter().enumerate() {
                let (w, h) = *sizes.get(ci)?;
                let sx = (x * component.h / frame.hmax).min(w.saturating_sub(1));
                let sy = (y * component.v / frame.vmax).min(h.saturating_sub(1));
                s[ci] = *planes.get(ci)?.get(sy * w + sx)?;
            }
            pixels.push(if frame.components.len() == 1 {
                Color::gray(s[0])
            } else {
                ycbcr(s[0], s[1], s[2])
            });
        }
    }

    Some(Image { width: frame.width, height: frame.height, pixels })
}
