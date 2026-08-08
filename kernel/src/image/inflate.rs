//! DEFLATE (RFC 1951) and zlib (RFC 1950) decompression.
//!
//! PNG stores its pixels as a zlib stream, so this is the price of admission
//! for reading one. It lives in its own module because DEFLATE is a format in
//! its own right and nothing here knows anything about images.
//!
//! Huffman decoding follows the counts-and-symbols scheme from Mark Adler's
//! `puff`: instead of building a lookup table it walks one bit at a time,
//! comparing against the first code of each length. That is slower than a
//! table-driven decoder and far easier to convince yourself is safe on
//! malformed input — the walk simply runs out of lengths and fails.

use alloc::vec;
use alloc::vec::Vec;

/// Ceiling on one decompressed stream. The compression ratio is unbounded, so
/// a few hundred bytes of hostile DEFLATE can ask for gigabytes; the limit has
/// to be on the output rather than the input.
const MAX_OUTPUT: usize = 16 * 1024 * 1024;

/// Longest Huffman code DEFLATE permits.
const MAX_BITS: usize = 15;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// The permuted order RFC 1951 writes the code-length code lengths in.
const CLEN_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct BitReader<'a> {
    data: &'a [u8],
    byte: usize,
    /// Bits already taken from `data[byte]`, always 0..8.
    bit: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, byte: 0, bit: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let b = *self.data.get(self.byte)?;
        let v = (b >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.byte += 1;
        }
        Some(v as u32)
    }

    /// DEFLATE packs multi-bit fields least-significant bit first.
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Some(v)
    }

    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }

    /// Consume `n` whole bytes from the current (aligned) position.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.byte.checked_add(n)?;
        let s = self.data.get(self.byte..end)?;
        self.byte = end;
        Some(s)
    }
}

struct Huffman {
    /// How many codes there are of each length, indexed by length.
    counts: [u16; MAX_BITS + 1],
    /// Symbols ordered by code length, then by symbol.
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            let l = l as usize;
            if l > MAX_BITS {
                return None;
            }
            counts[l] += 1;
        }
        counts[0] = 0;

        // Reject an over-subscribed set outright: it has no canonical
        // assignment, so anything we built from it would decode nonsense.
        // Incomplete sets are tolerated — a stream with a single distance
        // code produces one legitimately — and simply fail at decode time if
        // an unassigned code turns up.
        let mut left: i32 = 1;
        for len in 1..=MAX_BITS {
            left <<= 1;
            left -= counts[len] as i32;
            if left < 0 {
                return None;
            }
        }

        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let slot = offsets[l as usize] as usize;
            *symbols.get_mut(slot)? = sym as u16;
            offsets[l as usize] += 1;
        }

        Some(Huffman { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader) -> Option<u16> {
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAX_BITS {
            code |= br.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                let slot = index.checked_add(code - first)?;
                return self.symbols.get(slot as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

/// The literal/length and distance code lengths of a fixed-Huffman block, as
/// laid down in RFC 1951 §3.2.6.
fn fixed_tables() -> Option<(Huffman, Huffman)> {
    let mut lit = [0u8; 288];
    for (sym, l) in lit.iter_mut().enumerate() {
        *l = match sym {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    // Codes 30 and 31 never appear but are part of the tree.
    let dist = [5u8; 30];
    Some((Huffman::new(&lit)?, Huffman::new(&dist)?))
}

fn dynamic_tables(br: &mut BitReader) -> Option<(Huffman, Huffman)> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return None;
    }

    let mut clen = [0u8; 19];
    for &slot in CLEN_ORDER.iter().take(hclen) {
        clen[slot] = br.bits(3)? as u8;
    }
    let clen_tree = Huffman::new(&clen)?;

    // The two real trees are described by one run-length coded sequence, so
    // they are decoded together and split afterwards.
    let total = hlit + hdist;
    let mut lengths = vec![0u8; total];
    let mut i = 0;
    while i < total {
        let sym = clen_tree.decode(br)?;
        let (value, run) = match sym {
            0..=15 => (sym as u8, 1),
            16 => (*lengths.get(i.checked_sub(1)?)?, 3 + br.bits(2)? as usize),
            17 => (0, 3 + br.bits(3)? as usize),
            18 => (0, 11 + br.bits(7)? as usize),
            _ => return None,
        };
        let end = i.checked_add(run)?;
        if end > total {
            return None;
        }
        for slot in lengths.get_mut(i..end)? {
            *slot = value;
        }
        i = end;
    }

    // Without an end-of-block code the block could never terminate.
    if *lengths.get(256)? == 0 {
        return None;
    }
    let lit = Huffman::new(lengths.get(..hlit)?)?;
    let dist = Huffman::new(lengths.get(hlit..)?)?;
    Some((lit, dist))
}

fn stored_block(br: &mut BitReader, out: &mut Vec<u8>) -> Option<()> {
    br.align();
    let header = br.take(4)?;
    let len = u16::from_le_bytes([header[0], header[1]]);
    let nlen = u16::from_le_bytes([header[2], header[3]]);
    if len != !nlen {
        return None;
    }
    let len = len as usize;
    if out.len().checked_add(len)? > MAX_OUTPUT {
        return None;
    }
    out.extend_from_slice(br.take(len)?);
    Some(())
}

fn coded_block(br: &mut BitReader, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Option<()> {
    loop {
        let sym = lit.decode(br)? as usize;
        if sym < 256 {
            if out.len() >= MAX_OUTPUT {
                return None;
            }
            out.push(sym as u8);
            continue;
        }
        if sym == 256 {
            return Some(());
        }

        let idx = sym - 257;
        let len = *LENGTH_BASE.get(idx)? as usize + br.bits(*LENGTH_EXTRA.get(idx)?)? as usize;
        let dsym = dist.decode(br)? as usize;
        let back = *DIST_BASE.get(dsym)? as usize + br.bits(*DIST_EXTRA.get(dsym)?)? as usize;
        if back == 0 || back > out.len() {
            return None;
        }
        if out.len().checked_add(len)? > MAX_OUTPUT {
            return None;
        }
        // Copies may overlap themselves — that is how DEFLATE spells "repeat
        // this run" — so they have to go a byte at a time.
        let start = out.len() - back;
        for i in 0..len {
            let b = *out.get(start + i)?;
            out.push(b);
        }
    }
}

/// Inflate a bare DEFLATE stream.
pub fn raw(data: &[u8]) -> Option<Vec<u8>> {
    let mut br = BitReader::new(data);
    let mut out = Vec::new();
    loop {
        let last = br.bits(1)?;
        match br.bits(2)? {
            0 => stored_block(&mut br, &mut out)?,
            1 => {
                let (lit, dist) = fixed_tables()?;
                coded_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut br)?;
                coded_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

/// Inflate a zlib-wrapped stream, checking the two-byte header.
///
/// The trailing Adler-32 is not verified: a checksum mismatch would leave us
/// throwing away pixels we have already decoded correctly, and the framebuffer
/// is a more forgiving destination than an archive.
pub fn zlib(data: &[u8]) -> Option<Vec<u8>> {
    let cmf = *data.first()?;
    let flg = *data.get(1)?;
    if cmf & 0x0F != 8 {
        return None;
    }
    // A window larger than 32 KiB is not zlib, whatever the header claims.
    if cmf >> 4 > 7 {
        return None;
    }
    if (((cmf as u16) << 8) | flg as u16) % 31 != 0 {
        return None;
    }
    // A preset dictionary we do not have makes the stream undecodable.
    if flg & 0x20 != 0 {
        return None;
    }
    raw(data.get(2..)?)
}
