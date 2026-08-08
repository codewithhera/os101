//! Packed RGB colour shared by every drawing path.
//!
//! The framebuffer previously took a single `u8` intensity and wrote it to all
//! three channels. `Color` replaces that. `impl From<u8> for Color` maps an
//! intensity to the equivalent grey, so existing call sites keep compiling and
//! can be migrated to real colours one at a time.

/// Packed 24-bit colour, laid out as `0x00RR_GGBB`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Color(pub u32);

impl Color {
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color(((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    /// Build from a `0xRRGGBB` literal.
    #[inline]
    pub const fn hex(v: u32) -> Self {
        Color(v & 0x00FF_FFFF)
    }

    #[inline]
    pub const fn gray(v: u8) -> Self {
        Color::rgb(v, v, v)
    }

    #[inline]
    pub const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }

    #[inline]
    pub const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }

    #[inline]
    pub const fn b(self) -> u8 {
        self.0 as u8
    }

    /// True when all three channels match, which lets `fill_rect` use a plain
    /// byte memset instead of writing a repeating pattern.
    #[inline]
    pub const fn is_gray(self) -> bool {
        self.r() == self.g() && self.g() == self.b()
    }

    /// Rec.601 luminance, for `PixelFormat::U8` framebuffers.
    #[inline]
    pub const fn luma(self) -> u8 {
        ((self.r() as u32 * 299 + self.g() as u32 * 587 + self.b() as u32 * 114) / 1000) as u8
    }

    /// Composite `self` over `bg`; `alpha` 0 keeps `bg`, 255 keeps `self`.
    #[inline]
    pub const fn over(self, bg: Color, alpha: u8) -> Color {
        let a = alpha as u32;
        let inv = 255 - a;
        Color::rgb(
            ((bg.r() as u32 * inv + self.r() as u32 * a) / 255) as u8,
            ((bg.g() as u32 * inv + self.g() as u32 * a) / 255) as u8,
            ((bg.b() as u32 * inv + self.b() as u32 * a) / 255) as u8,
        )
    }

    /// Interpolate towards `other`; `t` 0 is `self`, 255 is `other`.
    #[inline]
    pub const fn lerp(self, other: Color, t: u8) -> Color {
        other.over(self, t)
    }

    /// Scale every channel by `num/den`, saturating at 255.
    /// `shade(120, 100)` is 20% brighter, `shade(80, 100)` is 20% darker.
    #[inline]
    pub const fn shade(self, num: u32, den: u32) -> Color {
        Color::rgb(
            scale_channel(self.r(), num, den),
            scale_channel(self.g(), num, den),
            scale_channel(self.b(), num, den),
        )
    }

    #[inline]
    pub const fn lighten(self, percent: u32) -> Color {
        self.lerp(Color::WHITE, pct_to_alpha(percent))
    }

    #[inline]
    pub const fn darken(self, percent: u32) -> Color {
        self.lerp(Color::BLACK, pct_to_alpha(percent))
    }
}

#[inline]
const fn scale_channel(v: u8, num: u32, den: u32) -> u8 {
    let x = v as u32 * num / den;
    if x > 255 { 255 } else { x as u8 }
}

#[inline]
const fn pct_to_alpha(percent: u32) -> u8 {
    let p = if percent > 100 { 100 } else { percent };
    ((p * 255) / 100) as u8
}

impl From<u8> for Color {
    #[inline]
    fn from(v: u8) -> Self {
        Color::gray(v)
    }
}
