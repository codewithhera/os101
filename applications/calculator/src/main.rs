//! OS101 Calculator — simple four-function desktop calculator.
//!
//! Sequential evaluation (classic desk calculator): each binary op commits the
//! previous one. Supports decimal point, sign flip, backspace, and clear.

#![no_std]
#![no_main]

extern crate alloc;

use linked_list_allocator::LockedHeap;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

use os101_user::{
    exit, gui_add_button, gui_add_label, gui_create_window, gui_get_event,
    gui_update_widget, GuiEvent,
};
use alloc::format;
use alloc::string::String;

// ── Layout ─────────────────────────────────────────────────────────────────
const WIN_W: usize = 268;
const WIN_H: usize = 380;

const PAD: usize = 12;
const DISPLAY_H: usize = 52;

const BTN_W: usize = 54;
const BTN_H: usize = 42;
const BTN_GAP: usize = 8;

const GRID_TOP_Y: usize = PAD + DISPLAY_H + PAD;

// ── Action IDs (digits 0..=9 reserved) ─────────────────────────────────────
const A_DOT: u64 = 20;
const A_EQ: u64 = 21;
const A_CLEAR: u64 = 22;
const A_BACK: u64 = 23;
const A_SIGN: u64 = 24;
const A_DIV: u64 = 30;
const A_MUL: u64 = 31;
const A_SUB: u64 = 32;
const A_ADD: u64 = 33;

#[derive(Clone, Copy)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    fn apply(self, a: f64, b: f64) -> Result<f64, ()> {
        match self {
            BinOp::Add => Ok(a + b),
            BinOp::Sub => Ok(a - b),
            BinOp::Mul => Ok(a * b),
            BinOp::Div => {
                if b == 0.0 || b == -0.0 {
                    Err(())
                } else {
                    Ok(a / b)
                }
            }
        }
    }
}

/// Classic desk-calculator state: optional left-hand side + pending op, current
/// entry string, and whether the next digit replaces the display.
struct Calc {
    /// Left operand when a binary op is waiting for the right side.
    lhs: Option<f64>,
    pending: Option<BinOp>,
    /// What the user sees / is typing.
    entry: String,
    /// After `=` or an operator, the next digit starts a fresh number.
    new_entry: bool,
    error: bool,
}

impl Calc {
    fn new() -> Self {
        Self {
            lhs: None,
            pending: None,
            entry: String::from("0"),
            new_entry: true,
            error: false,
        }
    }

    fn parse_entry(&self) -> Option<f64> {
        self.entry.parse().ok()
    }

    fn set_error(&mut self) {
        self.entry.clear();
        self.entry.push_str("Error");
        self.lhs = None;
        self.pending = None;
        self.new_entry = true;
        self.error = true;
    }

    fn clear_all(&mut self) {
        *self = Self::new();
    }

    fn push_digit(&mut self, d: u8) {
        if self.error {
            return;
        }
        let c = (b'0' + d) as char;
        if self.new_entry {
            self.entry.clear();
            self.entry.push(c);
            self.new_entry = false;
            return;
        }
        if self.entry.chars().count() >= 14 {
            return;
        }
        if self.entry == "0" {
            self.entry.clear();
        }
        self.entry.push(c);
    }

    fn push_dot(&mut self) {
        if self.error {
            return;
        }
        if self.new_entry {
            self.entry.clear();
            self.entry.push_str("0.");
            self.new_entry = false;
            return;
        }
        if !self.entry.contains('.') && self.entry.chars().count() < 14 {
            self.entry.push('.');
        }
    }

    fn backspace(&mut self) {
        if self.error || self.new_entry {
            return;
        }
        self.entry.pop();
        if self.entry.is_empty() || self.entry == "-" {
            self.entry.clear();
            self.entry.push('0');
            self.new_entry = true;
        }
    }

    fn negate(&mut self) {
        if self.error {
            return;
        }
        if self.entry == "0" {
            return;
        }
        if self.entry.starts_with('-') {
            self.entry.remove(0);
        } else {
            self.entry.insert(0, '-');
        }
        if self.entry == "-0" {
            self.entry.clear();
            self.entry.push('0');
        }
    }

    /// User chose a binary operator.
    fn push_op(&mut self, op: BinOp) {
        if self.error {
            return;
        }
        let Some(rhs) = self.parse_entry() else {
            self.set_error();
            return;
        };

        if let (Some(a), Some(p)) = (self.lhs, self.pending) {
            match p.apply(a, rhs) {
                Ok(v) => {
                    self.entry = format_f64(v);
                    self.lhs = Some(v);
                    self.pending = Some(op);
                    self.new_entry = true;
                }
                Err(()) => self.set_error(),
            }
        } else {
            // First operator on this calculation, or odd recovery: stage lhs + op.
            self.lhs = Some(rhs);
            self.pending = Some(op);
            self.new_entry = true;
        }
    }

