//! Boot splash — neon intro with developer credits and portrait.
//!
//! Plays once after self-tests and before the shell. Uses the embedded
//! `hera.bmp` portrait and a short animated sequence so the machine feels
//! unique rather than dumping straight into a text prompt.

use crate::color::Color;
use crate::framebuffer;
use crate::theme;

const FG: Color = Color::hex(0xECFEFF);
const MUTED: Color = Color::hex(0x94A3B8);
const ACCENT: Color = theme::ACCENT;
const BG: Color = Color::hex(0x020617);

/// Run the splash for roughly three seconds, then return to the caller.
pub fn play() {
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let start = crate::clock::ticks();
    // ~3.5 s at ~18 Hz — long enough to read the credits.
    let duration = 64u64;
    let portrait = crate::image::hera();

    loop {
        let now = crate::clock::ticks();
        let elapsed = now.wrapping_sub(start);
        if elapsed >= duration {
            break;
        }
        let t = ((elapsed * 255) / duration.max(1)) as u8;
        paint_frame(sw, sh, t, elapsed, portrait);
        framebuffer::mark_entire_dirty();
        framebuffer::present();
        // Yield a little so the loop is not a pure CPU burn.
        for _ in 0..8000 {
            core::hint::spin_loop();
        }
    }

    // Leave a clean console background for the banner / shell that follows.
    framebuffer::fill_rect(0, 0, sw, sh, theme::CONSOLE_BG);
    framebuffer::mark_entire_dirty();
    framebuffer::present();
}

fn paint_frame(sw: usize, sh: usize, t: u8, elapsed: u64, portrait: Option<&crate::image::Image>) {
    framebuffer::fill_rect(0, 0, sw, sh, BG);

    // Phase A (0–40): expanding neon frame from the centre.
    let expand = (t as u32 * 2).min(220) as usize;
    let cx = sw / 2;
    let cy = sh / 2;
    let hw = (sw / 8 + expand).min(sw / 2 - 8);
    let hh = (sh / 10 + expand / 2).min(sh / 2 - 8);
    let frame_a = ACCENT.lerp(Color::hex(0xA78BFA), (elapsed % 18) as u8 * 8);
    if hw > 4 && hh > 4 {
        framebuffer::stroke_rect(cx - hw, cy - hh, hw * 2, hh * 2, frame_a);
        framebuffer::stroke_rect(cx - hw + 3, cy - hh + 3, hw * 2 - 6, hh * 2 - 6, frame_a.darken(40));
    }

    // Horizontal scan beam.
    let beam_y = ((elapsed * 14) as usize) % sh.max(1);
    framebuffer::fill_rect(0, beam_y, sw, 2, ACCENT.lerp(BG, 120));

    // Title block (appears after a short beat).
    if t > 20 {
        let title = "OS101";
        let cw = framebuffer::TextSize::Huge.char_w();
        let tw = title.len() * cw;
        let tx = sw.saturating_sub(tw) / 2;
        let ty = sh / 10;
        framebuffer::draw_text_styled(
            tx,
            ty as isize,
            title,
            FG,
            BG,
            tw + 8,
            framebuffer::TextSize::Huge,
            true,
            None,
        );
        let sub = "A tiny OS that wants to grow up";
        let swd = sub.len() * 8;
        framebuffer::draw_text_blended(
            sw.saturating_sub(swd) / 2,
            ty + 44,
            sub,
            MUTED,
            BG,
            swd + 8,
        );
    }

    // Portrait (slides / fades in early so credits stay on screen longer).
    if t > 45 {
        if let Some(img) = portrait {
            let pw = 160usize.min(img.width);
            let ph = 160usize.min(img.height);
            let px = sw.saturating_sub(pw) / 2;
            let py = sh / 10 + 70;
            // Neon plate behind the photo.
            framebuffer::fill_rect(px.saturating_sub(6), py.saturating_sub(6), pw + 12, ph + 12, Color::hex(0x0F172A));
            framebuffer::stroke_rect(px.saturating_sub(6), py.saturating_sub(6), pw + 12, ph + 12, ACCENT);
            framebuffer::blit_scaled(px, py as isize, pw, ph, &img.pixels, img.width, img.height, sw, None);
        }
    }

    // Developer credits (typewriter reveal).
    if t > 80 {
        let lines: &[&str] = &[
            "Developed by",
            "SM Mamunur Rahaman Hera",
            "(Father of: Inaaya & Aayan)",
            "Software Engineer, Bangladesh",
            "linkedin.com/in/sm-mamunur-rahman",
        ];
        let reveal = ((t as usize).saturating_sub(80) / 10).min(lines.len()).max(1);
        let mut ly = sh.saturating_sub(140);
        for (i, line) in lines.iter().take(reveal).enumerate() {
            let color = if i == 1 { ACCENT } else { FG };
            let lw = line.len() * 8;
            let lx = sw.saturating_sub(lw.min(sw)) / 2;
            framebuffer::draw_text_blended(lx, ly, line, color, BG, sw.saturating_sub(40));
            ly += 18;
        }
    }

    // Progress bar at the bottom.
    let bar_w = (sw * 60 / 100).max(120);
    let bar_x = sw.saturating_sub(bar_w) / 2;
    let bar_y = sh.saturating_sub(36);
    framebuffer::fill_rect(bar_x, bar_y, bar_w, 8, Color::hex(0x1E293B));
    let fill = (bar_w as u32 * t as u32 / 255) as usize;
    framebuffer::fill_rect(bar_x, bar_y, fill.max(2), 8, ACCENT);
    framebuffer::stroke_rect(bar_x, bar_y, bar_w, 8, ACCENT.darken(20));
}
