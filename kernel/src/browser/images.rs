//! The pictures a page has been given.
//!
//! Layout needs a picture's natural size to give it a box, but the bytes
//! arrive long after the HTML does: the window layer renders the document,
//! asks it which pictures it wants ([`super::Page::image_sources`]), fetches
//! them one at a time and drops each result in here before laying the page out
//! again. This store is therefore the one piece of page state filled in from
//! outside, and layout has to cope with every entry being absent — the first
//! pass over a page always finds it empty.
//!
//! Failures are recorded rather than forgotten. A picture that will never
//! arrive should fall back to its alt text, and the only way layout can tell
//! "not yet" from "never" is to be told.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;

use crate::image::Image;

/// Most distinct pictures one page may ask for or keep.
///
/// Each one costs a round trip on a network stack polled from the same thread
/// as the redraw, and four bytes a pixel on a heap shared with everything else
/// the machine is doing. A page with a thousand tracking pixels on it does not
/// get to spend either.
pub(super) const MAX_PICTURES: usize = 256;

/// Largest side a picture's box may be asked for.
///
/// Nothing is allocated per pixel during layout, so this is not a memory
/// guard; it keeps a page whose author wrote `width="99999999"` from producing
/// a scrollbar that cannot be used and arithmetic that has lost its precision.
pub(super) const MAX_DIMENSION: f32 = 8192.0;

/// How far the alt text sits inside a placeholder's frame. Shared, because
/// layout decides how much of the text fits and the painter draws it.
pub(super) const PLACEHOLDER_INSET: f32 = 4.0;

enum Entry {
    /// Decoded and ready to blit.
    Ready(Arc<Image>),
    /// Fetched or decoded and failed. Worth remembering: it is the difference
    /// between a gap that will fill itself in and one that never will.
    Failed,
}

pub struct ImageStore {
    entries: BTreeMap<String, Entry>,
}

impl ImageStore {
    pub fn new() -> Self {
        ImageStore { entries: BTreeMap::new() }
    }

    /// A picture that arrived and decoded.
    ///
    /// `Arc` rather than `Rc` because the page lives inside a `Mutex` static
    /// and has to stay `Send`.
    pub fn insert(&mut self, src: &str, image: Arc<Image>) {
        self.record(src, Entry::Ready(image));
    }

    /// A picture that could not be fetched or decoded, so layout falls back to
    /// its alt text instead of leaving a hole.
    pub fn fail(&mut self, src: &str) {
        self.record(src, Entry::Failed);
    }

    pub fn get(&self, src: &str) -> Option<&Arc<Image>> {
        match self.entries.get(key(src))? {
            Entry::Ready(image) => Some(image),
            Entry::Failed => None,
        }
    }

    /// Natural size in pixels, if it is loaded.
    pub fn size(&self, src: &str) -> Option<(usize, usize)> {
        self.get(src).map(|image| (image.width, image.height))
    }

    /// True once [`Self::insert`] or [`Self::fail`] has been called for this
    /// src.
    pub fn known(&self, src: &str) -> bool {
        self.entries.contains_key(key(src))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn record(&mut self, src: &str, entry: Entry) {
        let key = key(src);
        // An `<img>` with no src names no picture, so there is nothing here to
        // remember and nothing layout could ever look up.
        if key.is_empty() {
            return;
        }
        // A src already in the store is replaced rather than refused: a reload
        // has to be able to overwrite what it found last time.
        if self.entries.len() >= MAX_PICTURES && !self.entries.contains_key(key) {
            return;
        }
        self.entries.insert(key.to_string(), entry);
    }
}

/// A src as the page wrote it, less any surrounding whitespace.
///
/// `src=" a.png "` names the same picture as `src="a.png"`, and the fetch and
/// the layout have to agree on which — they only ever meet through this key.
fn key(src: &str) -> &str {
    src.trim()
}

/// Read a `width` or `height` attribute, which HTML writes as a bare count of
/// pixels.
///
/// Trailing rubbish is ignored the way a browser ignores it, so `width="100px"`
/// is a hundred. A percentage is refused instead: it asks for a share of the
/// container, and reading `50%` as fifty pixels would be worse than ignoring
/// the attribute and using the picture's own size.
pub(super) fn dimension_attr(value: &str) -> Option<f32> {
    let text = value.trim();
    if text.contains('%') {
        return None;
    }

    let mut digits = 0usize;
    let mut total = 0u32;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            break;
        }
        digits += 1;
        // Saturating, because the attribute is a string off the internet and
        // may well be longer than a u32 can hold.
        total = total.saturating_mul(10).saturating_add((byte - b'0') as u32);
        if total as f32 >= MAX_DIMENSION {
            return Some(MAX_DIMENSION);
        }
    }

    if digits == 0 {
        None
    } else {
        Some(total as f32)
    }
}
