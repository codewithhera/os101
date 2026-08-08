//! Framebuffer — double-buffered, dirty-rect-driven.
//!
//! Phase 7 of `TASKS.md`. All drawing (text, 2D primitives, mouse cursor,
//! widgets) goes into an off-screen **back buffer** sized to match VRAM. A
//! running dirty-rect tracks the bounding box of anything that has changed
//! since the last blit. Calling [`present()`] memcpys only that region into
//! the real framebuffer, so the screen never shows half-drawn content —
//! eliminating the flicker we had when writes went straight to VRAM.
//!
//! Every drawing entry point takes `impl Into<Color>`, so a bare `u8` still
//! works and means "that grey". [`write_px`] is the single place that knows
//! the hardware channel order (RGB vs BGR).

use alloc::vec;
use alloc::vec::Vec;
use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::fmt::{self, Write};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster, get_raster_width};
use spin::Mutex;

use crate::color::Color;
use crate::theme;

const LINE_HEIGHT: RasterHeight = RasterHeight::Size16;
const CHAR_WIDTH: usize = get_raster_width(FontWeight::Regular, LINE_HEIGHT);
const ROW_HEIGHT: usize = LINE_HEIGHT.val();

/// The font sizes the system can draw, in pixels: 16, 20, 24 and 32.
///
/// The console and the window chrome only ever use [`TextSize::Normal`]; the
/// larger faces exist so the browser can render headings at something other
/// than body size. This is a bitmap font, so these are the only sizes there
/// are — nothing in between, and nothing smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TextSize {
    Normal,
    Medium,
    Large,
    Huge,
}

impl TextSize {
    const fn raster(self) -> RasterHeight {
        match self {
            TextSize::Normal => RasterHeight::Size16,
            TextSize::Medium => RasterHeight::Size20,
            TextSize::Large => RasterHeight::Size24,
            TextSize::Huge => RasterHeight::Size32,
        }
    }

    /// The size whose line height is closest to `px`, for mapping a CSS
    /// `font-size` onto the faces that exist.
    pub fn nearest(px: f32) -> TextSize {
        // Midpoints between the four available heights.
        if px < 18.0 {
            TextSize::Normal
        } else if px < 22.0 {
            TextSize::Medium
        } else if px < 28.0 {
            TextSize::Large
        } else {
            TextSize::Huge
        }
    }

    /// Width of one character. The face is monospaced, so this is exact.
    pub const fn char_w(self) -> usize {
        get_raster_width(FontWeight::Regular, self.raster())
    }

    /// Height of one line.
    pub const fn row_h(self) -> usize {
        self.raster().val()
    }
}
const MARGIN: usize = 16;
/// Pixels reserved at the bottom of the screen for widgets + status bar.
pub const BOTTOM_RESERVED: usize = 112;

const MOUSE_WIDTH: usize = 12;
const MOUSE_HEIGHT: usize = 19;

// 0: Transparent, 1: Black outline, 2: White fill
const MOUSE_SPRITE: [[u8; MOUSE_WIDTH]; MOUSE_HEIGHT] = [
    [1,1,0,0,0,0,0,0,0,0,0,0],
    [1,2,1,0,0,0,0,0,0,0,0,0],
    [1,2,2,1,0,0,0,0,0,0,0,0],
    [1,2,2,2,1,0,0,0,0,0,0,0],
    [1,2,2,2,2,1,0,0,0,0,0,0],
    [1,2,2,2,2,2,1,0,0,0,0,0],
    [1,2,2,2,2,2,2,1,0,0,0,0],
    [1,2,2,2,2,2,2,2,1,0,0,0],
    [1,2,2,2,2,2,2,2,2,1,0,0],
    [1,2,2,2,2,2,2,2,2,2,1,0],
    [1,2,2,2,2,2,2,2,2,2,2,1],
    [1,2,2,2,2,2,2,1,1,1,1,1],
    [1,2,2,2,1,2,1,0,0,0,0,0],
    [1,2,2,1,0,1,2,1,0,0,0,0],
    [1,2,1,0,0,1,2,1,0,0,0,0],
    [1,1,0,0,0,0,1,2,1,0,0,0],
    [0,0,0,0,0,0,1,2,1,0,0,0],
    [0,0,0,0,0,0,0,1,1,0,0,0],
    [0,0,0,0,0,0,0,0,0,0,0,0],
];

/// A rectangle in screen-space, represented as half-open `[x..x+w, y..y+h)`.
#[derive(Copy, Clone, Debug)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Write one pixel's worth of bytes in the hardware's channel order.
///
/// This is the only function that knows whether VRAM is RGB or BGR. `bytes`
/// must be exactly one pixel wide (`bytes_per_pixel`); the pad/alpha byte of a
/// 4-byte pixel is forced opaque, because leaving it at 0 makes some QEMU/VGA
/// scanout paths composite the whole screen as transparent black.
#[inline]
fn write_px(bytes: &mut [u8], fmt: PixelFormat, c: Color) {
    match fmt {
        PixelFormat::Rgb => {
            bytes[0] = c.r();
            bytes[1] = c.g();
            bytes[2] = c.b();
        }
        PixelFormat::Bgr => {
            bytes[0] = c.b();
            bytes[1] = c.g();
            bytes[2] = c.r();
        }
        PixelFormat::U8 => {
            bytes[0] = c.luma();
            return;
        }
        _ => return,
    }
    if bytes.len() > 3 {
        bytes[3] = 255;
    }
}

#[inline]
fn read_px(bytes: &[u8], fmt: PixelFormat) -> Color {
    match fmt {
        PixelFormat::Rgb => Color::rgb(bytes[0], bytes[1], bytes[2]),
        PixelFormat::Bgr => Color::rgb(bytes[2], bytes[1], bytes[0]),
        PixelFormat::U8 => Color::gray(bytes[0]),
        _ => Color::BLACK,
    }
}

