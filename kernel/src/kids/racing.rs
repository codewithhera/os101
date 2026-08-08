//! Simple top-down car racing for kids.
//!
//! Stay in your lane, dodge the other cars. Left / Right steer; Up / Down
//! change speed. The road scrolls automatically at the current speed.

use crate::color::Color;
use crate::framebuffer;
use crate::sound;

pub const CANVAS_W: usize = 280;
pub const CANVAS_H: usize = 320;
const LANES: usize = 3;
const CAR_W: i32 = 36;
const CAR_H: i32 = 48;
const ENEMY_COUNT: usize = 4;
const MIN_SPEED: i32 = 2;
const MAX_SPEED: i32 = 12;

const BG: Color = Color::hex(0x1E293B);
const ROAD: Color = Color::hex(0x334155);
const LINE: Color = Color::hex(0xF8FAFC);
const PLAYER: Color = Color::hex(0x22D3EE);
const ENEMY: Color = Color::hex(0xF43F5E);
const GRASS: Color = Color::hex(0x166534);

pub struct State {
    pub lane: usize,
    pub enemies: [(usize, i32); ENEMY_COUNT],
    pub score: u32,
    pub speed: i32,
    pub game_over: bool,
    pub scroll: i32,
    pub rng: u32,
    pub last_step_ticks: u64,
    /// Playfield size — matches the canvas widget so maximise scales the road.
    pub width: i32,
    pub height: i32,
}

impl State {
    pub fn new() -> Self {
        let mut s = Self {
            lane: 1,
            enemies: [(0, -80); ENEMY_COUNT],
            score: 0,
            speed: 4,
            game_over: false,
            scroll: 0,
            rng: 0xC0FFEE,
            last_step_ticks: 0,
            width: CANVAS_W as i32,
            height: CANVAS_H as i32,
        };
        for i in 0..ENEMY_COUNT {
            s.spawn_enemy(i, -(i as i32) * 90 - 40);
        }
        s
    }

    pub fn restart(&mut self) {
        let (w, h) = (self.width, self.height);
        *self = Self::new();
        self.width = w;
        self.height = h;
        for i in 0..ENEMY_COUNT {
            self.spawn_enemy(i, -(i as i32) * 90 - 40);
        }
    }

    /// Keep cars on the road when the window is resized / maximised.
    pub fn resize(&mut self, w: usize, h: usize) {
        let w = w.max(160) as i32;
        let h = h.max(160) as i32;
        if w == self.width && h == self.height {
            return;
        }
        let old_h = self.height.max(1);
        for enemy in &mut self.enemies {
            enemy.1 = enemy.1 * h / old_h;
        }
        self.width = w;
        self.height = h;
    }

    pub fn nudge(&mut self, dir: i32) {
        if self.game_over {
            return;
        }
        if dir < 0 && self.lane > 0 {
            self.lane -= 1;
            sound::blip();
        } else if dir > 0 && self.lane + 1 < LANES {
            self.lane += 1;
            sound::blip();
        }
    }

    pub fn faster(&mut self) {
        if self.game_over {
            return;
        }
        if self.speed < MAX_SPEED {
            self.speed += 1;
            sound::accelerate();
        }
    }

    pub fn slower(&mut self) {
        if self.game_over {
            return;
        }
        if self.speed > MIN_SPEED {
            self.speed -= 1;
            sound::decelerate();
        }
    }

    fn next_rng(&mut self) -> u32 {
        self.rng = self.rng.wrapping_mul(1664525).wrapping_add(1013904223);
        self.rng
    }

    fn lane_x(&self, lane: usize) -> i32 {
        let road_w = self.width - 40;
        let lane_w = road_w / LANES as i32;
        20 + lane as i32 * lane_w + (lane_w - CAR_W) / 2
    }

    fn spawn_enemy(&mut self, i: usize, y: i32) {
        let lane = (self.next_rng() as usize) % LANES;
        self.enemies[i] = (lane, y);
    }

