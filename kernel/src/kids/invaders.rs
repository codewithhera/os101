//! Nokia-style Space Invaders for kids.
//!
//! Move left / right, hold Fire (or Space) for rapid shots. Up / Down change
//! the game speed. The playfield tracks the canvas size so a maximised window
//! is a bigger, faster fight.

use crate::color::Color;
use crate::framebuffer;
use crate::sound;

pub const CANVAS_W: usize = 520;
pub const CANVAS_H: usize = 360;
const COLS: usize = 8;
const ROWS: usize = 4;
/// How many shots can be in flight at once — enough for a satisfying spray.
const MAX_BULLETS: usize = 8;
const MIN_SPEED: i32 = 1;
const MAX_SPEED: i32 = 8;

const BG: Color = Color::hex(0x0B1220);
const SHIP: Color = Color::hex(0x4ADE80);
const BULLET: Color = Color::hex(0xFDE68A);
const ALIEN_A: Color = Color::hex(0xF472B6);
const ALIEN_B: Color = Color::hex(0xA78BFA);
const HUD: Color = Color::hex(0xE2E8F0);

pub struct State {
    pub ship_x: i32,
    pub aliens: [[bool; COLS]; ROWS],
    pub alien_x: i32,
    pub alien_y: i32,
    pub alien_dir: i32,
    pub bullets: [(i32, i32); MAX_BULLETS],
    pub bullet_count: usize,
    pub score: u32,
    pub lives: u8,
    pub game_over: bool,
    pub won: bool,
    pub step_count: u32,
    pub last_step_ticks: u64,
    pub last_fire_ticks: u64,
    /// 1 = chill, higher = faster aliens and bullets.
    pub speed: i32,
    pub width: i32,
    pub height: i32,
}

impl State {
    pub fn new() -> Self {
        let mut s = Self {
            ship_x: 0,
            aliens: [[true; COLS]; ROWS],
            alien_x: 16,
            alien_y: 24,
            alien_dir: 2,
            bullets: [(0, 0); MAX_BULLETS],
            bullet_count: 0,
            score: 0,
            lives: 3,
            game_over: false,
            won: false,
            step_count: 0,
            last_step_ticks: 0,
            last_fire_ticks: 0,
            speed: 3,
            width: CANVAS_W as i32,
            height: CANVAS_H as i32,
        };
        s.ship_x = (s.width - s.ship_w()) / 2;
        s
    }

    pub fn restart(&mut self) {
        let (w, h) = (self.width, self.height);
        *self = Self::new();
        self.resize(w as usize, h as usize);
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        let nw = w.max(200) as i32;
        let nh = h.max(160) as i32;
        if nw == self.width && nh == self.height {
            return;
        }
        let old_w = self.width.max(1);
        let old_h = self.height.max(1);
        self.ship_x = (self.ship_x * nw / old_w).clamp(4, nw - self.ship_w_for(nw) - 4);
        self.alien_x = self.alien_x * nw / old_w;
        self.alien_y = self.alien_y * nh / old_h;
        for i in 0..self.bullet_count {
            self.bullets[i].0 = self.bullets[i].0 * nw / old_w;
            self.bullets[i].1 = self.bullets[i].1 * nh / old_h;
        }
        self.width = nw;
        self.height = nh;
        self.ship_x = self.ship_x.clamp(4, self.width - self.ship_w() - 4);
    }

    fn ship_w_for(&self, width: i32) -> i32 {
        (width / 16).clamp(24, 48)
    }

    fn ship_w(&self) -> i32 {
        self.ship_w_for(self.width)
    }

    fn ship_h(&self) -> i32 {
        (self.height / 28).clamp(10, 16)
    }

    fn alien_w(&self) -> i32 {
        (self.width / 18).clamp(18, 32)
    }

    fn alien_h(&self) -> i32 {
        (self.height / 24).clamp(12, 20)
    }

    fn alien_gap_x(&self) -> i32 {
        (self.width / 50).clamp(6, 14)
    }

    fn alien_gap_y(&self) -> i32 {
        (self.height / 36).clamp(8, 14)
    }

    fn move_step(&self) -> i32 {
        (self.width / 28).clamp(12, 36)
    }