pub struct FramebufferWriter {
    /// Actual VRAM, at the physical VBE mode (`main::DISPLAY_WIDTH/HEIGHT`).
    /// Only written by `present()`, which nearest-neighbour doubles `back`
    /// into it — see `info`'s doc for why.
    front: &'static mut [u8],
    /// Off-screen shadow, at half the physical resolution. Every drawing op
    /// (and every widget's layout math) writes here.
    back: Vec<u8>,
    /// Physical geometry of `front`/VRAM, used only by `present()` to address
    /// it. Everything else uses `info` — the *virtual* canvas.
    phys: FrameBufferInfo,
    /// Geometry of `back`, exposed to the rest of the kernel as "the screen".
    /// This is deliberately half of `phys` in each dimension: the physical
    /// VBE mode is sized so QEMU's 1:1 guest-pixel-to-backing-pixel cocoa
    /// display produces a window of the right *physical* size on a 2x-scale
    /// Retina host, which means it has twice the pixel density a normal
    /// desktop does. Drawing fonts, icons and widgets — all tuned in plain
    /// pixel counts for a normal-density screen — straight into that many
    /// pixels would make everything look half its intended size. Instead
    /// every drawing call in this module targets this smaller canvas at
    /// normal density, and `present()` doubles each pixel into a crisp 2x2
    /// block on the way to VRAM: an exact integer upscale, so there is no
    /// blur, just bigger pixels.
    info: FrameBufferInfo,
    cursor_x: usize,
    cursor_y: usize,
    /// Console foreground/background, used by the `print!` text path.
    text_fg: Color,
    text_bg: Color,
    mouse_bg: [u8; MOUSE_WIDTH * MOUSE_HEIGHT * 4],
    mouse_last_pos: Option<(usize, usize)>,
    /// Union of every pixel written since the last `present()`. `None` means
    /// nothing has changed since we last blitted.
    dirty: Option<Rect>,
    /// Reused scratch row for `present()`'s horizontal 2x expansion, sized to
    /// the widest possible virtual row so the hot path never allocates.
    scratch_row: Vec<u8>,
}

impl FramebufferWriter {
    pub fn info(&self) -> &FrameBufferInfo {
        &self.info
    }

    pub fn new(front: &'static mut [u8], phys: FrameBufferInfo) -> Self {
        let bpp = phys.bytes_per_pixel;
        // See the `info` field doc: draw at half the physical resolution,
        // `present()` doubles it back up. `main::DISPLAY_WIDTH/HEIGHT` are
        // kept multiples of 8 for the VBE stride rule, so this is exact.
        let v_width = (phys.width / 2).max(1);
        let v_height = (phys.height / 2).max(1);
        let info = FrameBufferInfo {
            byte_len: v_width * v_height * bpp,
            width: v_width,
            height: v_height,
            pixel_format: phys.pixel_format,
            bytes_per_pixel: bpp,
            stride: v_width,
        };
        let back = vec![0u8; info.byte_len];
        let scratch_row = vec![0u8; v_width * 2 * bpp];
        let mut writer = Self {
            front,
            back,
            phys,
            info,
            cursor_x: MARGIN,
            cursor_y: MARGIN,
            text_fg: theme::CONSOLE_TEXT,
            text_bg: theme::CONSOLE_BG,
            mouse_bg: [0; MOUSE_WIDTH * MOUSE_HEIGHT * 4],
            mouse_last_pos: None,
            dirty: None,
            scratch_row,
        };
        let (w, h) = (writer.info.width, writer.info.height);
        writer.paint_region(0, h, theme::CONSOLE_BG);
        writer.mark_dirty_rect(0, 0, w, h);
        writer
    }

    /// Console colours used by `write_char` / the `print!` macros.
    pub fn set_text_color(&mut self, fg: Color, bg: Color) {
        self.text_fg = fg;
        self.text_bg = bg;
    }

    /// Paint whole scanlines `y0..y1` a flat colour, ignoring the dirty rect.
    /// Callers that need a redraw mark it themselves.
    fn paint_region(&mut self, y0: usize, y1: usize, color: Color) {
        let bpp = self.info.bytes_per_pixel;
        let fmt = self.info.pixel_format;
        let stride_bytes = self.info.stride * bpp;
        let width = self.info.width;
        let y1 = y1.min(self.info.height);
        if y0 >= y1 || bpp == 0 {
            return;
        }
        for y in y0..y1 {
            let row = y * stride_bytes;
            for x in 0..width {
                let off = row + x * bpp;
                write_px(&mut self.back[off..off + bpp], fmt, color);
            }
        }
    }

    // ── Dirty-rect tracking ────────────────────────────────────────────────

    fn mark_dirty_rect(&mut self, x: usize, y: usize, w: usize, h: usize) {
        if w == 0 || h == 0 { return; }
        let width = self.info.width;
        let height = self.info.height;
        let x0 = x.min(width);
        let y0 = y.min(height);
        let x1 = (x + w).min(width);
        let y1 = (y + h).min(height);
        if x0 >= x1 || y0 >= y1 { return; }

        let new = Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
        self.dirty = Some(match self.dirty {
            None => new,
            Some(r) => {
                let nx = r.x.min(new.x);
                let ny = r.y.min(new.y);
                let nx1 = (r.x + r.w).max(new.x + new.w);
                let ny1 = (r.y + r.h).max(new.y + new.h);
                Rect { x: nx, y: ny, w: nx1 - nx, h: ny1 - ny }
            }
        });
    }

    /// Copy the current dirty region from the back buffer to VRAM, doubling
    /// every pixel into a 2x2 block (see the `info` field doc). Nearest
    /// neighbour rather than a blend, so the result stays crisp instead of
    /// blurring the bitmap font and pixel art.
    pub fn present(&mut self) {
        let Some(r) = self.dirty.take() else { return; };
        let bpp = self.info.bytes_per_pixel;
        if bpp == 0 { return; }
        let v_stride = self.info.stride * bpp;
        let p_stride = self.phys.stride * bpp;
        let row_bytes = r.w * bpp;
        let Some(exp_full) = self.scratch_row.get_mut(..row_bytes * 2) else { return; };

        for y in r.y..(r.y + r.h) {
            let src_off = y * v_stride + r.x * bpp;
            let Some(src) = self.back.get(src_off..src_off + row_bytes) else { break; };
            for i in 0..r.w {
                let s = i * bpp;
                let d = i * 2 * bpp;
                exp_full[d..d + bpp].copy_from_slice(&src[s..s + bpp]);
                exp_full[d + bpp..d + 2 * bpp].copy_from_slice(&src[s..s + bpp]);
            }
            let dst_x = r.x * 2 * bpp;
            for py in [y * 2, y * 2 + 1] {
                let dst_off = py * p_stride + dst_x;
                if let Some(dst) = self.front.get_mut(dst_off..dst_off + exp_full.len()) {
                    dst.copy_from_slice(exp_full);
                }
            }
        }
    }

    // ── Low-level plot (back buffer only) ──────────────────────────────────

