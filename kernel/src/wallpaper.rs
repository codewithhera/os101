//! The desktop wallpaper: a photograph if the user has chosen one, and
//! otherwise a scene drawn from scratch.
//!
//! The default is a dusk landscape — graded sky, stars, a moon with a soft
//! halo, and three layers of hills — generated from integer noise rather than
//! shipped as a bitmap, so it costs nothing in the disk image and adapts to
//! any screen size.
//!
//! Either way the result is the same: one screen-sized buffer. Producing it
//! touches every pixel, far too slow to repeat every frame, so it is cached
//! and the desktop just blits it. The cache is dropped whenever the size, the
//! theme or the chosen picture changes.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::color::Color;
use crate::image::Image;

struct Cached {
    w: usize,
    h: usize,
    dark: bool,
    pixels: Vec<Color>,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The picture the user picked, and where it came from. The path is kept so
/// the choice can be written down and restored on the next boot.
static PICTURE: Mutex<Option<(String, Arc<Image>)>> = Mutex::new(None);

/// Drop the cached image, so the next paint regenerates it. Called when the
/// theme changes.
pub fn invalidate() {
    *CACHE.lock() = None;
}

/// Use `image` as the wallpaper, remembering that it came from `path`.
pub fn set_picture(path: &str, image: Arc<Image>) {
    // The guard is dropped before `invalidate` takes the cache lock: `paint`
    // acquires them the other way round, and holding both here in the
    // opposite order is all it takes to deadlock the two.
    *PICTURE.lock() = Some((path.to_string(), image));
    invalidate();
}

/// Go back to the generated scene.
pub fn clear_picture() {
    *PICTURE.lock() = None;
    invalidate();
}

/// Where the current wallpaper came from, if it is a picture.
pub fn picture_path() -> Option<String> {
    PICTURE.lock().as_ref().map(|(p, _)| p.clone())
}

/// Draw the wallpaper into the top-left `w × h` of the screen, generating it
/// first if the cache is cold or stale.
pub fn paint(w: usize, h: usize, dark: bool) {
    if w == 0 || h == 0 {
        return;
    }

    let mut cache = CACHE.lock();
    let stale = match cache.as_ref() {
        Some(c) => c.w != w || c.h != h || c.dark != dark,
        None => true,
    };
    if stale {
        *cache = Some(Cached { w, h, dark, pixels: generate(w, h, dark) });
    }

    // Hold the lock across the blit rather than cloning several megabytes.
    if let Some(c) = cache.as_ref() {
        crate::framebuffer::blit(0, 0, w, h, &c.pixels, w);
    }
}

// ── Scene ───────────────────────────────────────────────────────────────────

/// Sky colour stops, from the top of the screen down to the horizon.
/// Kept for dark-mode experiments / future dusk variant.
#[allow(dead_code)]
const SKY_DUSK: [(u16, Color); 5] = [
    (0, Color::hex(0x05070F)),
    (90, Color::hex(0x0F2027)),
    (170, Color::hex(0x1D3B4A)),
    (225, Color::hex(0x2C5364)),
    (255, Color::hex(0x4C7F8C)),
];

const SKY_NIGHT: [(u16, Color); 5] = [
    (0, Color::hex(0x02030A)),
    (90, Color::hex(0x060B14)),
    (170, Color::hex(0x0A1620)),
    (225, Color::hex(0x0E2029)),
    (255, Color::hex(0x16323C)),
];

/// Hill silhouettes, far to near. Each is (colour, base height as a
/// percentage of screen height, amplitude percentage, noise period, seed).
const HILLS_DARK: [(Color, usize, usize, i32, u32); 3] = [
    (Color::hex(0x1B3A47), 62, 9, 320, 0x51ED),
    (Color::hex(0x122A34), 74, 11, 210, 0xB19B),
    (Color::hex(0x0A1A21), 86, 13, 140, 0x2C7F),
];

fn generate(w: usize, h: usize, dark: bool) -> Vec<Color> {
    if let Some((_, picture)) = PICTURE.lock().as_ref() {
        return from_picture(picture, w, h, dark);
    }
    generate_scene(w, h, dark)
}

/// Fit a photograph to the screen the way every desktop does: scale it until
/// it covers the whole area and crop what hangs over the edges, rather than
/// stretching it out of shape or leaving bars down the sides.
///
/// Sampling is bilinear. Nearest-neighbour is cheaper, but a wallpaper is
/// almost always being enlarged — a 288-pixel-wide thumbnail stretched across
/// 1280 pixels — and the blockiness is impossible to miss at that scale.
fn from_picture(picture: &Image, w: usize, h: usize, dark: bool) -> Vec<Color> {
    let mut px = vec![Color::BLACK; w * h];
    if picture.width == 0 || picture.height == 0 || picture.pixels.len() < picture.width * picture.height {
        return px;
    }

    // Work in 16.16 fixed point: the kernel has no hardware floating point in
    // its interrupt-safe paths, and this is exact enough for pixel addresses.
    const ONE: u64 = 1 << 16;
    let scale_x = (picture.width as u64 * ONE) / w.max(1) as u64;
    let scale_y = (picture.height as u64 * ONE) / h.max(1) as u64;
    // The smaller ratio is the one that still covers both axes.
    let step = scale_x.min(scale_y).max(1);

    // Centre the crop.
    let span_x = step * w as u64;
    let span_y = step * h as u64;
    let origin_x = ((picture.width as u64 * ONE).saturating_sub(span_x)) / 2;
    let origin_y = ((picture.height as u64 * ONE).saturating_sub(span_y)) / 2;

    for y in 0..h {
        let sy = origin_y + step * y as u64;
        for x in 0..w {
            let sx = origin_x + step * x as u64;
            let mut c = sample(picture, sx, sy);
            if dark {
                c = c.darken(30);
            }
            px[y * w + x] = c;
        }
    }

    vignette(&mut px, w, h);
    px
}

/// Bilinear sample at a 16.16 fixed-point coordinate.
fn sample(img: &Image, x: u64, y: u64) -> Color {
    const SHIFT: u32 = 16;
    const MASK: u64 = (1 << SHIFT) - 1;

    let x0 = (x >> SHIFT) as usize;
    let y0 = (y >> SHIFT) as usize;
    let x1 = (x0 + 1).min(img.width - 1);
    let y1 = (y0 + 1).min(img.height - 1);
    let x0 = x0.min(img.width - 1);
    let y0 = y0.min(img.height - 1);

    let fx = (x & MASK) as u32 >> 8;
    let fy = (y & MASK) as u32 >> 8;

    let at = |cx: usize, cy: usize| img.pixels[cy * img.width + cx];
    let top = at(x0, y0).lerp(at(x1, y0), fx as u8);
    let bottom = at(x0, y1).lerp(at(x1, y1), fx as u8);
    top.lerp(bottom, fy as u8)
}

fn generate_scene(w: usize, h: usize, dark: bool) -> Vec<Color> {
    if !dark {
        return generate_cyber(w, h);
    }
    let stops: &[(u16, Color)] = &SKY_NIGHT;
    let mut px = vec![Color::BLACK; w * h];

    // The horizon sits where the nearest hill starts, so the sky gradient is
    // compressed into the part of the screen that actually shows sky.
    let horizon = h * 62 / 100;

    let moon_x = (w * 78 / 100) as i32;
    let moon_y = (h * 20 / 100) as i32;
    let moon_r = (h / 26).max(6) as i32;

    for y in 0..h {
        // Position within the sky, saturating below the horizon.
        let t = if horizon > 0 {
            ((y.min(horizon) * 255) / horizon) as u16
        } else {
            255
        };
        let base = gradient(stops, t);

        for x in 0..w {
            let mut c = base;

            // Stars thin out towards the horizon and never sit on the moon.
            if y < horizon {
                if let Some(b) = star(x, y, horizon) {
                    c = c.lerp(Color::hex(0xFFFFFF), b);
                }
            }

            // Moon disc plus an inverse-square halo.
            let dx = x as i32 - moon_x;
            let dy = y as i32 - moon_y;
            let d2 = dx * dx + dy * dy;
            if d2 <= moon_r * moon_r {
                c = Color::hex(0xF4F7E8);
            } else {
                let halo = moon_r * 9;
                if d2 < halo * halo {
                    // Falls off with distance, squared again for a soft edge.
                    let d = isqrt(d2 as u32) as i32;
                    let f = ((halo - d) * 255 / halo).clamp(0, 255);
                    let f = (f * f / 255) as u8;
                    c = c.lerp(Color::hex(0xBFD8D5), (f / 3) as u8);
                }
            }

            px[y * w + x] = c;
        }
    }

    for (colour, base_pct, amp_pct, period, seed) in HILLS_DARK {
        let colour = colour.darken(20);
        let base = h * base_pct / 100;
        let amp = (h * amp_pct / 100) as i32;
        for x in 0..w {
            let top = (base as i32 - fbm(x as i32, period, seed) * amp / 255).max(0) as usize;
            for y in top..h {
                let depth = ((y - top) * 255 / h.max(1)) as u32;
                px[y * w + x] = colour.lighten(depth / 8);
            }
        }
    }

    vignette(&mut px, w, h);
    px
}

/// Soft neon aurora backdrop — colourful enough for kids, with neon accents
/// but solid readable regions (no dense grid noise).
fn generate_cyber(w: usize, h: usize) -> Vec<Color> {
    let mut px = vec![Color::BLACK; w * h];
    let top = Color::hex(0x0B1026);
    let mid = Color::hex(0x1A1440);
    let bottom = Color::hex(0x0F2A3A);

    // Soft colour blobs (aurora) behind a calm gradient.
    let orbs: [(i32, i32, i32, Color); 4] = [
        ((w as i32) * 22 / 100, (h as i32) * 28 / 100, (h as i32) / 3, Color::hex(0x22D3EE)),
        ((w as i32) * 70 / 100, (h as i32) * 22 / 100, (h as i32) / 4, Color::hex(0xA78BFA)),
        ((w as i32) * 48 / 100, (h as i32) * 55 / 100, (h as i32) / 3, Color::hex(0xF472B6)),
        ((w as i32) * 80 / 100, (h as i32) * 70 / 100, (h as i32) / 5, Color::hex(0x34D399)),
    ];

    for y in 0..h {
        let t = if h > 1 { ((y * 255) / (h - 1)) as u8 } else { 0 };
        let row = if t < 140 {
            top.lerp(mid, ((t as u16 * 255) / 140) as u8)
        } else {
            mid.lerp(bottom, (((t as u16 - 140) * 255) / 115) as u8)
        };
        for x in 0..w {
            let mut c = row;
            for &(ox, oy, r, col) in &orbs {
                let dx = x as i32 - ox;
                let dy = y as i32 - oy;
                let d2 = dx * dx + dy * dy;
                let r2 = r * r;
                if d2 < r2 {
                    let d = isqrt(d2 as u32) as i32;
                    let f = ((r - d) * 90 / r).clamp(0, 90) as u8;
                    c = c.lerp(col, f);
                }
            }
            // Sparse gentle stars — not a busy grid.
            if (hash32((x as u32).wrapping_mul(73).wrapping_add(y as u32 * 19)) & 0x7FF) < 2 {
                c = c.lerp(Color::hex(0xECFEFF), 140);
            }
            px[y * w + x] = c;
        }
    }

    // Soft neon horizon band so the desktop feels grounded.
    let hy = h * 68 / 100;
    if hy < h {
        for x in 0..w {
            px[hy * w + x] = px[hy * w + x].lerp(Color::hex(0x22D3EE), 55);
            if hy + 1 < h {
                px[(hy + 1) * w + x] = px[(hy + 1) * w + x].lerp(Color::hex(0x22D3EE), 25);
            }
        }
    }

    soft_vignette(&mut px, w, h);
    px
}

fn soft_vignette(px: &mut [Color], w: usize, h: usize) {
    let cx = (w / 2) as i32;
    let cy = (h / 2) as i32;
    let max_d = (isqrt((cx * cx + cy * cy) as u32) as i32).max(1);
    for y in 0..h {
        for x in 0..w {
            let dx = x as i32 - cx;
            let dy = y as i32 - cy;
            let d = isqrt((dx * dx + dy * dy) as u32) as i32;
            let f = (d * 100 / max_d) as u32;
            if f > 72 {
                px[y * w + x] = px[y * w + x].darken((f - 72) / 4);
            }
        }
    }
}

/// Darken the corners. Subtle, but it stops the desktop looking flat and
/// helps window edges stand out.
fn vignette(px: &mut [Color], w: usize, h: usize) {
    let cx = (w / 2) as i32;
    let cy = (h / 2) as i32;
    let max_d = (isqrt((cx * cx + cy * cy) as u32) as i32).max(1);
    for y in 0..h {
        for x in 0..w {
            let dx = x as i32 - cx;
            let dy = y as i32 - cy;
            let d = isqrt((dx * dx + dy * dy) as u32) as i32;
            let f = (d * 100 / max_d) as u32;
            if f > 55 {
                px[y * w + x] = px[y * w + x].darken((f - 55) / 2);
            }
        }
    }
}

/// Brightness of a star at this pixel, or `None` for empty sky. Stars are
/// placed on a coarse grid with one hashed candidate per cell, which spreads
/// them out without needing to store a list.
fn star(x: usize, y: usize, horizon: usize) -> Option<u8> {
    const CELL: usize = 22;
    let hv = hash32(((y / CELL) as u32).wrapping_mul(1021).wrapping_add((x / CELL) as u32));

    // Only about a fifth of cells hold a star at all.
    if hv & 0xFF > 52 {
        return None;
    }
    // The rest of the hash picks where in the cell it sits.
    if x % CELL != ((hv >> 8) as usize) % CELL || y % CELL != ((hv >> 16) as usize) % CELL {
        return None;
    }

    // Fade out as the sky approaches the horizon haze.
    let fade = 255 - (y * 255 / horizon.max(1)).min(255);
    Some(((90 + (hv >> 24) / 2) * fade as u32 / 255) as u8)
}

// ── Maths ───────────────────────────────────────────────────────────────────

/// Sample a multi-stop gradient at `t` (0..=255).
fn gradient(stops: &[(u16, Color)], t: u16) -> Color {
    if stops.is_empty() {
        return Color::BLACK;
    }
    let mut prev = stops[0];
    for &(pos, colour) in stops {
        if t <= pos {
            if pos == prev.0 {
                return colour;
            }
            let span = (pos - prev.0) as u32;
            let local = ((t - prev.0) as u32 * 255 / span) as u8;
            return prev.1.lerp(colour, local);
        }
        prev = (pos, colour);
    }
    prev.1
}

/// Fractal noise in 0..=255: three octaves of interpolated value noise.
fn fbm(x: i32, period: i32, seed: u32) -> i32 {
    let mut total = 0;
    let mut amp = 128;
    let mut p = period.max(1);
    let mut s = seed;
    for _ in 0..3 {
        total += noise(x, p, s) * amp / 255;
        amp /= 2;
        p = (p / 2).max(1);
        s = s.wrapping_mul(0x9E37_79B9).wrapping_add(1);
    }
    total.clamp(0, 255)
}

/// Interpolated 1-D value noise in 0..=255.
fn noise(x: i32, period: i32, seed: u32) -> i32 {
    let i = x.div_euclid(period);
    let f = x.rem_euclid(period);

    let a = (hash32((i as u32).wrapping_add(seed)) >> 24) as i32;
    let b = (hash32(((i + 1) as u32).wrapping_add(seed)) >> 24) as i32;

    // Smoothstep, 3t² - 2t³, in fixed point with 256 as one.
    let t = f * 256 / period;
    let s = (t * t * (3 * 256 - 2 * t)) / (256 * 256);
    a + (b - a) * s / 256
}

fn hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    x ^= x >> 16;
    x
}

