//! Breakout / DX-Ball style brick breaker.
//!
//! Bright rainbow bricks, a fat paddle, and a ball that bounces. Arrow keys
//! or on-screen buttons move the paddle. The playfield tracks the canvas size
//! so maximising the window gives a bigger court and faster feel.

use crate::color::Color;
use crate::framebuffer;
use crate::sound;

pub const COLS: usize = 10;
pub const ROWS: usize = 5;
pub const CANVAS_W: usize = 480;
pub const CANVAS_H: usize = 340;
const BRICK_GAP: i32 = 3;

const BG: Color = Color::hex(0x0B1A2E);
const PADDLE: Color = Color::hex(0xFBBF24);
const BALL: Color = Color::hex(0xF8FAFC);
const BRICK_COLORS: [Color; ROWS] = [
    Color::hex(0xEF4444),
    Color::hex(0xF97316),
    Color::hex(0xEAB308),
    Color::hex(0x22C55E),
    Color::hex(0x3B82F6),
];

pub struct State {
    pub paddle_x: i32,
    pub ball_x: i32,
    pub ball_y: i32,
    pub ball_vx: i32,
    pub ball_vy: i32,
    pub bricks: [[bool; COLS]; ROWS],
    pub score: u32,
    pub lives: u8,
    pub game_over: bool,
    pub won: bool,
    pub last_step_ticks: u64,
    /// 1 = slow, higher = faster ball.
    pub speed: i32,
    pub width: i32,
    pub height: i32,
}

const MIN_SPEED: i32 = 1;
const MAX_SPEED: i32 = 8;

impl State {
    pub fn new() -> Self {
        let mut s = Self {
            paddle_x: 0,
            ball_x: 0,
            ball_y: 0,
            ball_vx: 3,
            ball_vy: -3,
            bricks: [[true; COLS]; ROWS],
            score: 0,
            lives: 3,
            game_over: false,
            won: false,
            last_step_ticks: 0,
            speed: 3,
            width: CANVAS_W as i32,
            height: CANVAS_H as i32,
        };
        s.paddle_x = (s.width - s.paddle_w()) / 2;
        s.reset_ball();
        s
    }

    pub fn restart(&mut self) {
        let (w, h) = (self.width, self.height);
        *self = Self::new();
        self.resize(w as usize, h as usize);
    }

    /// Grow / shrink the court to match the canvas widget.
    pub fn resize(&mut self, w: usize, h: usize) {
        let nw = w.max(200) as i32;
        let nh = h.max(160) as i32;
        if nw == self.width && nh == self.height {
            return;
        }
        let old_w = self.width.max(1);
        let old_h = self.height.max(1);
        self.paddle_x = (self.paddle_x * nw / old_w).clamp(0, nw - self.paddle_w_for(nw));
        self.ball_x = self.ball_x * nw / old_w;
        self.ball_y = self.ball_y * nh / old_h;
        self.width = nw;
        self.height = nh;
        self.paddle_x = self.paddle_x.clamp(0, self.width - self.paddle_w());
        self.ball_x = self.ball_x.clamp(self.ball_r(), self.width - self.ball_r());
        self.ball_y = self.ball_y.clamp(self.ball_r(), self.height - self.ball_r());
    }

    fn paddle_w_for(&self, width: i32) -> i32 {
        (width / 6).clamp(56, 140)
    }

    fn paddle_w(&self) -> i32 {
        self.paddle_w_for(self.width)
    }

    fn paddle_h(&self) -> i32 {
        (self.height / 28).clamp(10, 18)
    }

    fn ball_r(&self) -> i32 {
        (self.width / 70).clamp(5, 10)
    }

    fn nudge_step(&self) -> i32 {
        (self.width / 18).clamp(16, 48)
    }

    pub fn nudge_paddle(&mut self, dir: i32) {
        let dx = if dir < 0 {
            -self.nudge_step()
        } else {
            self.nudge_step()
        };
        self.paddle_x = (self.paddle_x + dx).clamp(0, self.width - self.paddle_w());
    }

    pub fn faster(&mut self) {
        if self.game_over || self.won {
            return;
        }
        if self.speed < MAX_SPEED {
            self.speed += 1;
            self.rescale_ball_speed();
            sound::accelerate();
        }
    }

    pub fn slower(&mut self) {
        if self.game_over || self.won {
            return;
        }
        if self.speed > MIN_SPEED {
            self.speed -= 1;
            self.rescale_ball_speed();
            sound::decelerate();
        }
    }

    fn base_speed(&self) -> i32 {
        ((self.width / 160).clamp(3, 6) * self.speed / 3).max(2)
    }

    fn rescale_ball_speed(&mut self) {
        let target = self.base_speed();
        let sx = if self.ball_vx >= 0 { 1 } else { -1 };
        let sy = if self.ball_vy >= 0 { 1 } else { -1 };
        let cur = self.ball_vx.abs().max(self.ball_vy.abs()).max(1);
        // Keep the bounce angle, just change magnitude.
        self.ball_vx = (self.ball_vx.abs() * target / cur).max(1) * sx;
        self.ball_vy = (self.ball_vy.abs() * target / cur).max(1) * sy;
    }

    fn reset_ball(&mut self) {
        self.ball_x = self.paddle_x + self.paddle_w() / 2;
        self.ball_y = self.height - 40;
        let speed = self.base_speed();
        self.ball_vx = if (self.score & 1) == 0 { speed } else { -speed };
        self.ball_vy = -speed;
    }