    /// Set a single pixel in the back buffer. Updates the dirty rect.
    pub fn plot(&mut self, x: usize, y: usize, color: impl Into<Color>) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }
        let bpp = self.info.bytes_per_pixel;
        let offset = (y * self.info.stride + x) * bpp;
        write_px(
            &mut self.back[offset..offset + bpp],
            self.info.pixel_format,
            color.into(),
        );
        self.mark_dirty_rect(x, y, 1, 1);
    }

    // ── 2D primitives ──────────────────────────────────────────────────────

    /// Filled axis-aligned rectangle.
    ///
    /// Hot path: this drives desktop / window / button backgrounds. The
    /// previous implementation called `plot()` per pixel — for a 1280×720
    /// desktop fill that's 921k bounds-checks plus 921k dirty-rect unions.
    /// Here we clip once, build one row, then `copy_within` it down the
    /// rectangle and mark the union dirty exactly once.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: impl Into<Color>) {
        let color = color.into();
        if w == 0 || h == 0 { return; }
        let width = self.info.width;
        let height = self.info.height;
        let x0 = x.min(width);
        let y0 = y.min(height);
        let x1 = x.saturating_add(w).min(width);
        let y1 = y.saturating_add(h).min(height);
        if x0 >= x1 || y0 >= y1 { return; }

        let bpp = self.info.bytes_per_pixel;
        let fmt = self.info.pixel_format;
        let stride = self.info.stride * bpp;
        let xs = x0 * bpp;
        let row_len = (x1 - x0) * bpp;
        if bpp == 0 { return; }

        // A grey in a packed format is the same byte in every channel, so the
        // whole row collapses to a memset.
        let byte_uniform = match fmt {
            PixelFormat::U8 => true,
            PixelFormat::Rgb | PixelFormat::Bgr => bpp == 3 && color.is_gray(),
            _ => false,
        };

        if byte_uniform {
            let v = if matches!(fmt, PixelFormat::U8) { color.luma() } else { color.r() };
            for yy in y0..y1 {
                let off = yy * stride + xs;
                self.back[off..off + row_len].fill(v);
            }
        } else if matches!(fmt, PixelFormat::Rgb | PixelFormat::Bgr) {
            // Build the top row one pixel at a time, then copy it down. Slice
            // copies hit the same memcpy fast paths that `fill` does.
            let row_off = y0 * stride + xs;
            {
                let row = &mut self.back[row_off..row_off + row_len];
                let mut k = 0;
                while k + bpp <= row.len() {
                    write_px(&mut row[k..k + bpp], fmt, color);
                    k += bpp;
                }
            }
            for yy in (y0 + 1)..y1 {
                let dst_off = yy * stride + xs;
                self.back.copy_within(row_off..row_off + row_len, dst_off);
            }
        } else {
            return;
        }

        self.mark_dirty_rect(x0, y0, x1 - x0, y1 - y0);
    }

    /// Vertical linear gradient from `top` to `bottom`. Used for the desktop
    /// wallpaper and title bars; each scanline is a flat `fill_rect`.
    pub fn fill_vgradient(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        top: Color,
        bottom: Color,
    ) {
        if w == 0 || h == 0 { return; }
        for row in 0..h {
            // Guard h == 1 so the divisor is never zero.
            let t = if h > 1 { (row * 255 / (h - 1)) as u8 } else { 0 };
            self.fill_rect(x, y + row, w, 1, top.lerp(bottom, t));
        }
    }

    /// Integer square root of a non-negative value; 0 for anything negative,
    /// which keeps the corner arithmetic total.
    fn isqrt_i(n: i32) -> i32 {
        if n <= 0 {
            return 0;
        }
        let mut x = n;
        let mut y = (x + 1) / 2;
        while y < x {
            x = y;
            y = (x + n / x) / 2;
        }
        x
    }

    /// How far in from the edge row `row` of a `h`-tall rounded rectangle
    /// starts, given the radius of the top corners and of the bottom ones.
    ///
    /// A corner is a quarter circle centred its radius in from each edge, so
    /// the inset is that radius minus the horizontal half-chord at this
    /// height. Integer throughout: the kernel has no hardware floating point
    /// on this path, and at the radii a UI uses the difference is invisible.
    ///
    /// The two radii are separate so a title bar can be rounded along its top
    /// and square where it meets the window body.
    fn corner_inset(row: usize, h: usize, top_r: usize, bottom_r: usize) -> usize {
        let (r, from_edge) = if row < top_r {
            (top_r, top_r - row)
        } else if row + bottom_r >= h && h >= 1 {
            (bottom_r, row + bottom_r + 1 - h)
        } else {
            return 0;
        };
        if r == 0 {
            return 0;
        }
        let dy = from_edge.min(r) as i32;
        let rr = r as i32;
        let half = Self::isqrt_i(rr * rr - dy * dy);
        (rr - half).max(0) as usize
    }

    /// Rounded-corner fill. A flat colour if `top` and `bottom` match, and a
    /// vertical gradient otherwise — the two cases share every bit of the
    /// corner arithmetic, so they share the implementation too.
    pub fn fill_round(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        top_radius: usize,
        bottom_radius: usize,
        top: Color,
        bottom: Color,
    ) {
        if w == 0 || h == 0 {
            return;
        }
        let cap = (w / 2).min(h / 2);
        let (top_r, bottom_r) = (top_radius.min(cap), bottom_radius.min(cap));
        for row in 0..h {
            let inset = Self::corner_inset(row, h, top_r, bottom_r);
            if inset * 2 >= w {
                continue;
            }
            let t = if h > 1 { (row * 255 / (h - 1)) as u8 } else { 0 };
            let colour = if top.0 == bottom.0 { top } else { top.lerp(bottom, t) };
            self.fill_rect(x + inset, y + row, w - inset * 2, 1, colour);
        }
    }

    /// One-pixel rounded outline, drawn as the difference between the filled
    /// shape and the same shape one pixel smaller.
    pub fn stroke_round(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        radius: usize,
        color: Color,
    ) {
        if w < 2 || h < 2 {
            return;
        }
        let r = radius.min(w / 2).min(h / 2);
        for row in 0..h {
            let inset = Self::corner_inset(row, h, r, r);
            if inset * 2 >= w {
                continue;
            }
            let left = x + inset;
            let span = w - inset * 2;
            let edge_row = row == 0 || row + 1 == h;
            // Inside the straight sides only the two end pixels are on the
            // outline; across the curved part the inset changes row to row, so
            // fill the whole difference to avoid leaving gaps in the arc.
            let previous = if row == 0 { usize::MAX } else { Self::corner_inset(row - 1, h, r, r) };
            let next = if row + 1 == h { usize::MAX } else { Self::corner_inset(row + 1, h, r, r) };
            let step = previous.min(next);
            let thickness = if edge_row {
                span
            } else if step != usize::MAX && step < inset {
                (inset - step + 1).min(span)
            } else {
                1
            };
            self.fill_rect(left, y + row, thickness, 1, color);
            self.fill_rect(left + span - thickness, y + row, thickness, 1, color);
        }
    }

    /// Copy a rectangle of pixels into the back buffer.
    ///
    /// `src` is row-major with `src_w` pixels per row. Only the top-left
    /// `w`x`h` region is drawn, clipped to the screen. This exists so image
    /// rendering does not go through one `fill_rect` call per pixel.
    pub fn blit(
        &mut self,
        x: usize,
        y: usize,
        w: usize,
        h: usize,
        src: &[Color],
        src_w: usize,
    ) {
        if w == 0 || h == 0 || src_w == 0 { return; }
        let width = self.info.width;
        let height = self.info.height;
        let x1 = x.saturating_add(w).min(width);
        let y1 = y.saturating_add(h).min(height);
        if x >= x1 || y >= y1 { return; }

        let bpp = self.info.bytes_per_pixel;
        if bpp == 0 { return; }
        let fmt = self.info.pixel_format;
        let stride = self.info.stride * bpp;

        for row in 0..(y1 - y) {
            let src_start = row * src_w;
            let Some(src_row) = src.get(src_start..src_start + (x1 - x)) else {
                break;
            };
            let dst_off = (y + row) * stride + x * bpp;
            let dst_row = &mut self.back[dst_off..dst_off + (x1 - x) * bpp];
            for (i, px) in src_row.iter().enumerate() {
                write_px(&mut dst_row[i * bpp..(i + 1) * bpp], fmt, *px);
            }
        }
        self.mark_dirty_rect(x, y, x1 - x, y1 - y);
    }

    /// 1-pixel outlined axis-aligned rectangle. Implemented as four thin
    /// `fill_rect` strips so the fast row-fill path handles every edge.
    pub fn stroke_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: impl Into<Color>) {
        if w == 0 || h == 0 { return; }
        let color = color.into();
        self.fill_rect(x, y, w, 1, color);
        self.fill_rect(x, y + h - 1, w, 1, color);
        self.fill_rect(x, y, 1, h, color);
        self.fill_rect(x + w - 1, y, 1, h, color);
    }

    /// Bresenham line between two signed endpoints (clipped to the screen).
    pub fn draw_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: impl Into<Color>) {
        let color = color.into();
        let mut x = x0;
        let mut y = y0;
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.plot_i(x, y, color);
            if x == x1 && y == y1 { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; x += sx; }
            if e2 <= dx { err += dx; y += sy; }
        }
    }

    /// Midpoint-circle outline.
    pub fn draw_circle(&mut self, cx: i32, cy: i32, r: i32, color: impl Into<Color>) {
        if r <= 0 { return; }
        let color = color.into();
        let mut x = r;
        let mut y = 0;
        let mut err = 1 - r;
        while x >= y {
            self.plot_i(cx + x, cy + y, color);
            self.plot_i(cx - x, cy + y, color);
            self.plot_i(cx + x, cy - y, color);
            self.plot_i(cx - x, cy - y, color);
            self.plot_i(cx + y, cy + x, color);
            self.plot_i(cx - y, cy + x, color);
            self.plot_i(cx + y, cy - x, color);
            self.plot_i(cx - y, cy - x, color);
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }

    /// Filled midpoint circle — horizontal spans.
    pub fn fill_circle(&mut self, cx: i32, cy: i32, r: i32, color: impl Into<Color>) {
        if r <= 0 { return; }
        let color = color.into();
        let mut x = r;
        let mut y = 0;
        let mut err = 1 - r;
        while x >= y {
            self.hline(cx - x, cx + x, cy + y, color);
            self.hline(cx - x, cx + x, cy - y, color);
            self.hline(cx - y, cx + y, cy + x, color);
            self.hline(cx - y, cx + y, cy - x, color);
            y += 1;
            if err < 0 {
                err += 2 * y + 1;
            } else {
                x -= 1;
                err += 2 * (y - x) + 1;
            }
        }
    }

    fn hline(&mut self, x0: i32, x1: i32, y: i32, color: Color) {
        if y < 0 { return; }
        let a = x0.min(x1).max(0) as usize;
        let b = x0.max(x1).max(0) as usize;
        // `fill_rect` does its own right-edge clipping; convert inclusive
        // span [a..=b] to half-open width (b - a + 1).
        self.fill_rect(a, y as usize, b - a + 1, 1, color);
    }

    fn plot_i(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 { return; }
        self.plot(x as usize, y as usize, color);
    }

    // ── Text / shell rendering (back buffer) ───────────────────────────────

    fn newline(&mut self) {
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        self.cursor_x = MARGIN;
        self.cursor_y += ROW_HEIGHT;
        let text_bottom = self.info.height.saturating_sub(BOTTOM_RESERVED);
        if self.cursor_y + ROW_HEIGHT > text_bottom {
            self.scroll_up();
        }

        if let Some((x, y)) = m_pos { self.draw_mouse_cursor(x, y); }
    }

    fn backspace(&mut self) {
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        if self.cursor_x >= MARGIN + CHAR_WIDTH {
            self.cursor_x -= CHAR_WIDTH;
            let bg = self.text_bg;
            self.fill_rect(self.cursor_x, self.cursor_y, CHAR_WIDTH, ROW_HEIGHT, bg);
        }

        if let Some((x, y)) = m_pos { self.draw_mouse_cursor(x, y); }
    }

    fn clear_screen(&mut self) {
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        let text_bottom = self.info.height.saturating_sub(BOTTOM_RESERVED);
        // Only blank the scrollable text region — widgets + status bar stay.
        let bg = self.text_bg;
        self.paint_region(0, text_bottom, bg);
        self.mark_dirty_rect(0, 0, self.info.width, text_bottom);

        self.cursor_x = MARGIN;
        self.cursor_y = MARGIN;

        if let Some((x, y)) = m_pos { self.draw_mouse_cursor(x, y); }
    }

    fn render_cursor(&mut self, visible: bool) {
        let color = if visible { theme::CONSOLE_CURSOR } else { self.text_bg };
        for row in (ROW_HEIGHT - 2)..ROW_HEIGHT {
            for col in 0..CHAR_WIDTH {
                self.plot(self.cursor_x + col, self.cursor_y + row, color);
            }
        }
    }

    fn draw_mouse_cursor(&mut self, x: usize, y: usize) {
        // Stationary cursor: nothing to do. The main loop calls
        // `invalidate_mouse_cache()` before any full repaint, which clears
        // `mouse_last_pos` — so once the back buffer changes underneath us,
        // this guard correctly forces a fresh draw on the next call.
        if self.mouse_last_pos == Some((x, y)) {
            return;
        }
        // Erase old cursor first (restores saved background from back buffer).
        self.erase_mouse_cursor();

        // Save new background from back buffer.
        let bpp = self.info.bytes_per_pixel;
        for row in 0..MOUSE_HEIGHT {
            for col in 0..MOUSE_WIDTH {
                let px = x + col;
                let py = y + row;
                if px < self.info.width && py < self.info.height {
                    let offset = (py * self.info.stride + px) * bpp;
                    for b in 0..bpp {
                        self.mouse_bg[(row * MOUSE_WIDTH + col) * bpp + b] =
                            self.back[offset + b];
                    }
                }
            }
        }
        self.mouse_last_pos = Some((x, y));

        for row in 0..MOUSE_HEIGHT {
            for col in 0..MOUSE_WIDTH {
                match MOUSE_SPRITE[row][col] {
                    1 => self.plot(x + col, y + row, Color::BLACK),
                    2 => self.plot(x + col, y + row, Color::WHITE),
                    _ => {}
                }
            }
        }
        self.mark_dirty_rect(x, y, MOUSE_WIDTH, MOUSE_HEIGHT);
    }

    fn erase_mouse_cursor(&mut self) {
        if let Some((x, y)) = self.mouse_last_pos.take() {
            let bpp = self.info.bytes_per_pixel;
            for row in 0..MOUSE_HEIGHT {
                for col in 0..MOUSE_WIDTH {
                    let px = x + col;
                    let py = y + row;
                    if px < self.info.width && py < self.info.height {
                        let offset = (py * self.info.stride + px) * bpp;
                        for b in 0..bpp {
                            self.back[offset + b] =
                                self.mouse_bg[(row * MOUSE_WIDTH + col) * bpp + b];
                        }
                    }
                }
            }
            self.mark_dirty_rect(x, y, MOUSE_WIDTH, MOUSE_HEIGHT);
        }
    }

    pub fn draw_button(&mut self, x: usize, y: usize, w: usize, h: usize,
                       label: &str, bg: impl Into<Color>, fg: impl Into<Color>) {
        let (bg, fg) = (bg.into(), fg.into());
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        self.fill_rect(x, y, w, h, bg);
        self.stroke_rect(x, y, w, h, theme::BUTTON_BORDER);

        let label_w = label.chars().count() * CHAR_WIDTH;
        let mut cx = x + (w.saturating_sub(label_w)) / 2;
        let cy = y + (h.saturating_sub(ROW_HEIGHT)) / 2;
        for c in label.chars() {
            if let Some(raster) = get_raster(c, FontWeight::Regular, LINE_HEIGHT) {
                for (row, line) in raster.raster().iter().enumerate() {
                    for (col, &alpha) in line.iter().enumerate() {
                        if alpha > 0 {
                            self.plot(cx + col, cy + row, fg.over(bg, alpha));
                        }
                    }
                }
            }
            cx += CHAR_WIDTH;
        }

        if let Some((mx, my)) = m_pos { self.draw_mouse_cursor(mx, my); }
    }

    pub fn draw_status_bar(&mut self, text: &str) {
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        let (old_x, old_y) = (self.cursor_x, self.cursor_y);

        self.cursor_x = MARGIN;
        self.cursor_y = self.info.height - ROW_HEIGHT - MARGIN;

        let (bg, fg) = (theme::STATUS_BG, theme::STATUS_TEXT);
        self.fill_rect(MARGIN, self.cursor_y, self.info.width - 2 * MARGIN, ROW_HEIGHT, bg);

        for c in text.chars() {
            let char_raster = get_raster(c, FontWeight::Regular, LINE_HEIGHT).expect("font error");
            for (row, row_data) in char_raster.raster().iter().enumerate() {
                for (col, &alpha) in row_data.iter().enumerate() {
                    self.plot(self.cursor_x + col, self.cursor_y + row, fg.over(bg, alpha));
                }
            }
            self.cursor_x += CHAR_WIDTH;
        }

        self.cursor_x = old_x;
        self.cursor_y = old_y;

        if let Some((x, y)) = m_pos { self.draw_mouse_cursor(x, y); }
    }

    fn scroll_up(&mut self) {
        let stride_bytes = self.info.stride * self.info.bytes_per_pixel;
        let row_bytes = ROW_HEIGHT * stride_bytes;
        let text_bottom = self.info.height.saturating_sub(BOTTOM_RESERVED);
        let text_bottom_bytes = text_bottom * stride_bytes;

        if text_bottom_bytes > row_bytes {
            self.back.copy_within(row_bytes..text_bottom_bytes, 0);
            let bg = self.text_bg;
            self.paint_region(text_bottom - ROW_HEIGHT, text_bottom, bg);
            self.mark_dirty_rect(0, 0, self.info.width, text_bottom);
        }
        self.cursor_y -= ROW_HEIGHT;
    }

    fn write_char(&mut self, c: char) {
        let m_pos = self.mouse_last_pos;
        if m_pos.is_some() { self.erase_mouse_cursor(); }

        match c {
            '\n' => self.newline(),
            '\r' => self.cursor_x = MARGIN,
            '\x08' => self.backspace(),
            c => {
                if self.cursor_x + CHAR_WIDTH > self.info.width - MARGIN {
                    self.newline();
                }
                let raster = get_raster(c, FontWeight::Regular, LINE_HEIGHT)
                    .unwrap_or_else(|| {
                        get_raster('?', FontWeight::Regular, LINE_HEIGHT)
                            .expect("font must contain '?'")
                    });
                let (fg, bg) = (self.text_fg, self.text_bg);
                for (row, line) in raster.raster().iter().enumerate() {
                    for (col, &alpha) in line.iter().enumerate() {
                        // The font raster is coverage, not colour: blend the
                        // text colour over the background by that coverage.
                        self.plot(self.cursor_x + col, self.cursor_y + row, fg.over(bg, alpha));
                    }
                }
                self.cursor_x += CHAR_WIDTH;
            }
        }

        if let Some((x, y)) = m_pos { self.draw_mouse_cursor(x, y); }
    }
}