pub fn selftest() -> crate::selftest::Report {
    let mut r = crate::selftest::Report::new();
    // The tests below set and clear the chosen picture, and by this point in
    // the boot the user's own choice has already been restored. Put it back
    // afterwards rather than leaving them staring at the default scene.
    let saved = PICTURE.lock().clone();

    let solid = |w: usize, h: usize, c: Color| Image {
        width: w,
        height: h,
        pixels: vec![c; w * h],
    };

    let out = from_picture(&solid(4, 4, Color::hex(0x3366CC)), 64, 32, false);
    r.check("a picture fills the screen buffer", out.len() == 64 * 32);
    // The vignette only touches the corners, so the middle keeps the colour.
    r.check("a flat picture keeps its colour", out[16 * 64 + 32] == Color::hex(0x3366CC));
    r.check("the corners are darkened", out[0].0 < Color::hex(0x3366CC).0);

    let dark = from_picture(&solid(4, 4, Color::hex(0x3366CC)), 64, 32, true);
    r.check(
        "dark mode dims the picture",
        dark[16 * 64 + 32].0 < out[16 * 64 + 32].0,
    );

    // A wide picture on a narrow screen must be cropped left and right, not
    // squashed: sampling stays inside the source either way, so the check is
    // that nothing outside the image is ever read.
    for (iw, ih, sw, sh) in [(1, 1, 8, 8), (100, 1, 16, 16), (1, 100, 16, 16), (3, 7, 40, 9)] {
        let img = solid(iw, ih, Color::WHITE);
        let px = from_picture(&img, sw, sh, false);
        r.check("odd aspect ratios still fill", px.len() == sw * sh);
    }

    let degenerate = Image { width: 0, height: 0, pixels: Vec::new() };
    r.check(
        "an empty picture does not panic",
        from_picture(&degenerate, 8, 8, false).len() == 64,
    );
    let lying = Image { width: 8, height: 8, pixels: vec![Color::WHITE; 3] };
    r.check(
        "a picture shorter than its dimensions is refused",
        from_picture(&lying, 8, 8, false).iter().all(|c| *c == Color::BLACK),
    );

    // A gradient across the source proves the crop is centred: with a source
    // twice as wide as the target aspect, the middle column of the screen must
    // be the middle column of the picture.
    let mut ramp = Image { width: 16, height: 16, pixels: vec![Color::BLACK; 256] };
    for y in 0..16 {
        for x in 0..16 {
            ramp.pixels[y * 16 + x] = Color::gray((x * 17) as u8);
        }
    }
    let cropped = from_picture(&ramp, 8, 16, false);
    let left = cropped[8 * 8].r() as i32;
    let right = cropped[8 * 8 + 7].r() as i32;
    r.check("the crop is centred", (left as i32 - (255 - right)).abs() <= 24);
    r.check("the crop keeps the gradient", right > left);

    let scene = generate_scene(80, 60, false);
    r.check("the generated scene fills the screen", scene.len() == 80 * 60);
    let night = generate_scene(80, 60, true);
    r.check(
        "night is darker than dusk",
        night[5 * 80 + 40].luma() <= scene[5 * 80 + 40].luma(),
    );

    // Not "no picture by default": by this point the user's saved choice has
    // been restored, and on a machine that has one, there is a picture here.
    clear_picture();
    r.check("clearing leaves no picture", picture_path().is_none());
    set_picture("/disk/test.png", Arc::new(solid(2, 2, Color::WHITE)));
    r.check("the chosen picture is remembered", picture_path().as_deref() == Some("/disk/test.png"));
    r.check("choosing a picture drops the cache", CACHE.lock().is_none());
    clear_picture();
    r.check("clearing goes back to the scene", picture_path().is_none());

    *PICTURE.lock() = saved;
    invalidate();
    r
}

/// Integer square root. The kernel builds without hardware floating point,
/// so distances are computed this way rather than with `sqrt`.
fn isqrt(n: u32) -> u32 {
    if n == 0 {
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
