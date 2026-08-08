//! Central colour palette — sharp, dark, futuristic.
//!
//! Flat panels, cyan accents, no soft pastels. The shell/console shares the
//! same dark base so the whole machine feels like one UI.

use crate::color::Color;

// ── Desktop ────────────────────────────────────────────────────────────────

pub const DESKTOP_TOP: Color = Color::hex(0x020617);
pub const DESKTOP_BOTTOM: Color = Color::hex(0x0F172A);

// ── Window chrome ──────────────────────────────────────────────────────────

pub const WINDOW_BG: Color = Color::hex(0x0B1220);
pub const WINDOW_BORDER: Color = Color::hex(0x1E293B);
pub const WINDOW_BORDER_ACTIVE: Color = Color::hex(0x22D3EE);
pub const WINDOW_SHADOW: Color = Color::hex(0x000000);

pub const TITLEBAR_ACTIVE: Color = Color::hex(0x083344);
pub const TITLEBAR_INACTIVE: Color = Color::hex(0x111827);
pub const TITLEBAR_TEXT: Color = Color::hex(0xECFEFF);
pub const TITLEBAR_TEXT_INACTIVE: Color = Color::hex(0x64748B);

pub const CLOSE_BUTTON: Color = Color::hex(0xF43F5E);
pub const CLOSE_BUTTON_HOVER: Color = Color::hex(0xFB7185);

// ── Text ───────────────────────────────────────────────────────────────────

pub const TEXT: Color = Color::hex(0xE2E8F0);
pub const TEXT_MUTED: Color = Color::hex(0x94A3B8);
pub const TEXT_ON_ACCENT: Color = Color::hex(0x020617);

// ── Controls ───────────────────────────────────────────────────────────────

pub const BUTTON_BG: Color = Color::hex(0x111827);
pub const BUTTON_HOVER: Color = Color::hex(0x164E63);
pub const BUTTON_PRESSED: Color = Color::hex(0x0891B2);
pub const BUTTON_BORDER: Color = Color::hex(0x22D3EE);
pub const BUTTON_TEXT: Color = Color::hex(0xECFEFF);

pub const ACCENT: Color = Color::hex(0x22D3EE);
pub const ACCENT_HOVER: Color = Color::hex(0x67E8F9);
pub const ACCENT_PRESSED: Color = Color::hex(0x06B6D4);

pub const FIELD_BG: Color = Color::hex(0x020617);
pub const FIELD_BORDER: Color = Color::hex(0x334155);
pub const FIELD_FOCUS: Color = Color::hex(0x22D3EE);

// ── Taskbar / status ───────────────────────────────────────────────────────

pub const TASKBAR_BG: Color = Color::hex(0x020617);
pub const TASKBAR_ITEM: Color = Color::hex(0x0F172A);
pub const TASKBAR_ITEM_ACTIVE: Color = Color::hex(0x164E63);
pub const TASKBAR_ACCENT: Color = Color::hex(0x22D3EE);
pub const STATUS_BG: Color = Color::hex(0x020617);
pub const STATUS_TEXT: Color = Color::hex(0xA5F3FC);

// ── Semantic ───────────────────────────────────────────────────────────────

pub const SUCCESS: Color = Color::hex(0x34D399);
pub const WARNING: Color = Color::hex(0xFBBF24);
pub const ERROR: Color = Color::hex(0xF43F5E);
pub const INFO: Color = Color::hex(0x38BDF8);

// ── Console / shell ────────────────────────────────────────────────────────

pub const CONSOLE_BG: Color = Color::hex(0x020617);
pub const CONSOLE_TEXT: Color = Color::hex(0xE2E8F0);
pub const CONSOLE_CURSOR: Color = Color::hex(0x22D3EE);

// ── Per-app accents ────────────────────────────────────────────────────────

pub const APP_ACCENTS: [Color; 8] = [
    Color::hex(0x22D3EE),
    Color::hex(0x34D399),
    Color::hex(0xA78BFA),
    Color::hex(0xF472B6),
    Color::hex(0xFBBF24),
    Color::hex(0x60A5FA),
    Color::hex(0xFB7185),
    Color::hex(0x2DD4BF),
];

#[inline]
pub fn app_accent(index: usize) -> Color {
    APP_ACCENTS[index % APP_ACCENTS.len()]
}