impl Write for FramebufferWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

pub static WRITER: Mutex<Option<FramebufferWriter>> = Mutex::new(None);

/// Where VRAM is mapped: `(address, byte length, physical width, physical
/// height)`. This is the real VBE mode, twice `screen_size()` in each
/// dimension — see `FramebufferWriter`'s `info` field doc.
pub fn info() -> Option<(u64, usize, usize, usize)> {
    let writer = WRITER.lock();
    let w = writer.as_ref()?;
    Some((
        w.front.as_ptr() as u64,
        w.front.len(),
        w.phys.width,
        w.phys.height,
    ))
}

/// Install a framebuffer writer. Must be called AFTER the heap allocator is
/// ready, because the back buffer is allocated on the heap.
pub fn init(info: FrameBufferInfo, buffer: &'static mut [u8]) {
    let bpp = info.bytes_per_pixel;
    let fmt = info.pixel_format;
    let len = buffer.len();
    if bpp > 0 {
        let mut i = 0;
        while i + bpp <= len {
            write_px(&mut buffer[i..i + bpp], fmt, theme::CONSOLE_BG);
            i += bpp;
        }
        for byte in &mut buffer[i..] {
            *byte = 0;
        }
    }
    *WRITER.lock() = Some(FramebufferWriter::new(buffer, info));
}