    fn alien_step(&self) -> i32 {
        ((self.width / 200).clamp(2, 5) * self.speed / 3).max(1)
    }

    pub fn nudge(&mut self, dir: i32) {
        if self.game_over || self.won {
            return;
        }
        let dx = if dir < 0 {
            -self.move_step()
        } else {
            self.move_step()
        };
        self.ship_x = (self.ship_x + dx).clamp(4, self.width - self.ship_w() - 4);
    }

    pub fn faster(&mut self) {
        if self.game_over || self.won {
            return;
        }
        if self.speed < MAX_SPEED {
            self.speed += 1;
            sound::accelerate();
        }
    }

    pub fn slower(&mut self) {
        if self.game_over || self.won {
            return;
        }
        if self.speed > MIN_SPEED {
            self.speed -= 1;
            sound::decelerate();
        }
    }

    /// Fire a shot. Rapid fire is allowed — a short cool-down stops the
    /// keyboard repeat from filling every slot in one frame.
    pub fn fire(&mut self) {
        if self.game_over || self.won {
            return;
        }
        if self.bullet_count >= MAX_BULLETS {
            return;
        }
        let now = crate::clock::ticks();
        // Cool-down shrinks as speed rises so "rapid" stays meaningful.
        let cool = (3u64).saturating_sub((self.speed as u64) / 3).max(1);
        if now.saturating_sub(self.last_fire_ticks) < cool && self.last_fire_ticks != 0 {
            return;
        }
        let sw = self.ship_w();
        self.bullets[self.bullet_count] = (self.ship_x + sw / 2 - 1, self.height - 28);
        self.bullet_count += 1;
        self.last_fire_ticks = now;
        sound::zap();
    }

    fn alien_at(&self, c: usize, r: usize, ox: i32, oy: i32) -> (i32, i32) {
        let x = ox + c as i32 * (self.alien_w() + self.alien_gap_x());
        let y = oy + r as i32 * (self.alien_h() + self.alien_gap_y());
        (x, y)
    }

    fn alive_bounds(&self) -> Option<(i32, i32)> {
        let mut min_x = i32::MAX;
        let mut max_x = i32::MIN;
        let aw = self.alien_w();
        for r in 0..ROWS {
            for c in 0..COLS {
                if !self.aliens[r][c] {
                    continue;
                }
                let (x, _) = self.alien_at(c, r, self.alien_x, self.alien_y);
                min_x = min_x.min(x);
                max_x = max_x.max(x + aw);
            }
        }
        if min_x == i32::MAX {
            None
        } else {
            Some((min_x, max_x))
        }
    }

    pub fn step(&mut self) {
        if self.game_over || self.won {
            return;
        }
        self.step_count = self.step_count.wrapping_add(1);

        let aw = self.alien_w();
        let ah = self.alien_h();
        let bullet_h = (self.height / 28).clamp(8, 14);
        let bullet_speed = ((self.height / 50).clamp(5, 10) * self.speed / 3).max(4);

        // Advance every live bullet; drop ones that leave the top or hit.
        let mut write = 0usize;
        for i in 0..self.bullet_count {
            let (bx, by) = self.bullets[i];
            let by = by - bullet_speed;
            if by < 0 {
                continue;
            }
            let mut hit = false;
            'hit: for r in 0..ROWS {
                for c in 0..COLS {
                    if !self.aliens[r][c] {
                        continue;
                    }
                    let (ax, ay) = self.alien_at(c, r, self.alien_x, self.alien_y);
                    if bx + 2 >= ax && bx <= ax + aw && by <= ay + ah && by + bullet_h >= ay {
                        self.aliens[r][c] = false;
                        self.score += 10;
                        hit = true;
                        sound::hit();
                        break 'hit;
                    }
                }
            }
            if !hit {
                self.bullets[write] = (bx, by);
                write += 1;
            }
        }
        self.bullet_count = write;

        // March faster as speed rises (smaller period).
        let march_every = (5i32 - self.speed / 2).clamp(1, 4) as u32;
        if self.step_count % march_every != 0 {
            if self.aliens.iter().all(|row| row.iter().all(|a| !*a)) {
                self.won = true;
                sound::cheer();
            }
            return;
        }