    fn brick_rect(&self, c: usize, r: usize) -> (i32, i32, i32, i32) {
        let bw = (self.width - BRICK_GAP * (COLS as i32 + 1)) / COLS as i32;
        let bh = (self.height / 18).clamp(12, 22);
        let x = BRICK_GAP + c as i32 * (bw + BRICK_GAP);
        let y = 12 + r as i32 * (bh + BRICK_GAP);
        (x, y, bw, bh)
    }

    pub fn step(&mut self) {
        if self.game_over || self.won {
            return;
        }

        let r = self.ball_r();
        self.ball_x += self.ball_vx;
        self.ball_y += self.ball_vy;

        if self.ball_x - r <= 0 {
            self.ball_x = r;
            self.ball_vx = self.ball_vx.abs();
        } else if self.ball_x + r >= self.width {
            self.ball_x = self.width - r;
            self.ball_vx = -self.ball_vx.abs();
        }
        if self.ball_y - r <= 0 {
            self.ball_y = r;
            self.ball_vy = self.ball_vy.abs();
        }

        let pw = self.paddle_w();
        let ph = self.paddle_h();
        let py = self.height - 18;
        if self.ball_vy > 0
            && self.ball_y + r >= py
            && self.ball_y + r <= py + ph + 4
            && self.ball_x >= self.paddle_x - r
            && self.ball_x <= self.paddle_x + pw + r
        {
            self.ball_y = py - r;
            self.ball_vy = -self.ball_vy.abs();
            let hit = self.ball_x - (self.paddle_x + pw / 2);
            let max_vx = (3 + self.speed).clamp(3, 8);
            self.ball_vx = (hit / 8).clamp(-max_vx, max_vx);
            if self.ball_vx == 0 {
                self.ball_vx = if hit >= 0 { 2 } else { -2 };
            }
            // Keep vertical speed matched to the chosen speed level.
            let target = self.base_speed();
            self.ball_vy = -target;
            sound::blip();
        }

        if self.ball_y - r > self.height {
            if self.lives > 1 {
                self.lives -= 1;
                sound::wrong();
                self.reset_ball();
            } else {
                self.lives = 0;
                self.game_over = true;
                sound::boom();
            }
            return;
        }

        'bricks: for row in 0..ROWS {
            for c in 0..COLS {
                if !self.bricks[row][c] {
                    continue;
                }
                let (bx, by, bw, bh) = self.brick_rect(c, row);
                if self.ball_x + r >= bx
                    && self.ball_x - r <= bx + bw
                    && self.ball_y + r >= by
                    && self.ball_y - r <= by + bh
                {
                    self.bricks[row][c] = false;
                    self.score += 10 * (ROWS as u32 - row as u32);
                    sound::hit();
                    let from_top = (self.ball_y - by).abs() < (self.ball_y - (by + bh)).abs();
                    if from_top {
                        self.ball_vy = -self.ball_vy;
                    } else {
                        self.ball_vx = -self.ball_vx;
                    }
                    break 'bricks;
                }
            }
        }

        if self.bricks.iter().all(|row| row.iter().all(|b| !*b)) {
            self.won = true;
            sound::cheer();
        }
    }

    pub fn status_line(&self) -> alloc::string::String {
        if self.won {
            alloc::format!("You win!  Score {}", self.score)
        } else if self.game_over {
            alloc::format!("Oh no!  Score {}  — Restart", self.score)
        } else {
            alloc::format!(
                "Score {}   Hearts {}   Speed {}  (↑↓)",
                self.score, self.lives, self.speed
            )
        }
    }

    pub fn render(&self, cx: usize, cy: usize, cw: usize, ch: usize) {
        framebuffer::fill_rect(cx, cy, cw, ch, BG);
        framebuffer::stroke_rect(cx, cy, cw, ch, Color::hex(0x64748B));

        for row in 0..ROWS {
            for c in 0..COLS {
                if !self.bricks[row][c] {
                    continue;
                }
                let (bx, by, bw, bh) = self.brick_rect(c, row);
                let x = cx + bx as usize;
                let y = cy + by as usize;
                if x + bw as usize <= cx + cw && y + bh as usize <= cy + ch {
                    framebuffer::fill_rect(x, y, bw as usize, bh as usize, BRICK_COLORS[row]);
                    framebuffer::stroke_rect(
                        x,
                        y,
                        bw as usize,
                        bh as usize,
                        Color::hex(0xFFFFFF),
                    );
                }
            }
        }

        let pw = self.paddle_w() as usize;
        let ph = self.paddle_h() as usize;
        let px = cx + self.paddle_x.max(0) as usize;
        let py = cy + (self.height as usize).saturating_sub(18);
        framebuffer::fill_rect(px, py, pw, ph, PADDLE);

        let r = self.ball_r();
        let bx = cx + (self.ball_x - r).max(0) as usize;
        let by = cy + (self.ball_y - r).max(0) as usize;
        let d = (r * 2) as usize;
        framebuffer::fill_rect(bx, by, d, d, BALL);

        if self.game_over || self.won {
            let msg = if self.won {
                "YOU WIN!  Press Restart"
            } else {
                "Oh no!  Press Restart"
            };
            let mx = cx + 40;
            let my = cy + ch / 2;
            framebuffer::draw_text_blended(mx, my, msg, Color::hex(0xFDE68A), BG, cw.saturating_sub(60));
        }
    }
}
