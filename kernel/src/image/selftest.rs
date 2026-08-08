//! Boot-time checks for the image decoders.
//!
//! Two fixtures carry most of the weight: `test16.png` and `test16.jpg` are
//! the same sixteen-pixel-square picture — red, green, blue and white
//! quadrants — in both formats, so a decoder that gets its rows, planes or
//! chroma the wrong way round shows up immediately. JPEG is lossy, so its
//! quadrants are sampled well inside the edges and compared with a tolerance.
//!
//! The rest is built here rather than shipped: [`build_png`] assembles PNGs
//! around scanlines the test chose, which is the only practical way to reach
//! the palette, alpha, sub-byte and 16-bit paths, and a hand-assembled
//! greyscale JPEG covers restart markers, which the fixture is too small to
//! contain (one MCU leaves nowhere to put one).
//!
//! The last group matters most. Every prefix and a spread of single-byte
//! corruptions of each fixture go back through the decoders; the check is that
//! each returns either `None` or an image whose buffer matches its own
//! dimensions. A panic here would not fail a check, it would take the kernel
//! down, so these run at boot for the same reason the network checksums do.

use alloc::vec;
use alloc::vec::Vec;

use crate::color::Color;
use crate::selftest::Report;

use super::{bmp, inflate, jpeg, png, Image};

const TEST_PNG: &[u8] = include_bytes!("../../assets/test16.png");
const TEST_JPG: &[u8] = include_bytes!("../../assets/test16.jpg");

/// Offsets of the IHDR fields, which the signature and chunk header pin to
/// fixed positions in every PNG there is.
const IHDR_WIDTH: usize = 16;
const IHDR_DEPTH: usize = 24;
const IHDR_COLOUR: usize = 25;
const IHDR_INTERLACE: usize = 28;

/// A 16x16 greyscale baseline JPEG of four flat 8x8 quadrants — 32, 96, 160
/// and 224 — with a restart interval of one MCU, so RST0 to RST2 all appear.
/// Every block is DC-only against a quantiser of 8, which makes the expected
/// output exact rather than approximate.
const GREY_JPG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0xFF,
    0xC0, 0x00, 0x0B, 0x08, 0x00, 0x10, 0x00, 0x10, 0x01, 0x01, 0x11, 0x00,
    0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02,
    0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xC4, 0x00,
    0x15, 0x10, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xDD, 0x00, 0x04,
    0x00, 0x01, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00,
    0xF1, 0xF7, 0xFF, 0xD0, 0xE7, 0xDF, 0xFF, 0xD1, 0xE8, 0x1F, 0xFF, 0xD2,
    0xF6, 0x07, 0xFF, 0xD9,
];