        let Some((left, right)) = self.alive_bounds() else {
            self.won = true;
            sound::cheer();
            return;
        };

        let step = self.alien_step();
        let mut drop = false;
        if self.alien_dir > 0 && right + step >= self.width - 8 {
            drop = true;
        } else if self.alien_dir < 0 && left - step <= 8 {
            drop = true;
        }

        if drop {
            self.alien_dir = -self.alien_dir;
            self.alien_y += (self.height / 30).clamp(10, 18);
        } else {
            self.alien_x += if self.alien_dir > 0 { step } else { -step };
        }

        let bottom = self.alien_y + ROWS as i32 * (ah + self.alien_gap_y());
        if bottom >= self.height - 40 {
            if self.lives > 1 {
                self.lives -= 1;
                sound::wrong();
                self.alien_y = 24;
                self.alien_x = 16;
                self.alien_dir = 2;
                self.bullet_count = 0;
            } else {
                self.lives = 0;
                self.game_over = true;
                sound::boom();
            }
        }

        if self.aliens.iter().all(|row| row.iter().all(|a| !*a)) {
            self.won = true;
            sound::cheer();
        }
    }

    pub fn status_line(&self) -> alloc::string::String {
        if self.won {
            alloc::format!("You win!  Score {}", self.score)
        } else if self.game_over {
            alloc::format!("Invaded!  Score {}  — Restart", self.score)
        } else {
            alloc::format!(
                "Score {}   Lives {}   Speed {}  (↑↓)",
                self.score, self.lives, self.speed
            )
        }
    }

    pub fn render(&self, cx: usize, cy: usize, cw: usize, ch: usize) {
        framebuffer::fill_rect(cx, cy, cw, ch, BG);
        framebuffer::stroke_rect(cx, cy, cw, ch, Color::hex(0x22D3EE));

        let aw = self.alien_w() as usize;
        let ah = self.alien_h() as usize;
        for r in 0..ROWS {
            for c in 0..COLS {
                if !self.aliens[r][c] {
                    continue;
                }
                let (ax, ay) = self.alien_at(c, r, self.alien_x, self.alien_y);
                if ax < 0 || ay < 0 {
                    continue;
                }
                let x = cx + ax as usize;
                let y = cy + ay as usize;
                if x + aw > cx + cw || y + ah > cy + ch {
                    continue;
                }
                let color = if r % 2 == 0 { ALIEN_A } else { ALIEN_B };
                framebuffer::fill_rect(x, y, aw, ah, color);
                let eye = (aw / 5).max(3);
                framebuffer::fill_rect(x + aw / 5, y + ah / 3, eye, eye, Color::hex(0x0F172A));
                framebuffer::fill_rect(x + aw * 3 / 5, y + ah / 3, eye, eye, Color::hex(0x0F172A));
            }
        }

        let bullet_h = (self.height / 28).clamp(8, 14) as usize;
        for i in 0..self.bullet_count {
            let (bx, by) = self.bullets[i];
            if by >= 0 {
                framebuffer::fill_rect(
                    cx + bx as usize,
                    cy + by as usize,
                    3,
                    bullet_h,
                    BULLET,
                );
            }
        }

        let sw = self.ship_w() as usize;
        let sh = self.ship_h() as usize;
        let sx = cx + self.ship_x.max(0) as usize;
        let sy = cy + (self.height as usize).saturating_sub(22);
        framebuffer::fill_rect(sx, sy, sw, sh, SHIP);
        framebuffer::fill_rect(sx + sw / 3, sy.saturating_sub(6), sw / 3, 6, SHIP);

        if self.game_over || self.won {
            let msg = if self.won {
                "YOU WIN!  Press Restart"
            } else {
                "Oh no!  Press Restart"
            };
            framebuffer::fill_rect(
                cx + 40,
                cy + ch / 2 - 12,
                cw.saturating_sub(80),
                28,
                Color::hex(0x111827),
            );
            framebuffer::draw_text_blended(
                cx + 56,
                cy + ch / 2 - 4,
                msg,
                HUD,
                Color::hex(0x111827),
                cw.saturating_sub(100),
            );
        }
    }
}