    fn equals(&mut self) {
        if self.error {
            return;
        }
        let Some(rhs) = self.parse_entry() else {
            self.set_error();
            return;
        };
        let Some(a) = self.lhs else {
            self.new_entry = true;
            return;
        };
        let Some(p) = self.pending else {
            self.new_entry = true;
            return;
        };

        match p.apply(a, rhs) {
            Ok(v) => {
                self.entry = format_f64(v);
                self.lhs = None;
                self.pending = None;
                self.new_entry = true;
            }
            Err(()) => self.set_error(),
        }
    }

    fn status_line(&self) -> String {
        if self.error {
            return String::from("Press C");
        }
        match (self.lhs, self.pending) {
            (Some(a), Some(op)) => {
                let op_ch = match op {
                    BinOp::Add => '+',
                    BinOp::Sub => '-',
                    BinOp::Mul => '*',
                    BinOp::Div => '/',
                };
                format!("{} {} ...", format_f64(a), op_ch)
            }
            _ => String::from(""),
        }
    }
}

fn format_f64(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        return String::from("Error");
    }
    let magnitude_ok = v > -1.0e14 && v < 1.0e14;
    if magnitude_ok {
        let as_i = v as i64;
        if (as_i as f64) == v {
            return format!("{}", as_i);
        }
    }
    let mut s = format!("{:.10}", v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s.chars().count() > 14 {
        return s.chars().take(14).collect();
    }
    s
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    static mut HEAP: [u8; 256 * 1024] = [0; 256 * 1024];
    unsafe {
        let ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        ALLOCATOR.lock().init(ptr, 256 * 1024);
    }

    let win = gui_create_window("Calculator", WIN_W, WIN_H);

    let display = gui_add_label(win, PAD, PAD + 10, "                   0");

    // Row 0: clear, backspace, sign, divide
    // ASCII labels so the kernel font always renders reliably.
    let grid: &[(usize, usize, u64, &str)] = &[
        (0, 0, A_CLEAR, "C"),
        (1, 0, A_BACK, "<-"),
        (2, 0, A_SIGN, "+/-"),
        (3, 0, A_DIV, "/"),
        (0, 1, 7, "7"),
        (1, 1, 8, "8"),
        (2, 1, 9, "9"),
        (3, 1, A_MUL, "*"),
        (0, 2, 4, "4"),
        (1, 2, 5, "5"),
        (2, 2, 6, "6"),
        (3, 2, A_SUB, "-"),
        (0, 3, 1, "1"),
        (1, 3, 2, "2"),
        (2, 3, 3, "3"),
        (3, 3, A_ADD, "+"),
    ];

    for &(col, row, action, label) in grid {
        let x = PAD + col * (BTN_W + BTN_GAP);
        let y = GRID_TOP_Y + row * (BTN_H + BTN_GAP);
        gui_add_button(win, x, y, BTN_W, BTN_H, label, action);
    }

    let row4 = 4usize;
    let y4 = GRID_TOP_Y + row4 * (BTN_H + BTN_GAP);
    let zero_w = BTN_W * 2 + BTN_GAP;
    gui_add_button(win, PAD, y4, zero_w, BTN_H, "0", 0);
    gui_add_button(
        win,
        PAD + zero_w + BTN_GAP,
        y4,
        BTN_W,
        BTN_H,
        ".",
        A_DOT,
    );
    gui_add_button(
        win,
        PAD + zero_w + BTN_GAP + BTN_W + BTN_GAP,
        y4,
        BTN_W,
        BTN_H,
        "=",
        A_EQ,
    );

    let footer_y = GRID_TOP_Y + 5 * (BTN_H + BTN_GAP) + 6;
    let status = gui_add_label(win, PAD, footer_y, "");

    let mut calc = Calc::new();
    let mut last_display = pad_right_align(&calc.entry, 20);
    let mut last_status = calc.status_line();
    gui_update_widget(win, display, &last_display);
    gui_update_widget(win, status, &last_status);

    loop {
        let mut changed = false;
        match gui_get_event(win) {
            GuiEvent::ButtonClicked { action_id } => {
                changed = true;
                match action_id {
                    0..=9 => calc.push_digit(action_id as u8),
                    A_DOT => calc.push_dot(),
                    A_BACK => calc.backspace(),
                    A_SIGN => calc.negate(),
                    A_CLEAR => calc.clear_all(),
                    A_DIV => calc.push_op(BinOp::Div),
                    A_MUL => calc.push_op(BinOp::Mul),
                    A_SUB => calc.push_op(BinOp::Sub),
                    A_ADD => calc.push_op(BinOp::Add),
                    A_EQ => calc.equals(),
                    _ => {}
                }
            }
            _ => {}
        }

        if changed {
            let new_display = pad_right_align(&calc.entry, 20);
            if new_display != last_display {
                gui_update_widget(win, display, &new_display);
                last_display = new_display;
            }
            let new_status = calc.status_line();
            if new_status != last_status {
                gui_update_widget(win, status, &new_status);
                last_status = new_status;
            }
        }

        os101_user::yield_now();
    }
}

fn pad_right_align(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        return String::from(s);
    }
    let mut out = String::with_capacity(width);
    for _ in 0..(width - n) {
        out.push(' ');
    }
    out.push_str(s);
    out
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    exit(1)
}