pub fn run() -> Report {
    let mut r = Report::new();

    sniffing(&mut r);
    png_fixture(&mut r);
    jpeg_fixture(&mut r);
    jpeg_restarts(&mut r);
    bmp_fixture(&mut r);
    deflate(&mut r);
    png_colour_types(&mut r);
    png_filters(&mut r);
    rejections(&mut r);
    hostile_input(&mut r);

    r
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn at(img: &Image, x: usize, y: usize) -> Color {
    img.pixels.get(y * img.width + x).copied().unwrap_or(Color::BLACK)
}

/// JPEG is lossy and the fixture's quadrant edges ring, so comparisons sample
/// the middle of a quadrant and allow a channel to drift.
fn near(a: Color, b: Color, tolerance: u8) -> bool {
    let close = |x: u8, y: u8| x.abs_diff(y) <= tolerance;
    close(a.r(), b.r()) && close(a.g(), b.g()) && close(a.b(), b.b())
}

fn quadrants(img: &Image, tolerance: u8) -> bool {
    near(at(img, 4, 4), Color::rgb(255, 0, 0), tolerance)
        && near(at(img, 12, 4), Color::rgb(0, 255, 0), tolerance)
        && near(at(img, 4, 12), Color::rgb(0, 0, 255), tolerance)
        && near(at(img, 12, 12), Color::WHITE, tolerance)
}

fn with_byte(bytes: &[u8], offset: usize, value: u8) -> Vec<u8> {
    let mut copy = bytes.to_vec();
    if let Some(slot) = copy.get_mut(offset) {
        *slot = value;
    }
    copy
}

/// Locate an `FF xx` marker, so the JPEG tests can rewrite a field without
/// hard-coding an offset into a file they did not create.
fn find_marker(bytes: &[u8], marker: u8) -> usize {
    bytes
        .windows(2)
        .position(|w| w[0] == 0xFF && w[1] == marker)
        .unwrap_or(bytes.len())
}

// ── Format sniffing ──────────────────────────────────────────────────────

fn sniffing(r: &mut Report) {
    r.check("sniff png", super::format_name(TEST_PNG) == Some("PNG"));
    r.check("sniff jpeg", super::format_name(TEST_JPG) == Some("JPEG"));
    r.check("sniff bmp", super::format_name(b"BM\0\0\0\0") == Some("BMP"));
    r.check("sniff unknown", super::format_name(b"GIF89a").is_none());
    r.check("sniff empty", super::format_name(&[]).is_none());
    r.check("sniff short", super::format_name(&[0xFF, 0xD8]).is_none());

    r.check(
        "decode dispatches to png",
        super::decode(TEST_PNG).map(|i| i.width) == Some(16),
    );
    r.check(
        "decode dispatches to jpeg",
        super::decode(TEST_JPG).map(|i| i.width) == Some(16),
    );
    r.check("decode rejects unknown", super::decode(b"GIF89a").is_none());

    // A decoder handed the wrong format must bail on the magic, not wander
    // into the body looking for something familiar.
    r.check("png decoder rejects jpeg", png::decode(TEST_JPG).is_none());
    r.check("jpeg decoder rejects png", jpeg::decode(TEST_PNG).is_none());
    r.check("bmp decoder rejects png", bmp::decode(TEST_PNG).is_none());
}

// ── The two fixtures ─────────────────────────────────────────────────────

fn png_fixture(r: &mut Report) {
    let Some(img) = png::decode(TEST_PNG) else {
        r.check("png fixture decodes", false);
        return;
    };
    r.check("png fixture decodes", true);
    r.check("png fixture width", img.width == 16);
    r.check("png fixture height", img.height == 16);
    r.check("png fixture pixel count", img.pixels.len() == 256);
    r.check("png top-left red", at(&img, 4, 4) == Color::rgb(255, 0, 0));
    r.check("png top-right green", at(&img, 12, 4) == Color::rgb(0, 255, 0));
    r.check("png bottom-left blue", at(&img, 4, 12) == Color::rgb(0, 0, 255));
    r.check("png bottom-right white", at(&img, 12, 12) == Color::WHITE);
    // The corners pin down row order: a vertically flipped decode would still
    // pass the quadrant checks if it also flipped the colours.
    r.check("png first pixel", at(&img, 0, 0) == Color::rgb(255, 0, 0));
    r.check("png last pixel", at(&img, 15, 15) == Color::WHITE);
    r.check("png quadrant boundary", at(&img, 8, 0) == Color::rgb(0, 255, 0));
}

fn jpeg_fixture(r: &mut Report) {
    let Some(img) = jpeg::decode(TEST_JPG) else {
        r.check("jpeg fixture decodes", false);
        return;
    };
    r.check("jpeg fixture decodes", true);
    r.check("jpeg fixture width", img.width == 16);
    r.check("jpeg fixture height", img.height == 16);
    r.check("jpeg fixture pixel count", img.pixels.len() == 256);
    r.check("jpeg top-left red", near(at(&img, 4, 4), Color::rgb(255, 0, 0), 24));
    r.check("jpeg top-right green", near(at(&img, 12, 4), Color::rgb(0, 255, 0), 24));
    r.check("jpeg bottom-left blue", near(at(&img, 4, 12), Color::rgb(0, 0, 255), 24));
    r.check("jpeg bottom-right white", near(at(&img, 12, 12), Color::WHITE, 24));
    r.check("jpeg quadrants", quadrants(&img, 24));

    // The two formats hold the same picture, so they must agree away from the
    // edges where JPEG's ringing lives.
    if let Some(reference) = png::decode(TEST_PNG) {
        let agree = [(4, 4), (11, 4), (4, 11), (11, 11)]
            .iter()
            .all(|&(x, y)| near(at(&img, x, y), at(&reference, x, y), 24));
        r.check("jpeg agrees with png", agree);
    }
}

fn jpeg_restarts(r: &mut Report) {
    let Some(img) = jpeg::decode(GREY_JPG) else {
        r.check("greyscale jpeg decodes", false);
        return;
    };
    r.check("greyscale jpeg decodes", true);
    r.check("greyscale jpeg size", img.width == 16 && img.height == 16);
    // Each quadrant is one MCU, and each MCU sits behind its own restart
    // marker, so a wrong value here means the marker was mishandled or the DC
    // predictor was not reset.
    r.check("restart mcu 0", at(&img, 4, 4) == Color::gray(32));
    r.check("restart mcu 1", at(&img, 12, 4) == Color::gray(96));
    r.check("restart mcu 2", at(&img, 4, 12) == Color::gray(160));
    r.check("restart mcu 3", at(&img, 12, 12) == Color::gray(224));
    r.check("greyscale is grey", img.pixels.iter().all(|c| c.is_gray()));
}

fn bmp_fixture(r: &mut Report) {
    let Some(img) = super::hera() else {
        r.check("hera bmp decodes", false);
        return;
    };
    r.check("hera bmp decodes", true);
    r.check("hera has pixels", img.width > 0 && img.height > 0);
    r.check("hera buffer matches size", img.width * img.height == img.pixels.len());
    // The cache hands out one leaked allocation; a second call that decoded
    // again would leak another every time.
    let cached = super::hera().map(|i| core::ptr::from_ref(i));
    r.check("hera is cached", cached == Some(core::ptr::from_ref(img)));

    // A hand-built 2x2 bottom-up 24bpp BMP, to check row order independently
    // of the shipped asset.
    let mut file = Vec::new();
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&[0; 8]);
    file.extend_from_slice(&54u32.to_le_bytes());
    file.extend_from_slice(&40u32.to_le_bytes());
    file.extend_from_slice(&2i32.to_le_bytes());
    file.extend_from_slice(&2i32.to_le_bytes());
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&24u16.to_le_bytes());
    file.extend_from_slice(&[0; 24]);
    // Bottom row first: blue, white; then top row: red, green. Stored as BGR,
    // and every row is padded out to a multiple of four bytes.
    file.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]);
    file.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]);
    match bmp::decode(&file) {
        Some(img) => {
            r.check("bmp top row", at(&img, 0, 0) == Color::rgb(255, 0, 0));
            r.check("bmp bottom-up order", at(&img, 0, 1) == Color::rgb(0, 0, 255));
            r.check("bmp bgr order", at(&img, 1, 0) == Color::rgb(0, 255, 0));
        }
        None => {
            r.check("bmp top row", false);
            r.check("bmp bottom-up order", false);
            r.check("bmp bgr order", false);
        }
    }
}

