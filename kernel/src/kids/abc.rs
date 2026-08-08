//! ABC Fun — tap the letter you see.
//!
//! A big colourful letter on the canvas and three huge answer buttons. Right
//! answers earn stars; wrong ones gently ask to try again. Aimed at early
//! readers who are learning the alphabet.

use crate::color::Color;
use crate::framebuffer;

pub const CANVAS_W: usize = 320;
pub const CANVAS_H: usize = 160;

const BG: Color = Color::hex(0x1E1B4B);
const CARD: Color = Color::hex(0x312E81);
const LETTER: Color = Color::hex(0xFDE68A);
const STAR: Color = Color::hex(0xFBBF24);

const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";

pub struct State {
    pub target: u8,
    pub choices: [u8; 3],
    pub score: u32,
    pub streak: u32,
    pub message: alloc::string::String,
    pub rng: u32,
    pub flash_ticks: u64,
}

impl State {
    pub fn new() -> Self {
        let mut s = Self {
            target: b'A',
            choices: [b'A', b'B', b'C'],
            score: 0,
            streak: 0,
            message: alloc::string::String::from("Tap the letter!"),
            rng: 0xC0FF_EE01,
            flash_ticks: 0,
        };
        s.next_round();
        s
    }

    fn next_u32(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1664525).wrapping_add(1013904223);
        self.rng
    }

    pub fn next_round(&mut self) {
        let ti = (self.next_u32() as usize) % LETTERS.len();
        self.target = LETTERS[ti];

        let mut picks = [self.target, 0, 0];
        let mut n = 1;
        while n < 3 {
            let c = LETTERS[(self.next_u32() as usize) % LETTERS.len()];
            if !picks[..n].contains(&c) {
                picks[n] = c;
                n += 1;
            }
        }
        // Shuffle.
        for i in (1..3).rev() {
            let j = (self.next_u32() as usize) % (i + 1);
            picks.swap(i, j);
        }
        self.choices = picks;
        self.message = alloc::string::String::from("Which letter is this?");
    }

    pub fn pick(&mut self, which: usize) {
        let Some(&chosen) = self.choices.get(which) else {
            return;
        };
        if chosen == self.target {
            self.score += 1;
            self.streak += 1;
            crate::sound::cheer();
            self.message = if self.streak >= 5 {
                alloc::string::String::from("Super star!!!")
            } else if self.streak >= 3 {
                alloc::string::String::from("Great job!")
            } else {
                alloc::string::String::from("Yes!")
            };
            self.next_round();
        } else {
            self.streak = 0;
            crate::sound::wrong();
            self.message = alloc::format!("Try again — look for {}", self.target as char);
        }
    }

    pub fn status_line(&self) -> alloc::string::String {
        alloc::format!("Stars {}   {}", self.score, self.message)
    }

    pub fn choice_label(&self, which: usize) -> alloc::string::String {
        self.choices
            .get(which)
            .map(|c| alloc::format!("  {}  ", *c as char))
            .unwrap_or_default()
    }

    pub fn render(&self, cx: usize, cy: usize, cw: usize, ch: usize) {
        framebuffer::fill_rect(cx, cy, cw, ch, BG);
        let pad = 16usize;
        let card_w = cw.saturating_sub(pad * 2);
        let card_h = ch.saturating_sub(pad * 2);
        framebuffer::fill_rect(cx + pad, cy + pad, card_w, card_h, CARD);
        framebuffer::stroke_rect(cx + pad, cy + pad, card_w, card_h, STAR);

        // Draw the letter as a big blocky glyph using the bitmap font scaled
        // by repeating cells — we lack a huge font, so paint a filled round
        // card and the character enlarged via draw_text near the centre.
        let letter = alloc::format!("{}", self.target as char);
        let lx = cx + cw / 2 - 20;
        let ly = cy + ch / 2 - 24;
        framebuffer::fill_rect(lx.saturating_sub(16), ly.saturating_sub(12), 80, 72, Color::hex(0x4338CA));
        framebuffer::draw_text_styled(
            lx,
            ly as isize,
            &letter,
            LETTER,
            Color::hex(0x4338CA),
            80,
            framebuffer::TextSize::Huge,
            true,
            None,
        );

        // Decorative stars in the corners.
        for (sx, sy) in [(pad + 8, pad + 8), (cw - pad - 16, pad + 8), (pad + 8, ch - pad - 16)] {
            framebuffer::fill_rect(cx + sx, cy + sy, 8, 8, STAR);
        }
    }
}
