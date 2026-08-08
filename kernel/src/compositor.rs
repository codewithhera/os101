#![allow(dead_code)] // Scaffold for Phase 8 — intentionally unused today.
//! Compositor scaffold — Phase 7.
//!
//! A minimal Z-ordered list of `Layer` trait objects. Each layer knows how
//! to paint itself into the framebuffer's back buffer; the compositor walks
//! them bottom-to-top in order. A single `present()` then blits the result
//! to VRAM in one shot.
//!
//! This is deliberately small — just enough structure for Phase 8's window
//! manager to slot into. The existing shell + widgets don't route through
//! the compositor yet; they paint directly. The purpose here is to prove
//! the layering + z-order contract with a real example (a desktop
//! background layer) so Phase 8 can build on it rather than inventing it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::framebuffer::Rect;

pub trait Layer {
    /// Paint this layer into the back buffer.
    fn redraw(&self);
    /// Region this layer occupies — used later for dirty-rect culling.
    fn bounds(&self) -> Rect;
    fn visible(&self) -> bool { true }
}

pub struct Compositor {
    layers: Vec<Box<dyn Layer + Send + Sync>>,
}

impl Compositor {
    pub const fn new() -> Self {
        Self { layers: Vec::new() }
    }

    /// Insert a layer on top of everything already present.
    pub fn push(&mut self, layer: Box<dyn Layer + Send + Sync>) {
        self.layers.push(layer);
    }

    /// Paint every visible layer in z-order (bottom first).
    pub fn render(&self) {
        for layer in &self.layers {
            if layer.visible() {
                layer.redraw();
            }
        }
    }
}

// ── A sample layer: desktop background ──────────────────────────────────────
//
// Proves the scaffold works end-to-end without pulling in Phase 8 machinery.
// Draws a subtle dot grid across the text region — quiet enough not to
// distract from the shell but visible enough to see "something is layered
// underneath".

pub struct DotGridBackground {
    pub width: usize,
    pub height: usize,
    pub step: usize,
    pub intensity: u8,
}

impl Layer for DotGridBackground {
    fn redraw(&self) {
        let mut y = self.step;
        while y < self.height {
            let mut x = self.step;
            while x < self.width {
                crate::framebuffer::fill_rect(x, y, 1, 1, self.intensity);
                x += self.step;
            }
            y += self.step;
        }
    }

    fn bounds(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.width, h: self.height }
    }
}