// ── DEFLATE ──────────────────────────────────────────────────────────────

/// The three sentences the dynamic-Huffman fixture below compresses, repeated
/// three times over.
const PANGRAMS: &[u8] = b"The quick brown fox jumps over the lazy dog. \
Pack my box with five dozen liquor jugs. \
How vexingly quick daft zebras jump! \
Sphinx of black quartz, judge my vow. ";

fn deflate(r: &mut Report) {
    // Stored: zlib at level 0 over "hello, deflate".
    const STORED: &[u8] = &[
        0x78, 0x01, 0x01, 0x0E, 0x00, 0xF1, 0xFF, 0x68, 0x65, 0x6C, 0x6C, 0x6F, 0x2C, 0x20, 0x64,
        0x65, 0x66, 0x6C, 0x61, 0x74, 0x65, 0x26, 0xAD, 0x05, 0x36,
    ];
    r.check("stored block", inflate::zlib(STORED).as_deref() == Some(&b"hello, deflate"[..]));

    // Fixed Huffman, literals only: twenty-four 'a's.
    const FIXED_RUN: &[u8] = &[0x4B, 0x4C, 0xC4, 0x0E, 0x00];
    r.check("fixed huffman run", inflate::raw(FIXED_RUN).as_deref() == Some(&[b'a'; 24][..]));

    // Fixed Huffman with a length/distance pair: "abc" six times.
    const FIXED_MATCH: &[u8] = &[0x4B, 0x4C, 0x4A, 0x4E, 0x44, 0x45, 0x00];
    let expected: Vec<u8> = b"abc".iter().copied().cycle().take(18).collect();
    r.check("fixed huffman back-reference", inflate::raw(FIXED_MATCH).as_deref() == Some(&expected[..]));

    // Dynamic Huffman: enough varied text that zlib judged a custom tree worth
    // the header it costs.
    const DYNAMIC: &[u8] = &[
        0xED, 0x8E, 0x4B, 0x12, 0xC2, 0x20, 0x10, 0x44, 0xAF, 0xD2, 0xEE, 0xAD, 0x9C, 0xC3, 0xA5,
        0x55, 0xE6, 0x02, 0x20, 0x03, 0x41, 0x09, 0x93, 0x10, 0x7E, 0xE1, 0xF4, 0x21, 0x96, 0x37,
        0x70, 0xEB, 0xFA, 0x75, 0xBF, 0xEE, 0x71, 0x22, 0xAC, 0xC9, 0x3E, 0xDF, 0x90, 0x81, 0x8B,
        0x87, 0xE6, 0x8A, 0x57, 0x9A, 0x97, 0x0D, 0x9C, 0x29, 0x20, 0x76, 0xEC, 0x44, 0xDB, 0xA1,
        0xD8, 0x0C, 0xB8, 0x8B, 0x9E, 0x9B, 0x77, 0xC8, 0x1E, 0x2A, 0x36, 0x4E, 0xD0, 0x36, 0x53,
        0x47, 0x8D, 0x3C, 0x9C, 0x5D, 0x13, 0x87, 0xDE, 0x35, 0xDB, 0x80, 0x1B, 0x17, 0x64, 0xAA,
        0xD6, 0x1B, 0xB7, 0x7F, 0xF5, 0x4A, 0xE8, 0x88, 0x46, 0x32, 0x88, 0xED, 0x33, 0x70, 0xC1,
        0x63, 0x99, 0xAC, 0xAF, 0x60, 0x0D, 0xE9, 0x4E, 0xF1, 0x9A, 0x44, 0x88, 0xED, 0xDA, 0xA9,
        0x32, 0x74, 0xCE, 0x64, 0x2E, 0x03, 0xC6, 0xFF, 0xC1, 0x1F, 0x0F, 0x1E,
    ];
    let mut pangrams = Vec::new();
    for _ in 0..3 {
        pangrams.extend_from_slice(PANGRAMS);
    }
    let dynamic = inflate::raw(DYNAMIC);
    r.check("dynamic huffman length", dynamic.as_ref().map(Vec::len) == Some(pangrams.len()));
    r.check("dynamic huffman content", dynamic.as_deref() == Some(&pangrams[..]));

    // A stored block long enough to need more than one 64 KiB block exercises
    // the loop that walks between them.
    let long: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    r.check("multi-block stored", inflate::zlib(&zlib_stored(&long)).as_deref() == Some(&long[..]));

    r.check("zlib rejects empty", inflate::zlib(&[]).is_none());
    r.check("zlib rejects one byte", inflate::zlib(&[0x78]).is_none());
    r.check("zlib rejects wrong method", inflate::zlib(&[0x79, 0x01, 0x03, 0x00]).is_none());
    r.check("zlib rejects bad check bits", inflate::zlib(&[0x78, 0x02, 0x03, 0x00]).is_none());
    r.check("zlib rejects preset dictionary", inflate::zlib(&[0x78, 0x20, 0x03, 0x00]).is_none());
    r.check("raw rejects empty", inflate::raw(&[]).is_none());
    r.check("raw rejects reserved block type", inflate::raw(&[0x07]).is_none());
    // LEN and NLEN must be complements.
    r.check("stored rejects bad nlen", inflate::raw(&[0x01, 0x05, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0]).is_none());
    r.check("stored rejects truncation", inflate::raw(&[0x01, 0x05, 0x00, 0xFA, 0xFF, b'a']).is_none());
    // A final block that never arrives.
    r.check("raw rejects unterminated", inflate::raw(&[0x00, 0x01, 0x00, 0xFE, 0xFF, b'a']).is_none());
}