    pub fn step(&mut self) {
        if self.game_over {
            return;
        }
        self.scroll = (self.scroll + self.speed) % 40;
        self.score += 1;
        // Gentle auto-ramp so a parked player still gets a challenge, but the
        // Up/Down keys remain the main speed control.
        if self.score % 250 == 0 && self.speed < MAX_SPEED {
            self.speed += 1;
        }

        let player_y = self.height - CAR_H - 16;
        for i in 0..ENEMY_COUNT {
            self.enemies[i].1 += self.speed;
            if self.enemies[i].1 > self.height + 20 {
                let y = -CAR_H - (self.next_rng() as i32 % 120);
                self.spawn_enemy(i, y);
                continue;
            }
            if self.enemies[i].0 == self.lane {
                let ey = self.enemies[i].1;
                if ey + CAR_H > player_y && ey < player_y + CAR_H {
                    self.game_over = true;
                    sound::boom();
                    return;
                }
            }
        }
    }

    pub fn status_line(&self) -> alloc::string::String {
        if self.game_over {
            alloc::format!("Crash!  Score {}  — Restart", self.score)
        } else {
            alloc::format!("Score {}   Speed {}  (↑ faster ↓ slower)", self.score, self.speed)
        }
    }

    fn draw_car(cx: usize, cy: usize, canvas_h: i32, x: i32, y: i32, color: Color) {
        if y + CAR_H < 0 || y >= canvas_h {
            return;
        }
        let sx = cx + x.max(0) as usize;
        let sy = cy + y.max(0) as usize;
        let top_clip = if y < 0 { (-y) as usize } else { 0 };
        let h = (CAR_H as usize)
            .saturating_sub(top_clip)
            .min((canvas_h - y.max(0)) as usize);
        if h == 0 {
            return;
        }
        framebuffer::fill_rect(sx, sy, CAR_W as usize, h, color);
        framebuffer::stroke_rect(sx, sy, CAR_W as usize, h, Color::hex(0xFFFFFF));
        if top_clip < 14 && h > 10 {
            framebuffer::fill_rect(
                sx + 6,
                sy + 8usize.saturating_sub(top_clip),
                24,
                10,
                Color::hex(0x0F172A),
            );
        }
    }

    pub fn render(&self, cx: usize, cy: usize, cw: usize, ch: usize) {
        let _ = BG;
        framebuffer::fill_rect(cx, cy, cw, ch, GRASS);
        let road_w = (self.width as usize).saturating_sub(40).min(cw.saturating_sub(40));
        framebuffer::fill_rect(cx + 20, cy, road_w, ch, ROAD);

        let lane_w = ((self.width - 40) / LANES as i32).max(1) as usize;
        for lane in 1..LANES {
            let lx = cx + 20 + lane * lane_w;
            let mut y = self.scroll;
            while y < self.height {
                let sy = cy + y as usize;
                if sy + 16 <= cy + ch {
                    framebuffer::fill_rect(lx.saturating_sub(1), sy, 3, 16, LINE);
                }
                y += 40;
            }
        }
        framebuffer::fill_rect(cx + 18, cy, 4, ch, Color::hex(0xFBBF24));
        let right_rail = cx + 20 + road_w;
        if right_rail + 4 <= cx + cw {
            framebuffer::fill_rect(right_rail, cy, 4, ch, Color::hex(0xFBBF24));
        }

        for &(lane, y) in &self.enemies {
            Self::draw_car(cx, cy, self.height, self.lane_x(lane), y, ENEMY);
        }
        let py = self.height - CAR_H - 16;
        Self::draw_car(cx, cy, self.height, self.lane_x(self.lane), py, PLAYER);

        if self.game_over {
            framebuffer::fill_rect(
                cx + 30,
                cy + ch / 2 - 12,
                cw.saturating_sub(60),
                28,
                Color::hex(0x0F172A),
            );
            framebuffer::draw_text_blended(
                cx + 48,
                cy + ch / 2 - 4,
                "CRASH!  Press Restart",
                Color::hex(0xFDE68A),
                Color::hex(0x0F172A),
                cw.saturating_sub(80),
            );
        }
    }
}