/// Mark the full virtual screen dirty so the next [`present`] copies every row
/// to VRAM (used after the first paint so the visible scanout cannot lag a
/// partial dirty union).
pub fn mark_entire_dirty() {
    if let Some(w) = WRITER.lock().as_mut() {
        let width = w.info.width;
        let height = w.info.height;
        w.mark_dirty_rect(0, 0, width, height);
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        $crate::framebuffer::_print(format_args!($($arg)*));
        $crate::serial::_print(format_args!($($arg)*));
    });
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    if let Some(writer) = WRITER.lock().as_mut() {
        let _ = writer.write_fmt(args);
    }
}

pub fn clear_screen() {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.clear_screen();
    }
}

pub fn set_cursor_visible(visible: bool) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.render_cursor(visible);
    }
}

pub fn update_mouse_cursor(x: usize, y: usize) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.draw_mouse_cursor(x, y);
    }
}

/// Invalidate cached mouse background without restoring it.
///
/// Use this right before a full-scene redraw that will repaint every pixel
/// anyway. It prevents stale cursor background data from being written back
/// on the next cursor move.
pub fn invalidate_mouse_cache() {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.mouse_last_pos = None;
    }
}

pub fn update_status_bar(text: &str) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.draw_status_bar(text);
    }
}

pub fn draw_button(x: usize, y: usize, w: usize, h: usize,
                   label: &str, bg: impl Into<Color>, fg: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.draw_button(x, y, w, h, label, bg, fg);
    }
}