// ── PNG construction ─────────────────────────────────────────────────────

struct PngSpec<'a> {
    width: u32,
    height: u32,
    depth: u8,
    colour: u8,
    palette: &'a [u8],
    transparency: &'a [u8],
    scanlines: &'a [u8],
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    // The decoder does not verify chunk CRCs, so there is nothing to compute.
    out.extend_from_slice(&[0, 0, 0, 0]);
}

/// Wrap bytes in a zlib stream of stored blocks: no compression, but real
/// structure, which also drags `inflate`'s stored path through every PNG test
/// below.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut rest = data;
    loop {
        let n = rest.len().min(0xFFFF);
        let last = n == rest.len();
        out.push(last as u8);
        out.extend_from_slice(&(n as u16).to_le_bytes());
        out.extend_from_slice(&(!(n as u16)).to_le_bytes());
        out.extend_from_slice(&rest[..n]);
        rest = &rest[n..];
        if last {
            break;
        }
    }
    // Adler-32, which the decoder ignores.
    out.extend_from_slice(&[0, 0, 0, 0]);
    out
}

fn build_png(spec: &PngSpec) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&png::SIGNATURE);

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&spec.width.to_be_bytes());
    ihdr.extend_from_slice(&spec.height.to_be_bytes());
    ihdr.extend_from_slice(&[spec.depth, spec.colour, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);

    if !spec.palette.is_empty() {
        chunk(&mut out, b"PLTE", spec.palette);
    }
    if !spec.transparency.is_empty() {
        chunk(&mut out, b"tRNS", spec.transparency);
    }
    chunk(&mut out, b"IDAT", &zlib_stored(spec.scanlines));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Decode a synthesised PNG and compare it against the colours it was built
/// from.
fn expect_png(r: &mut Report, name: &'static str, spec: &PngSpec, expected: &[Color]) {
    let ok = match png::decode(&build_png(spec)) {
        Some(img) => img.pixels == expected,
        None => false,
    };
    r.check(name, ok);
}

fn png_colour_types(r: &mut Report) {
    let grey = [0x00, 0x40, 0x80, 0xFF];
    expect_png(
        r,
        "png 8-bit grey",
        &PngSpec {
            width: 4,
            height: 1,
            depth: 8,
            colour: 0,
            palette: &[],
            transparency: &[],
            scanlines: &[0, grey[0], grey[1], grey[2], grey[3]],
        },
        &grey.map(Color::gray),
    );

    expect_png(
        r,
        "png 1-bit grey",
        &PngSpec {
            width: 8,
            height: 1,
            depth: 1,
            colour: 0,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0b1011_0001],
        },
        &[255, 0, 255, 255, 0, 0, 0, 255].map(Color::gray),
    );

    expect_png(
        r,
        "png 2-bit grey",
        &PngSpec {
            width: 4,
            height: 1,
            depth: 2,
            colour: 0,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0b00_01_10_11],
        },
        &[0, 85, 170, 255].map(Color::gray),
    );

    expect_png(
        r,
        "png 4-bit grey",
        &PngSpec {
            width: 4,
            height: 1,
            depth: 4,
            colour: 0,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0x05, 0xAF],
        },
        &[0, 85, 170, 255].map(Color::gray),
    );

    expect_png(
        r,
        "png 16-bit truecolour",
        &PngSpec {
            width: 1,
            height: 1,
            depth: 16,
            colour: 2,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC],
        },
        &[Color::rgb(0x12, 0x56, 0x9A)],
    );

    expect_png(
        r,
        "png grey+alpha over white",
        &PngSpec {
            width: 3,
            height: 1,
            depth: 8,
            colour: 4,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0x00, 0xFF, 0x00, 0x00, 0x00, 0x80],
        },
        &[Color::BLACK, Color::WHITE, Color::gray(127)],
    );

    expect_png(
        r,
        "png rgba over white",
        &PngSpec {
            width: 3,
            height: 1,
            depth: 8,
            colour: 6,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 128],
        },
        &[Color::rgb(255, 0, 0), Color::WHITE, Color::gray(127)],
    );

    expect_png(
        r,
        "png 16-bit grey+alpha",
        &PngSpec {
            width: 2,
            height: 1,
            depth: 16,
            colour: 4,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0x40, 0xFF, 0xFF, 0xFF, 0x40, 0x00, 0x00, 0x00],
        },
        &[Color::gray(0x40), Color::WHITE],
    );

    let palette = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    expect_png(
        r,
        "png palette",
        &PngSpec {
            width: 4,
            height: 1,
            depth: 8,
            colour: 3,
            palette: &palette,
            transparency: &[],
            scanlines: &[0, 0, 1, 2, 3],
        },
        &[
            Color::rgb(255, 0, 0),
            Color::rgb(0, 255, 0),
            Color::rgb(0, 0, 255),
            Color::WHITE,
        ],
    );

    expect_png(
        r,
        "png palette with trns",
        &PngSpec {
            width: 3,
            height: 1,
            depth: 8,
            colour: 3,
            palette: &palette,
            transparency: &[255, 0, 128],
            scanlines: &[0, 0, 1, 2],
        },
        &[Color::rgb(255, 0, 0), Color::WHITE, Color::rgb(127, 127, 255)],
    );

    // Four entries in one byte, most significant pair first.
    expect_png(
        r,
        "png 2-bit palette",
        &PngSpec {
            width: 4,
            height: 1,
            depth: 2,
            colour: 3,
            palette: &palette,
            transparency: &[],
            scanlines: &[0, 0b11_10_01_00],
        },
        &[
            Color::WHITE,
            Color::rgb(0, 0, 255),
            Color::rgb(0, 255, 0),
            Color::rgb(255, 0, 0),
        ],
    );

    // Several IDATs must be joined before inflating, not inflated one by one.
    let scanlines = [0u8, 1, 2, 3, 0, 4, 5, 6];
    let stream = zlib_stored(&scanlines);
    let mut split = Vec::new();
    split.extend_from_slice(&png::SIGNATURE);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut split, b"IHDR", &ihdr);
    let (head, tail) = stream.split_at(stream.len() / 2);
    chunk(&mut split, b"IDAT", head);
    chunk(&mut split, b"IDAT", tail);
    chunk(&mut split, b"IEND", &[]);
    r.check(
        "png split idat",
        png::decode(&split).map(|i| i.pixels) == Some(vec![Color::rgb(1, 2, 3), Color::rgb(4, 5, 6)]),
    );
}

