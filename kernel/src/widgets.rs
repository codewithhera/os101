//! Clickable on-screen buttons.
//!
//! A tiny widget layer that lives *below* the shell's scrollable text area
//! and *above* the status bar. Each button knows its bounds, its label, and
//! which command to execute on click. It renders three visual states so the
//! user can see mouse interactions: normal, hover, pressed (after click).
//!
//! Phase 6 of `TASKS.md` ends with "clicks register"; this is the first
//! concrete use of that — a mouse that can actually do something.

use alloc::string::String;

/// What happens when a button is clicked / double-clicked.
#[derive(Copy, Clone)]
pub enum Action {
    Clear,
    Help,
    Reboot,
    Shutdown,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum State {
    Normal,
    Hover,
    Pressed,
}

pub struct Button {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub label: &'static str,
    pub action: Action,
    pub state: State,
}

impl Button {
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// Layout buttons along the bottom of the screen, inside the framebuffer's
/// reserved bottom region (see `framebuffer::BOTTOM_RESERVED`). Sits above
/// the status bar (status bar = 16 px text + 16 px margin).
pub fn build(_screen_w: usize, screen_h: usize) -> [Button; 4] {
    let y = screen_h.saturating_sub(112).saturating_add(8);
    let h = 28;
    let w = 140;
    let gap = 12;
    let x0 = 16;
    [
        Button { x: x0,                 y, w, h, label: "Clear",    action: Action::Clear,    state: State::Normal },
        Button { x: x0 + (w + gap),     y, w, h, label: "Help",     action: Action::Help,     state: State::Normal },
        Button { x: x0 + 2*(w + gap),   y, w, h, label: "Reboot",   action: Action::Reboot,   state: State::Normal },
        Button { x: x0 + 3*(w + gap),   y, w, h, label: "Shutdown", action: Action::Shutdown, state: State::Normal },
    ]
}

/// Find the index of the button containing the point, if any.
pub fn hit(buttons: &[Button], px: usize, py: usize) -> Option<usize> {
    for (i, b) in buttons.iter().enumerate() {
        if b.contains(px, py) { return Some(i); }
    }
    None
}

/// Render a single button with its current state into the framebuffer.
pub fn render(b: &Button) {
    use crate::theme;
    let (bg, fg) = match b.state {
        State::Normal  => (theme::BUTTON_BG, theme::BUTTON_TEXT),
        State::Hover   => (theme::BUTTON_HOVER, theme::TEXT),
        State::Pressed => (theme::ACCENT, theme::TEXT_ON_ACCENT),
    };
    crate::framebuffer::draw_button(b.x, b.y, b.w, b.h, b.label, bg, fg);
}

pub fn render_all(buttons: &[Button]) {
    for b in buttons { render(b); }
}

/// Describe what an action did, for echoing to the shell / status bar.
pub fn describe(a: Action) -> String {
    match a {
        Action::Clear  => String::from("[button] Clear"),
        Action::Help   => String::from("[button] Help"),
        Action::Reboot => String::from("[button] Reboot (double-click to confirm)"),
        Action::Shutdown => String::from("[button] Shutdown (double-click to confirm)"),
    }
}