/// Console colours for subsequent `print!` output.
pub fn set_text_color(fg: Color, bg: Color) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.set_text_color(fg, bg);
    }
}

/// Run `f` with a temporary console colour, then restore the previous one.
/// Used for `[ OK ]` / `[WARN]` style boot lines.
pub fn with_text_color<R>(fg: Color, f: impl FnOnce() -> R) -> R {
    let previous = WRITER.lock().as_ref().map(|w| (w.text_fg, w.text_bg));
    if let Some((_, bg)) = previous {
        set_text_color(fg, bg);
    }
    let out = f();
    if let Some((old_fg, old_bg)) = previous {
        set_text_color(old_fg, old_bg);
    }
    out
}

pub fn screen_size() -> Option<(usize, usize)> {
    WRITER.lock().as_ref().map(|w| (w.info.width, w.info.height))
}

/// Blit the dirty region from the back buffer to VRAM. Call after each
/// burst of drawing ops (end of event-loop iteration).
pub fn present() {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.present();
    }
}

// ── Public 2D primitives (wrap the writer's methods) ───────────────────────

pub fn fill_rect(x: usize, y: usize, w: usize, h: usize, color: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.fill_rect(x, y, w, h, color); }
}
pub fn stroke_rect(x: usize, y: usize, w: usize, h: usize, color: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.stroke_rect(x, y, w, h, color); }
}
pub fn draw_line(x0: i32, y0: i32, x1: i32, y1: i32, color: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.draw_line(x0, y0, x1, y1, color); }
}
pub fn draw_circle(cx: i32, cy: i32, r: i32, color: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.draw_circle(cx, cy, r, color); }
}
pub fn fill_circle(cx: i32, cy: i32, r: i32, color: impl Into<Color>) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.fill_circle(cx, cy, r, color); }
}
/// Vertical gradient fill — the desktop wallpaper and title bars use this.
pub fn fill_vgradient(x: usize, y: usize, w: usize, h: usize, top: Color, bottom: Color) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.fill_vgradient(x, y, w, h, top, bottom); }
}
/// Rounded-corner fill in a single colour.
pub fn fill_round_rect(x: usize, y: usize, w: usize, h: usize, radius: usize, color: Color) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.fill_round(x, y, w, h, radius, radius, color, color);
    }
}
/// Rounded-corner fill shading from `top` to `bottom`.
pub fn fill_round_gradient(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    radius: usize,
    top: Color,
    bottom: Color,
) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.fill_round(x, y, w, h, radius, radius, top, bottom);
    }
}
/// Rounded along the top edge only, square along the bottom — a title bar, a
/// tab, anything that sits flush against what is below it.
pub fn fill_top_round_gradient(
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    radius: usize,
    top: Color,
    bottom: Color,
) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.fill_round(x, y, w, h, radius, 0, top, bottom);
    }
}
/// One-pixel rounded outline.
pub fn stroke_round_rect(x: usize, y: usize, w: usize, h: usize, radius: usize, color: Color) {
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.stroke_round(x, y, w, h, radius, color);
    }
}
/// Copy a rectangle of pixels (row-major, `src_w` per row) to the screen.
pub fn blit(x: usize, y: usize, w: usize, h: usize, src: &[Color], src_w: usize) {
    if let Some(writer) = WRITER.lock().as_mut() { writer.blit(x, y, w, h, src, src_w); }
}