// ── Scanline filters ─────────────────────────────────────────────────────

/// The Paeth predictor again, written from the specification rather than
/// borrowed from the decoder, so the two have to agree independently.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn apply_filter(raw: &[u8], stride: usize, height: usize, bpp: usize, filter: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let byte = |y: usize, i: usize| raw.get(y * stride + i).copied().unwrap_or(0);
    for y in 0..height {
        out.push(filter);
        for i in 0..stride {
            let x = byte(y, i);
            let a = if i >= bpp { byte(y, i - bpp) } else { 0 };
            let b = if y > 0 { byte(y - 1, i) } else { 0 };
            let c = if y > 0 && i >= bpp { byte(y - 1, i - bpp) } else { 0 };
            out.push(match filter {
                1 => x.wrapping_sub(a),
                2 => x.wrapping_sub(b),
                3 => x.wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                4 => x.wrapping_sub(paeth(a, b, c)),
                _ => x,
            });
        }
    }
    out
}

fn png_filters(r: &mut Report) {
    const WIDTH: usize = 4;
    const HEIGHT: usize = 3;
    const STRIDE: usize = WIDTH * 3;

    // Values chosen to wrap the byte arithmetic in both directions.
    let raw: Vec<u8> = (0..STRIDE * HEIGHT).map(|i| ((i * 37 + 11) % 256) as u8).collect();
    let expected: Vec<Color> = raw
        .chunks_exact(3)
        .map(|c| Color::rgb(c[0], c[1], c[2]))
        .collect();

    const NAMES: [&str; 5] = [
        "png filter none",
        "png filter sub",
        "png filter up",
        "png filter average",
        "png filter paeth",
    ];
    for (filter, name) in NAMES.into_iter().enumerate() {
        let scanlines = apply_filter(&raw, STRIDE, HEIGHT, 3, filter as u8);
        let spec = PngSpec {
            width: WIDTH as u32,
            height: HEIGHT as u32,
            depth: 8,
            colour: 2,
            palette: &[],
            transparency: &[],
            scanlines: &scanlines,
        };
        let ok = match png::decode(&build_png(&spec)) {
            Some(img) => img.pixels == expected,
            None => false,
        };
        r.check(name, ok);
    }
}

