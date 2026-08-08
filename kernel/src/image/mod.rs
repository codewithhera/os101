//! Raster image decoding.
//!
//! Three formats are understood: BMP, because the shipped assets are BMPs;
//! PNG and baseline JPEG, because those are what the browser pulls off the
//! web. [`decode`] sniffs the header and picks the right one, so callers that
//! have fetched some bytes never have to care which they got.
//!
//! Everything below this module parses attacker-controlled input, so the
//! house rules are strict: no slicing without `get`, no length arithmetic
//! without `checked_*`, and a hostile file returns `None` rather than
//! panicking the kernel.

mod bmp;
mod inflate;
mod jpeg;
mod png;
mod selftest;

use alloc::boxed::Box;
use alloc::vec::Vec;
use spin::Mutex;

use crate::color::Color;

const HERA_BMP: &[u8] = include_bytes!("../../assets/hera.bmp");

pub struct Image {
    pub width: usize,
    pub height: usize,
    /// Row-major, top row first.
    pub pixels: Vec<Color>,
}

static HERA: Mutex<Option<&'static Image>> = Mutex::new(None);

/// Largest image we will decode. The kernel heap is 32 MiB and a finished
/// image costs four bytes a pixel on top of whatever the decoder needed to
/// get there, so a hostile or corrupt header must not be able to ask for more
/// than a slice of it.
const MAX_PIXELS: usize = 4 * 1024 * 1024;

/// Decode on first use, cache thereafter.
pub fn hera() -> Option<&'static Image> {
    let mut slot = HERA.lock();
    if slot.is_none() {
        // Leak the decoded image so the `'static` reference is genuinely
        // static. The previous version handed out a reference into the
        // Mutex's contents with a forged lifetime, which would dangle the
        // moment the slot was replaced or cleared.
        *slot = decode(HERA_BMP).map(|img| &*Box::leak(Box::new(img)));
    }
    *slot
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Bmp,
}

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Bmp => "BMP",
        }
    }
}

/// Identify a file from its first few bytes.
///
/// Content sniffing rather than trusting a `Content-Type` header is
/// deliberate: servers lie about image types constantly, and the decoders
/// below are the ones that have to survive being wrong.
fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&png::SIGNATURE) {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"BM") {
        Some(Format::Bmp)
    } else {
        None
    }
}

/// Decode any format the kernel understands.
pub fn decode(bytes: &[u8]) -> Option<Image> {
    match sniff(bytes)? {
        Format::Png => png::decode(bytes),
        Format::Jpeg => jpeg::decode(bytes),
        Format::Bmp => bmp::decode(bytes),
    }
}

/// The name of the format a byte slice looks like, for error messages.
///
/// A `Some` here says only that the header matched; [`decode`] can still fail
/// on the body.
pub fn format_name(bytes: &[u8]) -> Option<&'static str> {
    sniff(bytes).map(Format::name)
}

/// Wired into the boot sequence by `main`; the attribute keeps the build
/// quiet in the meantime.
#[allow(dead_code)]
pub fn selftest() -> crate::selftest::Report {
    selftest::run()
}