/// Draw a picture scaled into `dst_w` × `dst_h`, clipped to a scrolling view.
///
/// `dst_y` is signed and `clip` is a half-open `[top, bottom)` band of screen
/// rows, for the same reason [`draw_text_styled`] takes them: a picture
/// halfway off the top of a scrolling page has to be drawn in part rather than
/// dropped. `max_x` stops it from spilling past the right-hand edge of the
/// view it belongs to.
///
/// Sampling is nearest-neighbour. Page images arrive close to the size they
/// are drawn at — the proxy is asked for the display size — so the extra cost
/// of interpolating would buy very little here; the wallpaper, which is
/// enlarged enormously, does its own bilinear pass instead.
pub fn blit_scaled(
    dst_x: usize,
    dst_y: isize,
    dst_w: usize,
    dst_h: usize,
    src: &[Color],
    src_w: usize,
    src_h: usize,
    max_x: usize,
    clip: Option<(usize, usize)>,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    if src.len() < src_w * src_h {
        return;
    }
    let mut guard = WRITER.lock();
    let Some(writer) = guard.as_mut() else { return };

    let screen_w = writer.info.width;
    let screen_h = writer.info.height;
    let bpp = writer.info.bytes_per_pixel;
    if bpp == 0 {
        return;
    }
    let fmt = writer.info.pixel_format;
    let stride = writer.info.stride * bpp;

    let (clip_top, clip_bottom) = clip.unwrap_or((0, screen_h));
    let top = dst_y.max(clip_top as isize).max(0);
    let bottom = (dst_y + dst_h as isize).min(clip_bottom as isize).min(screen_h as isize);
    if top >= bottom || dst_x >= screen_w {
        return;
    }
    let right = (dst_x + dst_w).min(max_x).min(screen_w);
    if dst_x >= right {
        return;
    }

    // 16.16 fixed point, so a scale factor never needs a divide per pixel.
    let step_x = ((src_w as u64) << 16) / dst_w as u64;
    let step_y = ((src_h as u64) << 16) / dst_h as u64;

    for row in top..bottom {
        let sy = (((row - dst_y) as u64 * step_y) >> 16) as usize;
        let sy = sy.min(src_h - 1);
        let src_row = sy * src_w;
        let dst_off = row as usize * stride + dst_x * bpp;
        let span = right - dst_x;
        let Some(dst_row) = writer.back.get_mut(dst_off..dst_off + span * bpp) else {
            break;
        };
        for i in 0..span {
            let sx = ((i as u64 * step_x) >> 16) as usize;
            let px = src[src_row + sx.min(src_w - 1)];
            write_px(&mut dst_row[i * bpp..(i + 1) * bpp], fmt, px);
        }
    }

    writer.mark_dirty_rect(dst_x, top as usize, right - dst_x, (bottom - top) as usize);
}

/// Render a single line of text into the back buffer with alpha-blended
/// glyphs against `bg`, clipping to `max_w` pixels.
///
/// This is the GUI text path. The naive version called `framebuffer::fill_rect`
/// once per non-zero glyph pixel — that's a Mutex acquisition + a Rect-union
/// for every dot in the font, and modern GUI screens can render 5–10 thousand
/// of them per frame. Here we lock once, write bytes directly, and union the
/// bounding box one final time.
pub fn draw_text_blended(
    x: usize,
    y: usize,
    s: &str,
    fg: impl Into<Color>,
    bg: impl Into<Color>,
    max_w: usize,
) {
    draw_text_styled(x, y as isize, s, fg, bg, max_w, TextSize::Normal, false, None);
}

/// Draw text without clearing the character cell — glyphs blend over whatever
/// is already in the back buffer. Used for desktop labels that should sit on
/// the wallpaper with no chip behind them.
pub fn draw_text_transparent(x: usize, y: usize, s: &str, fg: impl Into<Color>, max_w: usize) {
    let fg = fg.into();
    let weight = FontWeight::Regular;
    let raster_height = TextSize::Normal.raster();
    let char_width = TextSize::Normal.char_w();
    let row_height = TextSize::Normal.row_h();
    if max_w < char_width {
        return;
    }
    let max_chars = max_w / char_width;
    let mut guard = WRITER.lock();
    let Some(w) = guard.as_mut() else {
        return;
    };

    let bpp = w.info.bytes_per_pixel;
    let fmt = w.info.pixel_format;
    let stride_bytes = w.info.stride * bpp;
    let width = w.info.width;
    let height = w.info.height;
    let right_clip = x.saturating_add(max_w);

    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_xx = 0usize;
    let mut max_yy = 0usize;

    let mut cx = x;
    for c in s.chars().take(max_chars) {
        if cx >= right_clip || cx >= width {
            break;
        }
        let raster = match get_raster(c, weight, raster_height) {
            Some(r) => r,
            None => {
                cx += char_width;
                continue;
            }
        };
        for (row, line) in raster.raster().iter().enumerate() {
            let py = y + row;
            if py >= height {
                break;
            }
            let row_off = py * stride_bytes;
            for (col, &alpha) in line.iter().enumerate() {
                if alpha == 0 {
                    continue;
                }
                let px = cx + col;
                if px >= right_clip || px >= width {
                    continue;
                }
                let off = row_off + px * bpp;
                let bg = read_px(&w.back[off..off + bpp], fmt);
                write_px(&mut w.back[off..off + bpp], fmt, fg.over(bg, alpha));
                if px < min_x {
                    min_x = px;
                }
                if py < min_y {
                    min_y = py;
                }
                if px > max_xx {
                    max_xx = px;
                }
                if py > max_yy {
                    max_yy = py;
                }
            }
        }
        let _ = row_height;
        cx += char_width;
    }

    if min_x != usize::MAX {
        w.mark_dirty_rect(min_x, min_y, max_xx - min_x + 1, max_yy - min_y + 1);
    }
}