// ── Refusals ─────────────────────────────────────────────────────────────

fn rejections(r: &mut Report) {
    r.check("png rejects interlace", png::decode(&with_byte(TEST_PNG, IHDR_INTERLACE, 1)).is_none());
    r.check("png rejects bad colour type", png::decode(&with_byte(TEST_PNG, IHDR_COLOUR, 7)).is_none());
    r.check("png rejects bad depth", png::decode(&with_byte(TEST_PNG, IHDR_DEPTH, 3)).is_none());
    r.check("png rejects zero width", png::decode(&with_byte(TEST_PNG, IHDR_WIDTH + 3, 0)).is_none());
    r.check("png rejects broken signature", png::decode(&with_byte(TEST_PNG, 1, b'X')).is_none());
    r.check("png rejects empty", png::decode(&[]).is_none());

    // 2^24 by 2^8 pixels: well formed, and far past MAX_PIXELS.
    let mut huge = TEST_PNG.to_vec();
    if let Some(dimensions) = huge.get_mut(IHDR_WIDTH..IHDR_WIDTH + 8) {
        dimensions.copy_from_slice(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00]);
    }
    r.check("png rejects oversized header", png::decode(&huge).is_none());

    // Truncating the IDAT leaves a stream that inflates to less than the
    // scanlines need.
    let short = build_png(&PngSpec {
        width: 4,
        height: 4,
        depth: 8,
        colour: 2,
        palette: &[],
        transparency: &[],
        scanlines: &[0, 1, 2, 3],
    });
    r.check("png rejects short idat", png::decode(&short).is_none());

    r.check(
        "png rejects palette without plte",
        png::decode(&build_png(&PngSpec {
            width: 1,
            height: 1,
            depth: 8,
            colour: 3,
            palette: &[],
            transparency: &[],
            scanlines: &[0, 0],
        }))
        .is_none(),
    );

    r.check(
        "png rejects unknown filter",
        png::decode(&build_png(&PngSpec {
            width: 1,
            height: 1,
            depth: 8,
            colour: 0,
            palette: &[],
            transparency: &[],
            scanlines: &[9, 0],
        }))
        .is_none(),
    );

    let sof = find_marker(TEST_JPG, 0xC0);
    r.check("jpeg rejects progressive", jpeg::decode(&with_byte(TEST_JPG, sof + 1, 0xC2)).is_none());
    r.check("jpeg rejects arithmetic", jpeg::decode(&with_byte(TEST_JPG, sof + 1, 0xC9)).is_none());
    r.check("jpeg rejects lossless", jpeg::decode(&with_byte(TEST_JPG, sof + 1, 0xC3)).is_none());
    r.check("jpeg rejects 12-bit samples", jpeg::decode(&with_byte(TEST_JPG, sof + 4, 12)).is_none());
    r.check("jpeg rejects zero height", {
        let patched = with_byte(&with_byte(TEST_JPG, sof + 5, 0), sof + 6, 0);
        jpeg::decode(&patched).is_none()
    });
    r.check("jpeg rejects oversized frame", {
        let mut patched = TEST_JPG.to_vec();
        if let Some(dimensions) = patched.get_mut(sof + 5..sof + 9) {
            dimensions.copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        }
        jpeg::decode(&patched).is_none()
    });
    r.check("jpeg rejects empty", jpeg::decode(&[]).is_none());
    r.check("jpeg rejects header only", jpeg::decode(&[0xFF, 0xD8, 0xFF, 0xD9]).is_none());

    r.check("bmp rejects empty", bmp::decode(&[]).is_none());
    // Byte 28 of a BMP is the bit depth; only 24 and 32 are supported.
    r.check("bmp rejects 8bpp", bmp::decode(&with_byte(super::HERA_BMP, 28, 8)).is_none());
}