/// The full text path: any size, either weight, and an optional vertical clip.
///
/// `clip` is a half-open `[top, bottom)` band in screen rows. Without it a
/// glyph straddling the edge of a scrolling view would spill over whatever is
/// drawn above or below, so the browser passes its canvas bounds and gets
/// partial rows rendered correctly instead of dropped.
///
/// `y` is signed so a scrolling view can place a line that starts above the
/// top of the screen and is only partly visible.
pub fn draw_text_styled(
    x: usize,
    y: isize,
    s: &str,
    fg: impl Into<Color>,
    bg: impl Into<Color>,
    max_w: usize,
    size: TextSize,
    bold: bool,
    clip: Option<(usize, usize)>,
) {
    let weight = if bold { FontWeight::Bold } else { FontWeight::Regular };
    let raster_height = size.raster();
    let char_width = size.char_w();
    let row_height = size.row_h();

    let (fg, bg) = (fg.into(), bg.into());
    if max_w < char_width { return; }
    let max_chars = max_w / char_width;
    let mut guard = WRITER.lock();
    let Some(w) = guard.as_mut() else { return; };

    let bpp = w.info.bytes_per_pixel;
    let fmt = w.info.pixel_format;
    let stride_pixels = w.info.stride;
    let stride_bytes = stride_pixels * bpp;
    let width = w.info.width;
    let height = w.info.height;
    let right_clip = x.saturating_add(max_w);

    // Rows outside the band are skipped entirely; the default band is the
    // whole screen.
    let (band_top, band_bottom) = clip.unwrap_or((0, height));
    let clip_top = band_top.min(height) as isize;
    let clip_bottom = band_bottom.min(height) as isize;
    if clip_top >= clip_bottom || y >= clip_bottom || y + row_height as isize <= clip_top {
        return;
    }

    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_xx = 0usize;
    let mut max_yy = 0usize;

    let mut cx = x;
    for c in s.chars().take(max_chars) {
        if cx >= right_clip || cx >= width { break; }
        // Clear this character cell first so glyph changes do not leave
        // stale pixels (notably right-aligned numeric displays).
        let cell_w = char_width.min(right_clip.saturating_sub(cx)).min(width.saturating_sub(cx));
        let cell_top = y.max(clip_top).max(0) as usize;
        let cell_bottom = (y + row_height as isize).min(clip_bottom).max(0) as usize;
        if cell_w > 0 && cell_bottom > cell_top {
            for py in cell_top..cell_bottom {
                let row_off = py * stride_bytes;
                for dx in 0..cell_w {
                    let px = cx + dx;
                    let off = row_off + px * bpp;
                    write_px(&mut w.back[off..off + bpp], fmt, bg);
                }
            }
            if cx < min_x { min_x = cx; }
            if cell_top < min_y { min_y = cell_top; }
            let cell_max_x = cx + cell_w - 1;
            if cell_max_x > max_xx { max_xx = cell_max_x; }
            if cell_bottom - 1 > max_yy { max_yy = cell_bottom - 1; }
        }

        let raster = match get_raster(c, weight, raster_height) {
            Some(r) => r,
            None => { cx += char_width; continue; }
        };
        for (row, line) in raster.raster().iter().enumerate() {
            let py = y + row as isize;
            if py >= height as isize || py >= clip_bottom { break; }
            if py < clip_top { continue; }
            let py = py as usize;
            let row_off = py * stride_bytes;
            for (col, &alpha) in line.iter().enumerate() {
                if alpha == 0 { continue; }
                let px = cx + col;
                if px >= right_clip || px >= width { continue; }
                let off = row_off + px * bpp;
                write_px(&mut w.back[off..off + bpp], fmt, fg.over(bg, alpha));
                if px < min_x { min_x = px; }
                if py < min_y { min_y = py; }
                if px > max_xx { max_xx = px; }
                if py > max_yy { max_yy = py; }
            }
        }
        cx += char_width;
    }

    if min_x != usize::MAX {
        w.mark_dirty_rect(min_x, min_y, max_xx - min_x + 1, max_yy - min_y + 1);
    }
}

/// Like [`draw_text_styled`] at [`TextSize::Normal`], but each character
/// carries its own foreground colour — the Code Editor's syntax highlighter
/// uses this so a whole line can be drawn (and its dirty rect unioned) with
/// a single `WRITER` lock instead of one `draw_text_blended` call per token.
pub fn draw_text_multicolor(
    x: usize,
    y: isize,
    chars: &[(char, Color)],
    bg: impl Into<Color>,
    max_w: usize,
) {
    let weight = FontWeight::Regular;
    let raster_height = TextSize::Normal.raster();
    let char_width = TextSize::Normal.char_w();
    let row_height = TextSize::Normal.row_h();

    let bg = bg.into();
    if max_w < char_width {
        return;
    }
    let max_chars = max_w / char_width;
    let mut guard = WRITER.lock();
    let Some(w) = guard.as_mut() else { return };

    let bpp = w.info.bytes_per_pixel;
    let fmt = w.info.pixel_format;
    let stride_bytes = w.info.stride * bpp;
    let width = w.info.width;
    let height = w.info.height;
    let right_clip = x.saturating_add(max_w);

    if y >= height as isize || y + row_height as isize <= 0 {
        return;
    }

    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_xx = 0usize;
    let mut max_yy = 0usize;

    let mut cx = x;
    for &(c, fg) in chars.iter().take(max_chars) {
        if cx >= right_clip || cx >= width {
            break;
        }
        let cell_w = char_width.min(right_clip.saturating_sub(cx)).min(width.saturating_sub(cx));
        let cell_top = y.max(0) as usize;
        let cell_bottom = (y + row_height as isize).min(height as isize).max(0) as usize;
        if cell_w > 0 && cell_bottom > cell_top {
            for py in cell_top..cell_bottom {
                let row_off = py * stride_bytes;
                for dx in 0..cell_w {
                    let px = cx + dx;
                    let off = row_off + px * bpp;
                    write_px(&mut w.back[off..off + bpp], fmt, bg);
                }
            }
            if cx < min_x { min_x = cx; }
            if cell_top < min_y { min_y = cell_top; }
            let cell_max_x = cx + cell_w - 1;
            if cell_max_x > max_xx { max_xx = cell_max_x; }
            if cell_bottom - 1 > max_yy { max_yy = cell_bottom - 1; }
        }

        let raster = match get_raster(c, weight, raster_height) {
            Some(r) => r,
            None => { cx += char_width; continue; }
        };
        for (row, line) in raster.raster().iter().enumerate() {
            let py = y + row as isize;
            if py >= height as isize { break; }
            if py < 0 { continue; }
            let py = py as usize;
            let row_off = py * stride_bytes;
            for (col, &alpha) in line.iter().enumerate() {
                if alpha == 0 { continue; }
                let px = cx + col;
                if px >= right_clip || px >= width { continue; }
                let off = row_off + px * bpp;
                write_px(&mut w.back[off..off + bpp], fmt, fg.over(bg, alpha));
                if px < min_x { min_x = px; }
                if py < min_y { min_y = py; }
                if px > max_xx { max_xx = px; }
                if py > max_yy { max_yy = py; }
            }
        }
        cx += char_width;
    }

    if min_x != usize::MAX {
        w.mark_dirty_rect(min_x, min_y, max_xx - min_x + 1, max_yy - min_y + 1);
    }
}