// ── Malformed input ──────────────────────────────────────────────────────

/// An image is self-consistent when its buffer is exactly as large as its
/// dimensions claim. Anything else means a decoder lied about what it built.
fn consistent(img: Option<Image>) -> bool {
    match img {
        None => true,
        Some(img) => img.width.checked_mul(img.height) == Some(img.pixels.len()),
    }
}

fn prefixes_survive(bytes: &[u8], step: usize) -> bool {
    let mut n = 0;
    while n < bytes.len() {
        match bytes.get(..n) {
            Some(prefix) if consistent(super::decode(prefix)) => n += step,
            _ => return false,
        }
    }
    true
}

fn corruptions_survive(bytes: &[u8], step: usize) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        let mut copy = bytes.to_vec();
        if let Some(slot) = copy.get_mut(i) {
            *slot ^= 0xFF;
        }
        if !consistent(super::decode(&copy)) {
            return false;
        }
        i += step;
    }
    true
}

fn hostile_input(r: &mut Report) {
    r.check("png prefixes survive", prefixes_survive(TEST_PNG, 1));
    r.check("jpeg prefixes survive", prefixes_survive(TEST_JPG, 1));
    r.check("greyscale jpeg prefixes survive", prefixes_survive(GREY_JPG, 1));
    // The BMP is a quarter of a megabyte, so it is sampled rather than walked.
    r.check("bmp prefixes survive", prefixes_survive(super::HERA_BMP, 977));

    r.check("png corruptions survive", corruptions_survive(TEST_PNG, 1));
    // A flipped byte in the JPEG's frame header can name an image of a
    // million pixels, and decoding those is what makes this sweep the
    // slowest thing here, so it steps rather than walks.
    r.check("jpeg corruptions survive", corruptions_survive(TEST_JPG, 7));
    r.check("greyscale jpeg corruptions survive", corruptions_survive(GREY_JPG, 1));

    // Headers with nothing behind them, which is where an unchecked length
    // would walk off the end.
    r.check("png header alone", png::decode(&png::SIGNATURE).is_none());
    r.check("jpeg soi alone", jpeg::decode(&[0xFF, 0xD8, 0xFF]).is_none());
    r.check("bmp magic alone", bmp::decode(b"BM").is_none());
    r.check("all-zero input", super::decode(&[0u8; 64]).is_none());
    r.check(
        "runs of 0xff",
        consistent(jpeg::decode(&[0xFF; 512])) && consistent(png::decode(&[0xFF; 512])),
    );
}
