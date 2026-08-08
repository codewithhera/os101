//! Phase 8–12 — Windowing, widgets, and GUI apps.
//!
//! A minimal window manager on top of the Phase 7 framebuffer. Windows live
//! in a z-ordered `Vec`; the last entry is the topmost/focused one. Each
//! window owns a small list of widgets plus optional per-app state (Calc
//! accumulator, Paint pixel list, Snake board, etc.).

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crossbeam_queue::SegQueue;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, get_raster_width};
use pc_keyboard::{DecodedKey, KeyCode};
use spin::Mutex;

use crate::framebuffer;
use crate::color::Color;
use crate::theme;

const LINE_HEIGHT: RasterHeight = RasterHeight::Size16;
const CHAR_W: usize = get_raster_width(FontWeight::Regular, LINE_HEIGHT);
const ROW_H: usize = LINE_HEIGHT.val();

pub const TITLEBAR_H: usize = 26;
pub const BORDER: usize = 2;
pub const CLOSE_BTN: usize = 14;
/// Gap between titlebar chrome buttons (min / mid / max / close).
const CHROME_GAP: i32 = 4;
/// Height of the per-window status/footer band drawn at the bottom of the
/// content area. Windows opt in via `with_footer(..)`; a `None` footer
/// suppresses the band entirely.
pub const FOOTER_H: usize = 20;

// ── Palette ─────────────────────────────────────────────────────────────────
// Sourced from `crate::theme` so the whole desktop re-themes from one file.
const WIN_CONTENT_BG: Color = theme::WINDOW_BG;
const WIN_TITLE_BG_FOCUS: Color = theme::TITLEBAR_ACTIVE;
const WIN_TITLE_BG_BLUR: Color = theme::TITLEBAR_INACTIVE;
const WIN_TITLE_FG: Color = theme::TITLEBAR_TEXT;
const WIN_BORDER: Color = theme::WINDOW_BORDER;
const WIN_TEXT: Color = theme::TEXT;
const BTN_BG: Color = theme::BUTTON_BG;
const BTN_BG_HOVER: Color = theme::BUTTON_HOVER;
const BTN_BG_PRESSED: Color = theme::ACCENT_PRESSED;
const BTN_FG: Color = theme::BUTTON_TEXT;
const FOOTER_BG: Color = Color::hex(0x020617);
const FOOTER_FG: Color = theme::TEXT_MUTED;
const ICON_OUTLINE: Color = Color::hex(0x0EA5E9);
const ICON_FILL: Color = Color::hex(0x0B1220);
const ICON_ACCENT: Color = theme::ACCENT;
const TB_BG: Color = theme::FIELD_BG;
const TB_BG_FOCUS: Color = Color::hex(0x083344);
const TB_TEXT: Color = theme::TEXT;
const TB_CURSOR: Color = theme::ACCENT;
const CANVAS_BG: Color = Color::hex(0xFFFFFF);
const CANVAS_INK: Color = Color::hex(0x0F172A);
const SNAKE_GRID_BG: Color = Color::hex(0x0C4A6E);
const SNAKE_BODY: Color = Color::hex(0x4ADE80);
const SNAKE_HEAD: Color = Color::hex(0xBBF7D0);
const SNAKE_FOOD: Color = Color::hex(0xFACC15);

// ── Terminal palette ──────────────────────────────────────────────────────────
const TERM_BG:       Color = theme::CONSOLE_BG;
const TERM_FG:       Color = theme::CONSOLE_TEXT;
const TERM_INPUT_BG: Color = Color::hex(0x141A24);
const TERM_PROMPT_FG: Color = theme::SUCCESS;
const TERM_PROMPT: &str    = "os101$";
// ── Desktop taskbar ───────────────────────────────────────────────────────────
const DESKBAR_H:     usize = 40;
const DESKBAR_BG:    Color = theme::TASKBAR_BG;
const DESKBAR_FG:    Color = theme::STATUS_TEXT;
const DESKBAR_BTN:   Color = theme::TASKBAR_ITEM;
const DESK_ICON_FG:  Color = Color::hex(0xECFEFF);
// Desktop icon tiles — large badges for a kid-friendly Plasma-like desktop.
const DESK_ICON_W:   usize = 112;
const DESK_ICON_H:   usize = 108;
const DESK_ICON_X0:  usize = 18;
const DESK_ICON_Y0:  usize = 18;
const DESK_ICON_GAP: usize = 14;
const DESK_ICON_DRAW_SIZE: usize = 48;

/// The icons on the desktop, top to bottom. The window kind supplies both the
/// badge colour and what a double-click opens.
const DESKTOP_ICONS: [(&str, &crate::icons::Icon, WindowKind); 5] = [
    ("My Computer", &crate::icons::COMPUTER, WindowKind::MyComputer),
    ("Files", &crate::icons::FOLDER, WindowKind::FileManager),
    ("Web", &crate::icons::GLOBE, WindowKind::Browser),
    ("Apps", &crate::icons::APPS, WindowKind::Launcher),
    ("Install", &crate::icons::DRIVE, WindowKind::Installer),
];

// ── Global state ────────────────────────────────────────────────────────────

static GUI_MODE: AtomicBool = AtomicBool::new(false);
static GUI_DIRTY: AtomicBool = AtomicBool::new(false);
/// Hover-only dirty: redraw the top window without a full desktop pass.
static GUI_HOVER_DIRTY: AtomicBool = AtomicBool::new(false);
static LAST_TICK_SEC: AtomicU64 = AtomicU64::new(u64::MAX);
/// Monotonic id for each spawned window. User ELF syscalls return this (not
/// a `Vec` index) so `raise()`-induced z-order shuffles do not break
/// `SYS_GUI_GET_EVENT` / `sleep_on_window` / `wake_on_window` matching.
static NEXT_WINDOW_ID: AtomicU64 = AtomicU64::new(1);

// User-toggleable settings, persisted across window open/close within a boot.
pub static CURSOR_BLINK: AtomicBool = AtomicBool::new(true);
pub static DARK_THEME: AtomicBool = AtomicBool::new(false);

pub static WM: Mutex<WindowManager> = Mutex::new(WindowManager::new());

static NOTEPAD_SCRATCH: Mutex<String> = Mutex::new(String::new());
static FM_PATH: Mutex<String> = Mutex::new(String::new());
/// Current folder for Save As / Open dialogs.
static FILE_DIALOG_PATH: Mutex<String> = Mutex::new(String::new());
/// Text waiting to be written by Save As (copied from Notepad).
static SAVE_AS_PENDING: Mutex<String> = Mutex::new(String::new());
/// Last path Notepad saved/loaded (shown in the header label).
static NOTEPAD_PATH: Mutex<String> = Mutex::new(String::new());
/// Code Editor's in-memory buffer, mirrored the same way Notepad mirrors
/// `NOTEPAD_SCRATCH` — kept outside the widget so a fresh window (or the
/// Save As dialog) can read/seed it without borrowing the window manager.
static CODE_SCRATCH: Mutex<String> = Mutex::new(String::new());
/// Last path the Code Editor saved/loaded (shown in the header label, and
/// where Build/Run write the compiled source).
static CODE_PATH: Mutex<String> = Mutex::new(String::new());
/// Ctrl+C/Ctrl+X/Ctrl+V clipboard for the Code Editor's text selection.
static TEXT_CLIPBOARD: Mutex<String> = Mutex::new(String::new());
/// Which app the Save As / Open dialog is currently acting on.
#[derive(Clone, Copy, PartialEq)]
enum FileDialogTarget {
    Notepad,
    CodeEditor,
}
static FILE_DIALOG_TARGET: Mutex<FileDialogTarget> = Mutex::new(FileDialogTarget::Notepad);

// File Explorer layout: widget indices 0=header, 1..=12=list rows,
// 13..=19=toolbar, 20..=21=preview, ≥22=context menu.
const FM_ROWS: usize = 12;
const FM_FILE_Y0: usize = 62;
const FM_ROW_H: usize = 18;
const FM_WIDGET_BASE: usize = 22;
const FM_ICON_X: usize = 10;
/// List text starts to the right of a 16×16 icon.
const FM_LIST_LABEL_X: usize = FM_ICON_X + crate::icons::ICON_SIZE + 4;
const FM_PREVIEW_TITLE: usize = 20;
const FM_PREVIEW_BODY: usize = 21;
/// Large-icons grid (Windows / macOS style).
const FM_LG_COLS: usize = 5;
const FM_LG_CELL_W: usize = 108;
const FM_LG_CELL_H: usize = 96;
const FM_LG_ICON: usize = 40;
const FM_LG_Y0: usize = 62;
const FM_LG_VISIBLE: usize = 10; // 5×2 above the preview band

/// Save As / Open dialog list.
const FD_ROWS: usize = 8;
const FD_LIST_Y0: usize = 56;
/// Dialog widgets: 0=loc, 1..=8=rows, 9=Up, 10=name box, 11=Save/Open, 12=Cancel.
const FD_NAME_BOX: usize = 10;

/// File Explorer view: list rows or large icons.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum FmViewMode {
    List,
    LargeIcons,
}

/// Save As / Open file dialog mode.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum FileDialogMode {
    Save,
    Open,
}

pub fn is_gui_mode() -> bool { GUI_MODE.load(Ordering::Acquire) }
pub fn request_redraw() { GUI_DIRTY.store(true, Ordering::Release); }
pub fn take_redraw_request() -> bool { GUI_DIRTY.swap(false, Ordering::AcqRel) }
fn request_hover_redraw() { GUI_HOVER_DIRTY.store(true, Ordering::Release); }
pub fn take_hover_redraw_request() -> bool { GUI_HOVER_DIRTY.swap(false, Ordering::AcqRel) }

pub fn cursor_blink_enabled() -> bool {
    CURSOR_BLINK.load(Ordering::Relaxed)
}

// ── Widgets ─────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum BtnState { Normal, Hover, Pressed }

#[derive(Copy, Clone)]
pub enum WinAction {
    None,
    Close,
    ClearTextbox,
    // File manager / terminal
    RefreshFiles,
    OpenReadme,
    /// Preview selected file or open selected folder (`/data` RAM disk).
    FmPreview,
    FmUp,
    FmNewFolder,
    FmNewFile,
    FmDelete,
    FmOpen,
    /// Switch File Explorer to list rows.
    FmViewList,
    /// Switch File Explorer to large icons.
    FmViewIcons,
    /// Make the selected picture the desktop wallpaper.
    FmSetWallpaper,
    /// Go back to the wallpaper the OS draws itself.
    ResetWallpaper,
    TerminalRun,
    // Notepad
    NotepadSave, NotepadLoad, NotepadClear,
    // Code Editor
    CodeNew, CodeOpen, CodeSave, CodeBuild, CodeRun,
    /// Save As dialog: confirm write.
    FileDialogSave,
    /// Open dialog: load selected file into Notepad.
    FileDialogOpen,
    /// Dismiss Save As / Open dialog.
    FileDialogCancel,
    /// Navigate up one folder in the file dialog.
    FileDialogUp,
    // Browser
    BrowserGo,
    BrowserBack,
    BrowserHome,
    BrowserScroll(i8),
    /// Jump the page to an absolute offset; `usize::MAX` means the bottom.
    BrowserScrollTo(usize),
    /// Search for pictures matching whatever is in the address bar.
    BrowserImages,
    /// Reload the page that is on screen.
    BrowserReload,
    /// Open the picture the context menu was raised over.
    BrowserOpenImage,
    /// Save that picture to `/disk/downloads`.
    BrowserSaveImage,
    /// Save it and make it the desktop wallpaper.
    BrowserImageWallpaper,
    /// Send the form the caret is in, which is what Enter in a field does.
    BrowserSubmit,
    // Paint
    PaintClear,
    /// Select palette entry `n` as the brush colour.
    PaintColor(u8),
    /// Select brush radius `n`.
    PaintBrush(u8),
    /// Select brush / eraser / fill.
    PaintTool(PaintTool),
    // Snake
    SnakeRestart,
    SnakeDir(i8, i8),
    // Breakout
    BreakoutRestart,
    BreakoutLeft,
    BreakoutRight,
    BreakoutStop,
    BreakoutFaster,
    BreakoutSlower,
    // ABC Fun
    AbcPick(u8),
    AbcNext,
    // Race Cars
    RacingRestart,
    RacingLeft,
    RacingRight,
    RacingFaster,
    RacingSlower,
    // Space Invaders
    InvadersRestart,
    InvadersLeft,
    InvadersRight,
    InvadersFire,
    InvadersFaster,
    InvadersSlower,
    // Settings
    ToggleCursorBlink, ToggleDarkTheme,
    // Calculator (built-in fallback)
    CalcDigit(u8), CalcOp(char), CalcEq, CalcClear, CalcSign,
    LaunchApp(u8),
    /// My Computer: open a drive in File Explorer.
    /// 0 = /fat (FAT32), 1 = /init (initramfs).
    OpenDrive(u8),
    /// Open the Applications (launcher) window.
    OpenApplications,
    /// Open the My Computer window.
    OpenMyComputer,
    /// Open the permanent OS installer wizard.
    OpenInstaller,
    /// Open the web browser.
    OpenBrowser,
    /// Power off (QEMU isa-debug-exit / halt).
    Shutdown,
    /// Permanent OS installer wizard.
    InstallerNext,
    InstallerBack,
    /// Pick target disk by index in the wizard's current list.
    InstallerPick(u8),
    /// Confirm wipe (textbox must contain ERASE) and start copying.
    InstallerStart,
    InstallerReboot,
    /// An action generated by a userspace application.
    User(u64),
    ExitGui,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiEvent {
    None,
    ButtonClicked { action_id: u64 },
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum WindowKind {
    Generic, Launcher,
    Calculator,
    FileManager, Terminal, Monitor,
    Notepad, Paint, Snake, Breakout, Abc, Racing, Invaders, ImageView, Settings, About,
    MyComputer, Browser, CodeEditor,
    /// Wipe a disk and copy this live system onto it permanently.
    Installer,
    /// Modal Save As / Open file dialog (not listed in the app launcher).
    FileDialog,
}

pub enum Widget {
    Label { x: usize, y: usize, text: String },
    Button {
        x: usize, y: usize, w: usize, h: usize,
        label: String,
        state: BtnState,
        action: WinAction,
        /// Optional 16×16 icon drawn left of the label (launcher tiles,
        /// desktop shortcuts, sidebar).
        icon: Option<&'static crate::icons::Icon>,
    },
    TextBox {
        x: usize, y: usize, w: usize, h: usize,
        text: String,
    },
    /// Multi-line input area — Enter inserts a newline; simple wrap-on-render.
    TextArea {
        x: usize, y: usize, w: usize, h: usize,
        text: String,
    },
    /// Multi-line input area rendered with C syntax highlighting. Unlike
    /// `TextArea`, editing has a real caret: click to place it, arrow keys
    /// (plus Home/End) move it, and typing/Backspace/Delete act at its
    /// position rather than always at the end of the buffer.
    CodeArea {
        x: usize, y: usize, w: usize, h: usize,
        text: String,
        /// Character index (not byte offset) into `text`, `0..=text.chars().count()`.
        cursor: usize,
        /// The other end of a selection, as a character index. `None` means
        /// no selection (just a caret); `Some(a) == Some(cursor)` also counts
        /// as none. The selected range is `min(a, cursor)..max(a, cursor)`.
        selection: Option<usize>,
    },
    /// Read-only, wrapped, multi-line text — build diagnostics / program
    /// output in the Code Editor. Never takes keyboard focus.
    OutputArea {
        x: usize, y: usize, w: usize, h: usize,
        text: String,
        error: bool,
    },
    /// Drawable region — painting is owned by the window's `AppState`.
    Canvas { x: usize, y: usize, w: usize, h: usize },
    Checkbox {
        x: usize, y: usize, w: usize, h: usize,
        label: String,
        checked: bool,
        action: WinAction,
    },
    /// A flat colour chip. Behaves like a button, but shows the colour it
    /// selects rather than a label — the Paint palette is a row of these.
    Swatch {
        x: usize, y: usize, w: usize, h: usize,
        color: Color,
        selected: bool,
        action: WinAction,
    },
}

impl Widget {
    pub fn label(x: usize, y: usize, text: &str) -> Self {
        Widget::Label { x, y, text: String::from(text) }
    }
    pub fn button(x: usize, y: usize, w: usize, h: usize, label: &str, action: WinAction) -> Self {
        Widget::Button {
            x, y, w, h,
            label: String::from(label),
            state: BtnState::Normal,
            action,
            icon: None,
        }
    }
    pub fn icon_button(
        x: usize, y: usize, w: usize, h: usize,
        label: &str, action: WinAction,
        icon: &'static crate::icons::Icon,
    ) -> Self {
        Widget::Button {
            x, y, w, h,
            label: String::from(label),
            state: BtnState::Normal,
            action,
            icon: Some(icon),
        }
    }
    pub fn textbox(x: usize, y: usize, w: usize, h: usize) -> Self {
        Widget::TextBox { x, y, w, h, text: String::new() }
    }
    pub fn textarea(x: usize, y: usize, w: usize, h: usize) -> Self {
        Widget::TextArea { x, y, w, h, text: String::new() }
    }
    pub fn code_area(x: usize, y: usize, w: usize, h: usize) -> Self {
        Widget::CodeArea { x, y, w, h, text: String::new(), cursor: 0, selection: None }
    }
    pub fn output_area(x: usize, y: usize, w: usize, h: usize) -> Self {
        Widget::OutputArea { x, y, w, h, text: String::new(), error: false }
    }
    pub fn canvas(x: usize, y: usize, w: usize, h: usize) -> Self {
        Widget::Canvas { x, y, w, h }
    }
    pub fn swatch(x: usize, y: usize, w: usize, h: usize, color: Color, action: WinAction) -> Self {
        Widget::Swatch { x, y, w, h, color, selected: false, action }
    }
    pub fn checkbox(x: usize, y: usize, w: usize, h: usize, label: &str, checked: bool, action: WinAction) -> Self {
        Widget::Checkbox {
            x, y, w, h,
            label: String::from(label),
            checked,
            action,
        }
    }

    fn bounds(&self) -> Option<(usize, usize, usize, usize)> {
        match self {
            Widget::Label { .. } => None,
            Widget::Button { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::TextBox { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::TextArea { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::CodeArea { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::OutputArea { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::Canvas { x, y, w, h } => Some((*x, *y, *w, *h)),
            Widget::Checkbox { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
            Widget::Swatch { x, y, w, h, .. } => Some((*x, *y, *w, *h)),
        }
    }
}

// ── Per-app state ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintTool {
    Brush,
    Eraser,
    /// Flood fill from the clicked pixel across the contiguous region of
    /// whatever colour was there.
    Fill,
}

/// Paint's colour palette. Index into this is what `WinAction::PaintColor`
/// carries.
pub const PAINT_PALETTE: [Color; 16] = [
    Color::hex(0x0F172A), Color::hex(0x64748B), Color::hex(0xCBD5E1), Color::hex(0xFFFFFF),
    Color::hex(0xDC2626), Color::hex(0xEA580C), Color::hex(0xF59E0B), Color::hex(0xFACC15),
    Color::hex(0x16A34A), Color::hex(0x10B981), Color::hex(0x06B6D4), Color::hex(0x0EA5E9),
    Color::hex(0x2563EB), Color::hex(0x7C3AED), Color::hex(0xDB2777), Color::hex(0xF472B6),
];

pub enum AppState {
    None,
    Calculator {
        lhs: Option<i64>,
        op: Option<char>,
        entry: String,
        new_entry: bool,
    },
    Browser {
        /// The loaded document and the script engine attached to it, or
        /// `None` before anything is loaded.
        session: Option<crate::browser::script::Session>,
        /// Vertical scroll offset into the page, in pixels.
        scroll: usize,
        /// Message shown in the footer: what is loading, or what went wrong.
        status: String,
        /// Previously visited URLs, for the Back button.
        history: Vec<String>,
        /// Where the current page came from, after redirects.
        current: String,
    },
    Paint {
        /// The picture itself, one `Color` per canvas pixel, row-major. This
        /// replaced a list of drawn points, which grew without bound and
        /// could only ever hold a single ink colour.
        canvas: Vec<Color>,
        cw: usize,
        ch: usize,
        drawing: bool,
        last: Option<(u16, u16)>,
        color: Color,
        /// Brush radius in pixels; the stroke is a filled circle of this size.
        brush: usize,
        tool: PaintTool,
    },
    Snake {
        grid_w: u8,
        grid_h: u8,
        snake: Vec<(i16, i16)>,
        dir: (i8, i8),
        pending_dir: (i8, i8),
        food: (i16, i16),
        game_over: bool,
        score: u32,
        rng: u32,
        last_step_ticks: u64,
    },
    Breakout(crate::kids::breakout::State),
    Abc(crate::kids::abc::State),
    Racing(crate::kids::racing::State),
    Invaders(crate::kids::invaders::State),
    ImageView,
    Settings,
    /// Linux-style interactive terminal: a scrollback buffer plus the line
    /// the user is currently typing. The canvas renders it all; there is no
    /// separate textbox/button widget.
    Terminal {
        /// Rendered history — each entry is one already-emitted line. May
        /// include user-echoed prompts (`os101$ help`) and command output.
        scrollback: Vec<String>,
        /// The line being typed (no prompt prefix).
        input: String,
        /// Cursor blink state, flipped every ~0.5s by `tick()`.
        blink_on: bool,
        /// Last tick at which we flipped the blink state.
        last_blink_tick: u64,
    },
    /// Built-in file explorer: listing, selection, context menu widget tail.
    FileManager {
        entries: Vec<String>,
        selected: Option<usize>,
        view_mode: FmViewMode,
    },
    /// Save As / Open picker used by Notepad.
    FileDialog {
        mode: FileDialogMode,
        entries: Vec<String>,
        selected: Option<usize>,
    },
    /// Multi-step permanent installer.
    Installer {
        /// 0 = welcome, 1 = pick target, 2 = type ERASE, 3 = done / error.
        step: u8,
        source: crate::install::DiskId,
        /// Parallel to the on-screen target buttons.
        targets: Vec<crate::install::DiskId>,
        selected: Option<usize>,
        status: String,
    },
}

// ── Window ──────────────────────────────────────────────────────────────────

pub struct Window {
    /// Stable handle returned from `sys_create_window` and used in GUI syscalls.
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    pub title: String,
    pub widgets: Vec<Widget>,
    pub kind: WindowKind,
    pub focused_widget: Option<usize>,
    /// Set when a text field has just been focused by a click, so the next
    /// thing typed replaces its contents instead of being appended.
    ///
    /// This is what "select all on click" amounts to without a selection
    /// model, and doing without it is worse than it sounds: clicking the
    /// address bar and typing would silently paste the query onto the end of
    /// the URL already there.
    pub replace_on_type: bool,
    pub app: AppState,
    pub user_events: SegQueue<GuiEvent>,
    /// Optional status text drawn in the FOOTER_H band at the bottom of the
    /// content area. Apps update this via `set_footer` (kernel-side) or
    /// the `SYS_GUI_SET_FOOTER` syscall (ELF side).
    pub footer: Option<String>,
    /// PID of the userspace process that created this window, or `None` for
    /// kernel built-ins. GUI syscalls refuse to touch a window they do not
    /// own, so one app cannot drive or read another app's UI.
    pub owner_pid: Option<u64>,
    /// Hidden to the taskbar until restored.
    pub minimized: bool,
    /// Filling the desktop area (minus the taskbar).
    pub maximized: bool,
    /// Geometry to restore after maximize / mid / minimize.
    pub restore: Option<(i32, i32, usize, usize)>,
}

impl Window {
    pub fn new(x: i32, y: i32, w: usize, h: usize, title: &str) -> Self {
        Self {
            id: 0,
            x, y, w, h,
            title: String::from(title),
            widgets: Vec::new(),
            kind: WindowKind::Generic,
            focused_widget: None,
            replace_on_type: false,
            app: AppState::None,
            user_events: SegQueue::new(),
            footer: None,
            owner_pid: None,
            minimized: false,
            maximized: false,
            restore: None,
        }
    }

    pub fn with_kind(mut self, kind: WindowKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn with_app(mut self, app: AppState) -> Self {
        self.app = app;
        self
    }

    pub fn with_footer(mut self, text: &str) -> Self {
        self.footer = Some(String::from(text));
        self
    }

    pub fn add(mut self, widget: Widget) -> Self {
        self.widgets.push(widget);
        self
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        if self.minimized {
            return false;
        }
        px >= self.x && px < self.x + self.w as i32
            && py >= self.y && py < self.y + self.h as i32
    }

    pub fn in_titlebar(&self, px: i32, py: i32) -> bool {
        self.contains(px, py) && py < self.y + TITLEBAR_H as i32
    }

    /// Titlebar chrome: close is rightmost; max / mid / min sit to its left.
    fn chrome_btn_rect(&self, index_from_right: i32) -> (i32, i32, usize, usize) {
        let step = CLOSE_BTN as i32 + CHROME_GAP;
        let x = self.x + self.w as i32 - CLOSE_BTN as i32 - 5 - index_from_right * step;
        let y = self.y + 5;
        (x, y, CLOSE_BTN, CLOSE_BTN)
    }

    pub fn close_btn_rect(&self) -> (i32, i32, usize, usize) {
        self.chrome_btn_rect(0)
    }

    pub fn max_btn_rect(&self) -> (i32, i32, usize, usize) {
        self.chrome_btn_rect(1)
    }

    pub fn mid_btn_rect(&self) -> (i32, i32, usize, usize) {
        self.chrome_btn_rect(2)
    }

    pub fn min_btn_rect(&self) -> (i32, i32, usize, usize) {
        self.chrome_btn_rect(3)
    }

    fn in_chrome_btn(rect: (i32, i32, usize, usize), px: i32, py: i32) -> bool {
        let (cx, cy, cw, ch) = rect;
        px >= cx && px < cx + cw as i32 && py >= cy && py < cy + ch as i32
    }

    pub fn in_close_btn(&self, px: i32, py: i32) -> bool {
        Self::in_chrome_btn(self.close_btn_rect(), px, py)
    }

    pub fn in_max_btn(&self, px: i32, py: i32) -> bool {
        Self::in_chrome_btn(self.max_btn_rect(), px, py)
    }

    pub fn in_mid_btn(&self, px: i32, py: i32) -> bool {
        Self::in_chrome_btn(self.mid_btn_rect(), px, py)
    }

    pub fn in_min_btn(&self, px: i32, py: i32) -> bool {
        Self::in_chrome_btn(self.min_btn_rect(), px, py)
    }

    fn save_restore_if_needed(&mut self) {
        if self.restore.is_none() && !self.maximized {
            self.restore = Some((self.x, self.y, self.w, self.h));
        }
    }

    pub fn apply_minimize(&mut self) {
        self.save_restore_if_needed();
        self.minimized = true;
        self.maximized = false;
    }

    pub fn apply_restore_mid(&mut self) {
        self.minimized = false;
        if let Some((x, y, w, h)) = self.restore.take() {
            self.x = x;
            self.y = y;
            self.w = w;
            self.h = h;
            self.maximized = false;
            layout_kids_playfield(self);
            return;
        }
        // No saved size — snap to a comfortable medium window.
        let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
        let desk_h = sh.saturating_sub(DESKBAR_H);
        self.w = (sw * 55 / 100).max(420).min(sw.saturating_sub(40));
        self.h = (desk_h * 60 / 100).max(320).min(desk_h.saturating_sub(20));
        self.x = ((sw.saturating_sub(self.w)) / 2) as i32;
        self.y = ((desk_h.saturating_sub(self.h)) / 3) as i32;
        self.maximized = false;
        layout_kids_playfield(self);
    }

    pub fn apply_maximize(&mut self) {
        self.minimized = false;
        if self.maximized {
            self.apply_restore_mid();
            return;
        }
        self.save_restore_if_needed();
        let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
        let desk_h = sh.saturating_sub(DESKBAR_H);
        self.x = 0;
        self.y = 0;
        self.w = sw;
        self.h = desk_h;
        self.maximized = true;
        layout_kids_playfield(self);
    }

    fn content_origin(&self) -> (i32, i32) {
        (self.x + BORDER as i32, self.y + TITLEBAR_H as i32)
    }

    fn widget_at(&self, sx: i32, sy: i32) -> Option<usize> {
        let (ox, oy) = self.content_origin();
        let lx = sx - ox;
        let ly = sy - oy;
        // Last to first: widgets are painted in order, so the later one is the
        // one on top. A context menu is appended to the list and sits over the
        // page canvas, and searching forwards would hand every click to the
        // canvas underneath it.
        for (i, wgt) in self.widgets.iter().enumerate().rev() {
            if let Some((wx, wy, ww, wh)) = wgt.bounds() {
                if lx >= wx as i32 && lx < (wx + ww) as i32
                   && ly >= wy as i32 && ly < (wy + wh) as i32 {
                    return Some(i);
                }
            }
        }
        None
    }
}

// ── Window Manager ──────────────────────────────────────────────────────────

pub struct WindowManager {
    windows: Vec<Window>,
    dragging: Option<(usize, i32, i32)>,
    last_mouse: (i32, i32),
    mouse_left_down: bool,
}

impl WindowManager {
    pub const fn new() -> Self {
        Self {
            windows: Vec::new(),
            dragging: None,
            last_mouse: (0, 0),
            mouse_left_down: false,
        }
    }

    pub fn clear(&mut self) {
        self.windows.clear();
        self.dragging = None;
    }

    pub fn spawn(&mut self, mut w: Window) {
        w.id = NEXT_WINDOW_ID.fetch_add(1, Ordering::Relaxed);
        self.windows.push(w);
    }

    fn topmost_at(&self, px: i32, py: i32) -> Option<usize> {
        for (i, w) in self.windows.iter().enumerate().rev() {
            if w.contains(px, py) { return Some(i); }
        }
        None
    }

    fn raise(&mut self, idx: usize) {
        if idx >= self.windows.len() {
            return;
        }
        self.windows[idx].minimized = false;
        let last = self.windows.len().saturating_sub(1);
        if idx < last {
            let w = self.windows.remove(idx);
            self.windows.push(w);
        }
    }

    pub fn topmost_idx(&self) -> Option<usize> {
        self.windows
            .iter()
            .enumerate()
            .rev()
            .find(|(_, w)| !w.minimized)
            .map(|(i, _)| i)
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

fn paint_desktop() {
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let dark = DARK_THEME.load(Ordering::Relaxed);
    let desktop_h = sh.saturating_sub(DESKBAR_H);
    crate::wallpaper::paint(sw, desktop_h, dark);

    for (i, (label, icon, kind)) in DESKTOP_ICONS.iter().enumerate() {
        draw_desktop_icon(
            DESK_ICON_X0,
            DESK_ICON_Y0 + i * (DESK_ICON_H + DESK_ICON_GAP),
            label,
            icon,
            crate::icons::accent_for_window_kind(*kind),
        );
    }
}

/// Sharp square badge + solid label plate (readable for kids).
fn draw_desktop_icon(
    x: usize,
    y: usize,
    label: &str,
    icon: &'static crate::icons::Icon,
    accent: Color,
) {
    let badge = DESK_ICON_DRAW_SIZE + 16;
    let badge_x = x + DESK_ICON_W.saturating_sub(badge) / 2;

    framebuffer::fill_rect(badge_x, y, badge, badge, Color::hex(0x111827));
    framebuffer::stroke_rect(badge_x, y, badge, badge, accent);
    framebuffer::stroke_rect(
        badge_x + 2,
        y + 2,
        badge.saturating_sub(4),
        badge.saturating_sub(4),
        accent.darken(35),
    );

    let icon_x = badge_x + badge.saturating_sub(DESK_ICON_DRAW_SIZE) / 2;
    let icon_y = y + badge.saturating_sub(DESK_ICON_DRAW_SIZE) / 2;
    crate::icons::draw_scaled(
        icon_x,
        icon_y,
        DESK_ICON_DRAW_SIZE,
        icon,
        accent,
        Color::hex(0x0B1220),
        accent.lighten(40),
    );

    // Solid label plate — easier for kids than transparent text.
    let lw = label.chars().count() * CHAR_W;
    let chip_w = (lw + 16).min(DESK_ICON_W);
    let chip_x = x + DESK_ICON_W.saturating_sub(chip_w) / 2;
    let chip_y = y + badge + 8;
    let chip_bg = Color::hex(0x0F172A);
    framebuffer::fill_rect(chip_x, chip_y, chip_w, ROW_H + 6, chip_bg);
    framebuffer::stroke_rect(chip_x, chip_y, chip_w, ROW_H + 6, accent.darken(20));
    let lx = chip_x + chip_w.saturating_sub(lw) / 2;
    draw_text(lx, chip_y + 3, label, DESK_ICON_FG, chip_bg, chip_w);
}

fn paint_taskbar() {
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let bar_y = sh.saturating_sub(DESKBAR_H);
    framebuffer::fill_rect(0, bar_y, sw, DESKBAR_H, DESKBAR_BG);
    framebuffer::fill_rect(0, bar_y, sw, 2, theme::TASKBAR_ACCENT);

    const APPS_BTN_W: usize = 148;
    let bh = DESKBAR_H.saturating_sub(8);
    let by = bar_y + 4;
    framebuffer::fill_rect(8, by, APPS_BTN_W, bh, Color::hex(0x164E63));
    framebuffer::stroke_rect(8, by, APPS_BTN_W, bh, theme::ACCENT);
    crate::icons::draw_scaled(
        18,
        by + bh.saturating_sub(20) / 2,
        20,
        &crate::icons::APPS,
        theme::ACCENT,
        Color::hex(0x0B1220),
        Color::hex(0xECFEFF),
    );
    let label = "Applications";
    let lw = label.chars().count() * CHAR_W;
    let ly = by + bh.saturating_sub(ROW_H) / 2;
    draw_text(44, ly, label, Color::hex(0xECFEFF), Color::hex(0x164E63), lw.min(APPS_BTN_W - 44));

    // Shutdown — solid power button next to Applications.
    const POWER_W: usize = 100;
    let px = 8 + APPS_BTN_W + 10;
    framebuffer::fill_rect(px, by, POWER_W, bh, Color::hex(0x7F1D1D));
    framebuffer::stroke_rect(px, by, POWER_W, bh, Color::hex(0xF43F5E));
    crate::icons::draw_scaled(
        px + 8,
        by + bh.saturating_sub(20) / 2,
        20,
        &crate::icons::POWER,
        Color::hex(0xFECACA),
        Color::hex(0x7F1D1D),
        Color::hex(0xF43F5E),
    );
    draw_text(px + 34, ly, "Shutdown", Color::hex(0xFECACA), Color::hex(0x7F1D1D), POWER_W - 38);

    // Minimized windows as solid taskbar tiles.
    let wm = WM.lock();
    let mut tx = px + POWER_W + 12;
    for win in wm.windows.iter() {
        if !win.minimized {
            continue;
        }
        let title: String = win.title.chars().take(10).collect();
        let tw = (title.chars().count() * CHAR_W + 42).max(88).min(156);
        if tx + tw + 160 > sw {
            break;
        }
        framebuffer::fill_rect(tx, by, tw, bh, theme::TASKBAR_ITEM_ACTIVE);
        framebuffer::stroke_rect(tx, by, tw, bh, theme::ACCENT);
        let icon = crate::icons::for_window_kind(win.kind);
        crate::icons::draw_scaled(
            tx + 7,
            by + bh.saturating_sub(20) / 2,
            20,
            icon,
            ICON_OUTLINE,
            ICON_FILL,
            crate::icons::accent_for_window_kind(win.kind),
        );
        draw_text(tx + 33, ly, &title, DESKBAR_FG, theme::TASKBAR_ITEM_ACTIVE, tw.saturating_sub(38));
        tx += tw + 8;
    }
    drop(wm);

    let t = crate::clock::ticks();
    let secs = t / 18;
    let h = secs / 3600;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let clock = alloc::format!(
        "{}  ·  {:02}:{:02}:{:02}",
        crate::rtc::clock_text(),
        h,
        m,
        s
    );
    let cw = clock.chars().count() * CHAR_W;
    let cx = sw.saturating_sub(cw + 18);
    let cy = bar_y + DESKBAR_H.saturating_sub(ROW_H) / 2;
    framebuffer::fill_rect(cx - 10, by, cw + 20, bh, DESKBAR_BTN);
    framebuffer::stroke_rect(cx - 10, by, cw + 20, bh, theme::WINDOW_BORDER);
    draw_text(cx, cy, &clock, DESKBAR_FG, DESKBAR_BTN, cw + 4);
}

/// Which minimized taskbar tile was clicked, if any.
fn taskbar_minimized_at(px: i32, py: i32) -> Option<usize> {
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let bar_y = sh.saturating_sub(DESKBAR_H) as i32;
    let bh = DESKBAR_H.saturating_sub(8) as i32;
    let by = bar_y + 4;
    if py < by || py >= by + bh {
        return None;
    }
    const APPS_BTN_W: usize = 148;
    let mut tx = 8 + APPS_BTN_W + 10 + 100 + 12;
    let wm = WM.lock();
    for (i, win) in wm.windows.iter().enumerate() {
        if !win.minimized {
            continue;
        }
        let title_len = win.title.chars().take(10).count();
        let tw = (title_len * CHAR_W + 42).max(88).min(156) as i32;
        if tx + tw as usize + 160 > sw {
            break;
        }
        if px >= tx as i32 && px < tx as i32 + tw {
            return Some(i);
        }
        tx += tw as usize + 8;
    }
    None
}

/// Which desktop icon was clicked, or None. Indices match [`DESKTOP_ICONS`].
fn desktop_icon_at(px: i32, py: i32) -> Option<usize> {
    if px < 0 || py < 0 { return None; }
    let x = px as usize;
    let y = py as usize;
    if x < DESK_ICON_X0 || x > DESK_ICON_X0 + DESK_ICON_W { return None; }
    for i in 0..DESKTOP_ICONS.len() {
        let top = DESK_ICON_Y0 + i * (DESK_ICON_H + DESK_ICON_GAP);
        if y >= top && y < top + DESK_ICON_H {
            return Some(i);
        }
    }
    None
}

/// True when (px,py) is inside the taskbar "Applications" button.
fn taskbar_apps_btn_at(px: i32, py: i32) -> bool {
    let (_, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let bar_y = sh.saturating_sub(DESKBAR_H) as i32;
    py >= bar_y + 4 && py < bar_y + DESKBAR_H as i32 && px >= 8 && px < 8 + 148
}

/// True when (px,py) is inside the taskbar Shutdown button.
fn taskbar_shutdown_btn_at(px: i32, py: i32) -> bool {
    let (_, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let bar_y = sh.saturating_sub(DESKBAR_H) as i32;
    let x0 = 8 + 148 + 10;
    py >= bar_y + 4 && py < bar_y + DESKBAR_H as i32 && px >= x0 && px < x0 + 100
}

fn draw_text(x: usize, y: usize, s: &str, fg: impl Into<Color>, bg: impl Into<Color>, max_w: usize) {
    framebuffer::draw_text_blended(x, y, s, fg, bg, max_w);
}

fn render_widget(origin: (i32, i32), content_w: usize, widget: &Widget, focused: bool, paper: bool) {
    let (ox, oy) = origin;
    match widget {
        Widget::Label { x, y, text } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let max_w = content_w.saturating_sub(*x + 4);
            draw_text(sx, sy, text, WIN_TEXT, WIN_CONTENT_BG, max_w);
        }
        Widget::Button { x, y, w, h, label, state, icon, .. } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let bg = match state {
                BtnState::Normal => BTN_BG,
                BtnState::Hover => BTN_BG_HOVER,
                BtnState::Pressed => BTN_BG_PRESSED,
            };
            let border = match state {
                BtnState::Normal => theme::BUTTON_BORDER.darken(30),
                BtnState::Hover => theme::BUTTON_BORDER,
                BtnState::Pressed => theme::ACCENT_PRESSED.lighten(20),
            };
            framebuffer::fill_rect(sx, sy, *w, *h, bg);
            framebuffer::stroke_rect(sx, sy, *w, *h, border);
            // Left accent bar on hover/press for a sharp futuristic cue.
            if *state != BtnState::Normal {
                framebuffer::fill_rect(sx, sy, 2, *h, theme::ACCENT);
            }
            let icon_room = icon.is_some() && *h >= 20 && *w >= 16 + 8 + CHAR_W;
            if icon_room {
                let glyph = if *h >= 56 { 28usize } else if *h >= 40 { 22usize } else { crate::icons::ICON_SIZE };
                let icon_x = sx + 10;
                let icon_y = sy + h.saturating_sub(glyph) / 2;
                if let Some(ic) = icon {
                    let accent = crate::icons::accent_for_icon(ic);
                    let pad = 3usize;
                    let badge = glyph + pad * 2;
                    framebuffer::fill_rect(
                        icon_x.saturating_sub(pad),
                        icon_y.saturating_sub(pad),
                        badge,
                        badge,
                        accent.darken(30),
                    );
                    framebuffer::stroke_rect(
                        icon_x.saturating_sub(pad),
                        icon_y.saturating_sub(pad),
                        badge,
                        badge,
                        accent,
                    );
                    crate::icons::draw_scaled(
                        icon_x,
                        icon_y,
                        glyph,
                        ic,
                        accent,
                        Color::hex(0x0B1220),
                        accent.lighten(40),
                    );
                }
                let text_left = icon_x + glyph + 12;
                let text_right = sx + w.saturating_sub(8);
                let text_w = text_right.saturating_sub(text_left);
                let ly = sy + h.saturating_sub(ROW_H) / 2;
                draw_text(text_left, ly, label, BTN_FG, bg, text_w);
            } else {
                let lw = label.chars().count() * CHAR_W;
                let lx = sx + w.saturating_sub(lw) / 2;
                let ly = sy + h.saturating_sub(ROW_H) / 2;
                draw_text(lx, ly, label, BTN_FG, bg, w.saturating_sub(8));
            }
        }
        Widget::TextBox { x, y, w, h, text } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let bg = if focused { TB_BG_FOCUS } else { TB_BG };
            framebuffer::fill_rect(sx, sy, *w, *h, bg);
            framebuffer::stroke_rect(sx, sy, *w, *h, WIN_BORDER);
            let max_chars = (w.saturating_sub(8)) / CHAR_W;
            let shown: String = text.chars().rev().take(max_chars).collect::<String>()
                .chars().rev().collect();
            let tx = sx + 4;
            let ty = sy + h.saturating_sub(ROW_H) / 2;
            draw_text(tx, ty, &shown, TB_TEXT, bg, w.saturating_sub(8));
            if focused {
                let cursor_x = tx + shown.chars().count() * CHAR_W;
                framebuffer::fill_rect(cursor_x, ty, 1, ROW_H, TB_CURSOR);
            }
        }
        Widget::TextArea { x, y, w, h, text } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            // Notepad uses a white "paper" page; other textareas keep the field theme.
            let (bg, fg, border) = if paper {
                let paper_bg = Color::hex(0xFFFDF5);
                let paper_fg = Color::hex(0x1E293B);
                let paper_border = if focused { theme::ACCENT } else { Color::hex(0xCBD5E1) };
                (paper_bg, paper_fg, paper_border)
            } else {
                let bg = if focused { TB_BG_FOCUS } else { TB_BG };
                (bg, TB_TEXT, WIN_BORDER)
            };
            framebuffer::fill_rect(sx, sy, *w, *h, bg);
            framebuffer::stroke_rect(sx, sy, *w, *h, border);
            if paper {
                let mut rule_y = sy + 4 + ROW_H + 2;
                while rule_y + 1 < sy + *h {
                    framebuffer::fill_rect(sx + 4, rule_y, w.saturating_sub(8), 1, Color::hex(0xE2E8F0));
                    rule_y += ROW_H + 2;
                }
                framebuffer::fill_rect(sx + 28, sy + 4, 1, h.saturating_sub(8), Color::hex(0xFCA5A5));
            }

            let pad_left = if paper { 34usize } else { 4 };
            let inner_w = w.saturating_sub(pad_left + 4);
            let text_x = sx + pad_left;
            let max_chars = inner_w / CHAR_W.max(1);
            let line_step = ROW_H + 2;
            let mut line_y = sy + 4;
            let max_y = sy + h.saturating_sub(4);

            let mut last_line_chars: usize = 0;
            let mut last_line_y = line_y;
            for raw in text.split('\n') {
                let chars: Vec<char> = raw.chars().collect();
                if chars.is_empty() {
                    if line_y + ROW_H <= max_y {
                        last_line_chars = 0;
                        last_line_y = line_y;
                        line_y += line_step;
                    }
                    continue;
                }
                let mut i = 0;
                while i < chars.len() {
                    if line_y + ROW_H > max_y { break; }
                    let end = (i + max_chars).min(chars.len());
                    let chunk: String = chars[i..end].iter().collect();
                    draw_text(text_x, line_y, &chunk, fg, bg, inner_w);
                    last_line_chars = chunk.chars().count();
                    last_line_y = line_y;
                    line_y += line_step;
                    i = end;
                }
                if line_y + ROW_H > max_y { break; }
            }

            if focused {
                let cursor_x = text_x + last_line_chars * CHAR_W;
                if cursor_x + 1 < sx + w - 2 {
                    let cur = if paper { Color::hex(0x0F172A) } else { TB_CURSOR };
                    framebuffer::fill_rect(cursor_x, last_line_y, 2, ROW_H, cur);
                }
            }
        }
        Widget::CodeArea { x, y, w, h, text, cursor, selection } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let bg = Color::hex(0x1E1E2E);
            let sel_bg = Color::hex(0x264F78);
            let border = if focused { theme::ACCENT } else { WIN_BORDER };
            framebuffer::fill_rect(sx, sy, *w, *h, bg);
            framebuffer::stroke_rect(sx, sy, *w, *h, border);

            let colors = crate::highlight::colorize(text);
            let chars: Vec<char> = text.chars().collect();
            let pad_left = 6usize;
            let inner_w = w.saturating_sub(pad_left + 4);
            let text_x = sx + pad_left;
            let max_chars = code_area_max_chars(*w);
            let line_step = ROW_H + 2;
            let top = sy + 4;
            let max_y = sy + h.saturating_sub(4);
            let visible_rows = (max_y.saturating_sub(top) / line_step.max(1)).max(1);

            let rows = wrap_rows(&chars, max_chars);
            let cursor = (*cursor).min(chars.len());
            let (cursor_row, cursor_col) = cursor_row_col(&rows, cursor);
            // Scroll so the caret's row is always on screen — a code buffer
            // can easily grow past what a fixed-height pane can show at once.
            let first_row = cursor_row.saturating_sub(visible_rows.saturating_sub(1));

            let sel_range = selection
                .map(|a| a.min(chars.len()))
                .filter(|&a| a != cursor)
                .map(|a| (a.min(cursor), a.max(cursor)));

            let mut cursor_screen: Option<(usize, usize)> = None;
            for (i, &(start, end)) in rows.iter().enumerate().skip(first_row).take(visible_rows) {
                let line_y = top + (i - first_row) * line_step;
                // Selection highlight for this row, in row-local columns —
                // drawn as separate multicolor calls per segment since
                // `draw_text_multicolor` always paints one uniform `bg` per
                // call (see its doc comment).
                let sel_cols = sel_range.and_then(|(lo, hi)| {
                    let s = lo.max(start);
                    let e = hi.min(end);
                    if s < e { Some((s - start, e - start)) } else { None }
                });
                match sel_cols {
                    None => {
                        let line_buf: Vec<(char, Color)> =
                            (start..end).map(|j| (chars[j], colors[j])).collect();
                        framebuffer::draw_text_multicolor(text_x, line_y as isize, &line_buf, bg, inner_w);
                    }
                    Some((sc, ec)) => {
                        if sc > 0 {
                            let seg: Vec<(char, Color)> =
                                (start..start + sc).map(|j| (chars[j], colors[j])).collect();
                            framebuffer::draw_text_multicolor(text_x, line_y as isize, &seg, bg, inner_w);
                        }
                        let seg: Vec<(char, Color)> =
                            (start + sc..start + ec).map(|j| (chars[j], colors[j])).collect();
                        framebuffer::draw_text_multicolor(
                            text_x + sc * CHAR_W,
                            line_y as isize,
                            &seg,
                            sel_bg,
                            inner_w.saturating_sub(sc * CHAR_W),
                        );
                        if start + ec < end {
                            let seg: Vec<(char, Color)> =
                                (start + ec..end).map(|j| (chars[j], colors[j])).collect();
                            framebuffer::draw_text_multicolor(
                                text_x + ec * CHAR_W,
                                line_y as isize,
                                &seg,
                                bg,
                                inner_w.saturating_sub(ec * CHAR_W),
                            );
                        }
                    }
                }
                if i == cursor_row {
                    cursor_screen = Some((text_x + cursor_col * CHAR_W, line_y));
                }
            }

            if focused {
                if let Some((cx, cy)) = cursor_screen {
                    if cx + 1 < sx + w.saturating_sub(2) {
                        framebuffer::fill_rect(cx, cy, 2, ROW_H, Color::hex(0xE2E8F0));
                    }
                }
            }
        }
        Widget::OutputArea { x, y, w, h, text, error } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let bg = Color::hex(0x0B1220);
            let fg = if *error { Color::hex(0xF87171) } else { Color::hex(0x86EFAC) };
            framebuffer::fill_rect(sx, sy, *w, *h, bg);
            framebuffer::stroke_rect(sx, sy, *w, *h, WIN_BORDER);

            let pad_left = 6usize;
            let inner_w = w.saturating_sub(pad_left + 4);
            let text_x = sx + pad_left;
            let max_chars = (inner_w / CHAR_W.max(1)).max(1);
            let line_step = ROW_H + 2;
            let mut line_y = sy + 4;
            let max_y = sy + h.saturating_sub(4);

            'outer2: for raw in text.split('\n') {
                let chars: Vec<char> = raw.chars().collect();
                if chars.is_empty() {
                    if line_y + ROW_H > max_y { break 'outer2; }
                    line_y += line_step;
                    continue;
                }
                let mut i = 0;
                while i < chars.len() {
                    if line_y + ROW_H > max_y { break 'outer2; }
                    let end = (i + max_chars).min(chars.len());
                    let chunk: String = chars[i..end].iter().collect();
                    draw_text(text_x, line_y, &chunk, fg, bg, inner_w);
                    line_y += line_step;
                    i = end;
                }
            }
        }
        Widget::Canvas { x, y, w, h } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            framebuffer::fill_rect(sx, sy, *w, *h, CANVAS_BG);
            framebuffer::stroke_rect(sx, sy, *w, *h, WIN_BORDER);
        }
        Widget::Swatch { x, y, w, h, color, selected, .. } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            framebuffer::fill_rect(sx, sy, *w, *h, *color);
            if *selected {
                // A bright double ring reads as "selected" against both very
                // dark and very light chips.
                framebuffer::stroke_rect(sx, sy, *w, *h, theme::TEXT);
                framebuffer::stroke_rect(sx + 1, sy + 1, w.saturating_sub(2), h.saturating_sub(2),
                                         theme::ACCENT);
            } else {
                framebuffer::stroke_rect(sx, sy, *w, *h, WIN_BORDER);
            }
        }
        Widget::Checkbox { x, y, w, h, label, checked, .. } => {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            let box_side = 16usize;
            let bx = sx;
            let by = sy + h.saturating_sub(box_side) / 2;
            framebuffer::fill_rect(bx, by, box_side, box_side, TB_BG);
            framebuffer::stroke_rect(bx, by, box_side, box_side, WIN_BORDER);
            if *checked {
                framebuffer::draw_line(
                    (bx + 3) as i32, (by + 8) as i32,
                    (bx + 7) as i32, (by + 12) as i32, BTN_FG);
                framebuffer::draw_line(
                    (bx + 7) as i32, (by + 12) as i32,
                    (bx + 13) as i32, (by + 3) as i32, BTN_FG);
            }
            let tx = sx + box_side + 8;
            let ty = sy + h.saturating_sub(ROW_H) / 2;
            draw_text(tx, ty, label, WIN_TEXT, WIN_CONTENT_BG, w.saturating_sub(box_side + 12));
        }
    }
}

fn render_window(win: &Window, is_top: bool) {
    if win.x + win.w as i32 <= 0 || win.y + win.h as i32 <= 0 { return; }
    let sx = win.x.max(0) as usize;
    let sy = win.y.max(0) as usize;
    // Sharp rect chrome — no corner radius. Active window gets a cyan border.
    if is_top {
        framebuffer::fill_rect(sx + 3, sy + 3, win.w, win.h, theme::WINDOW_SHADOW);
    }
    framebuffer::fill_rect(sx, sy, win.w, win.h, WIN_CONTENT_BG);
    let tb_bg = if is_top { WIN_TITLE_BG_FOCUS } else { WIN_TITLE_BG_BLUR };
    let accent = crate::icons::accent_for_window_kind(win.kind);
    let (tb_top, tb_bottom) = if is_top {
        (tb_bg.lerp(accent, 35).lighten(4), tb_bg.darken(6))
    } else {
        (tb_bg, tb_bg.darken(4))
    };
    framebuffer::fill_vgradient(sx, sy, win.w, TITLEBAR_H, tb_top, tb_bottom);
    framebuffer::fill_rect(sx, sy + TITLEBAR_H, win.w, 1, theme::ACCENT.darken(if is_top { 0 } else { 50 }));
    let border = if is_top { theme::WINDOW_BORDER_ACTIVE } else { WIN_BORDER };
    framebuffer::stroke_rect(sx, sy, win.w, win.h, border);

    // Titlebar icon (left edge, 16×16 centred vertically in the titlebar).
    let icon = crate::icons::for_window_kind(win.kind);
    let icon_x = sx + 5;
    let icon_y = sy + TITLEBAR_H.saturating_sub(crate::icons::ICON_SIZE) / 2;
    crate::icons::draw(
        icon_x, icon_y, icon,
        ICON_OUTLINE, ICON_FILL, crate::icons::accent_for_window_kind(win.kind),
    );

    let title_left = icon_x + crate::icons::ICON_SIZE + 6;
    let chrome_w = 4 * CLOSE_BTN + 3 * CHROME_GAP as usize + 12;
    let title_right = sx + win.w.saturating_sub(chrome_w);
    let title_w = title_right.saturating_sub(title_left);
    let title_y = sy + (TITLEBAR_H.saturating_sub(ROW_H)) / 2;
    let title_fg = if is_top { WIN_TITLE_FG } else { theme::TITLEBAR_TEXT_INACTIVE };
    draw_text(title_left, title_y, &win.title, title_fg, tb_bg, title_w);

    // Chrome buttons: min / mid / max / close (left → right).
    let draw_chrome = |rect: (i32, i32, usize, usize), bg: Color, glyph: &dyn Fn(usize, usize, usize, usize)| {
        if rect.0 < 0 || rect.1 < 0 {
            return;
        }
        let (cxs, cys, cw, ch) = (rect.0 as usize, rect.1 as usize, rect.2, rect.3);
        framebuffer::fill_rect(cxs, cys, cw, ch, bg);
        framebuffer::stroke_rect(cxs, cys, cw, ch, bg.darken(30));
        glyph(cxs, cys, cw, ch);
    };
    let glyph_fg = Color::hex(0xECFEFF);
    draw_chrome(win.min_btn_rect(), Color::hex(0x334155), &|x, y, w, h| {
        // Minimize: short horizontal bar.
        framebuffer::fill_rect(x + 3, y + h / 2, w.saturating_sub(6), 2, glyph_fg);
    });
    draw_chrome(win.mid_btn_rect(), Color::hex(0x334155), &|x, y, w, h| {
        // Mid / restore: overlapping squares look like "normal size".
        framebuffer::stroke_rect(x + 3, y + 4, w.saturating_sub(7), h.saturating_sub(7), glyph_fg);
        framebuffer::stroke_rect(x + 5, y + 2, w.saturating_sub(7), h.saturating_sub(7), glyph_fg);
    });
    draw_chrome(win.max_btn_rect(), Color::hex(0x334155), &|x, y, w, h| {
        // Maximize: single square.
        framebuffer::stroke_rect(x + 3, y + 3, w.saturating_sub(6), h.saturating_sub(6), glyph_fg);
    });
    draw_chrome(win.close_btn_rect(), theme::CLOSE_BUTTON, &|x, y, w, h| {
        framebuffer::draw_line(
            (x + 2) as i32, (y + 2) as i32,
            (x + w - 3) as i32, (y + h - 3) as i32, glyph_fg);
        framebuffer::draw_line(
            (x + w - 3) as i32, (y + 2) as i32,
            (x + 2) as i32, (y + h - 3) as i32, glyph_fg);
    });

    let origin = win.content_origin();
    let content_w = win.w.saturating_sub(BORDER * 2);
    // The app paints into its canvas, over whatever was drawn there first, so
    // widgets are split around it: chrome underneath, and anything added after
    // the canvas — a context menu — on top, where it can be seen and clicked.
    let overlay = win
        .widgets
        .iter()
        .position(|w| matches!(w, Widget::Canvas { .. }))
        .map(|i| i + 1)
        .unwrap_or(win.widgets.len());
    let draw = |range: core::ops::Range<usize>| {
        let paper = win.kind == WindowKind::Notepad;
        for i in range {
            let focused = win.focused_widget == Some(i);
            render_widget(origin, content_w, &win.widgets[i], focused, paper);
        }
    };
    draw(0..overlay);
    render_app(win);
    draw(overlay..win.widgets.len());

    // Soft ruled-paper cream for notepad status bar.
    let footer_bg = if win.kind == WindowKind::Notepad {
        Color::hex(0xF1F5F9)
    } else {
        FOOTER_BG
    };
    let footer_fg = if win.kind == WindowKind::Notepad {
        Color::hex(0x334155)
    } else {
        FOOTER_FG
    };
    if let Some(footer) = &win.footer {
        if win.h > TITLEBAR_H + FOOTER_H + BORDER {
            let fsx = sx + BORDER;
            let fsy = sy + win.h - FOOTER_H - BORDER;
            let fw = win.w.saturating_sub(BORDER * 2);
            framebuffer::fill_rect(fsx, fsy, fw, FOOTER_H, footer_bg);
            framebuffer::draw_line(
                fsx as i32, fsy as i32,
                (fsx + fw) as i32 - 1, fsy as i32,
                if win.kind == WindowKind::Notepad { Color::hex(0xCBD5E1) } else { WIN_BORDER },
            );
            let ty = fsy + FOOTER_H.saturating_sub(ROW_H) / 2;
            draw_text(fsx + 8, ty, footer, footer_fg, footer_bg, fw.saturating_sub(16));
        }
    }
}

fn find_canvas_screen(win: &Window) -> Option<(usize, usize, usize, usize)> {
    let (ox, oy) = win.content_origin();
    for wgt in &win.widgets {
        if let Widget::Canvas { x, y, w, h } = wgt {
            let sx = (ox + *x as i32).max(0) as usize;
            let sy = (oy + *y as i32).max(0) as usize;
            return Some((sx, sy, *w, *h));
        }
    }
    None
}

fn render_app(win: &Window) {
    match &win.app {
        AppState::Browser { .. } => render_browser(win),
        AppState::Paint { canvas, cw: bw, ch: bh, .. } => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                framebuffer::blit(cx, cy, (*bw).min(cw), (*bh).min(ch), canvas, *bw);
            }
        }
        AppState::Snake { grid_w, grid_h, snake, food, game_over, .. } => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                let gw = *grid_w as usize;
                let gh = *grid_h as usize;
                let cell_w = cw / gw.max(1);
                let cell_h = ch / gh.max(1);
                framebuffer::fill_rect(cx, cy, cw, ch, SNAKE_GRID_BG);
                framebuffer::stroke_rect(cx, cy, cw, ch, WIN_BORDER);
                // Food
                if food.0 >= 0 && food.1 >= 0 {
                    let fx = cx + food.0 as usize * cell_w + 2;
                    let fy = cy + food.1 as usize * cell_h + 2;
                    framebuffer::fill_rect(
                        fx, fy,
                        cell_w.saturating_sub(4),
                        cell_h.saturating_sub(4),
                        SNAKE_FOOD,
                    );
                }
                // Snake
                for (i, (gx, gy)) in snake.iter().enumerate() {
                    if *gx < 0 || *gy < 0 { continue; }
                    let sx = cx + *gx as usize * cell_w + 1;
                    let sy = cy + *gy as usize * cell_h + 1;
                    let color = if i == 0 { SNAKE_HEAD } else { SNAKE_BODY };
                    framebuffer::fill_rect(
                        sx, sy,
                        cell_w.saturating_sub(2),
                        cell_h.saturating_sub(2),
                        color,
                    );
                }
                if *game_over {
                    let msg = "Oh no!  Tap Restart";
                    let mx = cx + 24;
                    let my = cy + ch / 2 - ROW_H / 2;
                    draw_text(mx, my, msg, Color::hex(0xFDE68A), SNAKE_GRID_BG, cw.saturating_sub(40));
                }
            }
        }
        AppState::Breakout(state) => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                state.render(cx, cy, cw, ch);
            }
        }
        AppState::Abc(state) => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                state.render(cx, cy, cw, ch);
            }
        }
        AppState::Racing(state) => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                state.render(cx, cy, cw, ch);
            }
        }
        AppState::Invaders(state) => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                state.render(cx, cy, cw, ch);
            }
        }
        AppState::Terminal { scrollback, input, blink_on, .. } => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                // Solid dark background like a real terminal.
                framebuffer::fill_rect(cx, cy, cw, ch, TERM_BG);
                let pad = 6usize;
                let line_step = ROW_H + 2;
                let inner_w = cw.saturating_sub(pad * 2);
                let max_cols = (inner_w / CHAR_W.max(1)).max(1);
                let mut lines: Vec<String> = Vec::new();
                // Wrap each scrollback line to the viewport width.
                for s in scrollback.iter() {
                    if s.is_empty() {
                        lines.push(String::new());
                        continue;
                    }
                    let chars: Vec<char> = s.chars().collect();
                    let mut i = 0;
                    while i < chars.len() {
                        let end = (i + max_cols).min(chars.len());
                        lines.push(chars[i..end].iter().collect());
                        i = end;
                    }
                }
                // Add the prompt + current input as the final line(s).
                let prompt = alloc::format!("{} {}", TERM_PROMPT, input);
                let pchars: Vec<char> = prompt.chars().collect();
                let mut pi = 0;
                let mut prompt_chunks: Vec<String> = Vec::new();
                if pchars.is_empty() {
                    prompt_chunks.push(String::new());
                } else {
                    while pi < pchars.len() {
                        let end = (pi + max_cols).min(pchars.len());
                        prompt_chunks.push(pchars[pi..end].iter().collect());
                        pi = end;
                    }
                }
                // Show only as many lines as fit, scrolled to the bottom.
                let max_lines = ch.saturating_sub(pad * 2) / line_step.max(1);
                let total = lines.len() + prompt_chunks.len();
                let start = total.saturating_sub(max_lines);
                let mut y = cy + pad;
                for idx in start..total {
                    if y + ROW_H > cy + ch - pad { break; }
                    let (is_prompt_line, is_last_prompt, s) = if idx < lines.len() {
                        (false, false, &lines[idx])
                    } else {
                        let pidx = idx - lines.len();
                        let last = pidx == prompt_chunks.len() - 1;
                        (true, last, &prompt_chunks[pidx])
                    };
                    let fg = if is_prompt_line && s.starts_with(TERM_PROMPT) {
                        TERM_PROMPT_FG
                    } else {
                        TERM_FG
                    };
                    draw_text(cx + pad, y, s, fg, TERM_BG, inner_w);
                    // Blinking cursor lives at the end of the last prompt row.
                    if is_last_prompt && *blink_on {
                        let col = s.chars().count();
                        let cursor_x = cx + pad + col * CHAR_W;
                        if cursor_x + CHAR_W <= cx + cw - pad {
                            framebuffer::fill_rect(cursor_x, y, CHAR_W, ROW_H, TERM_FG);
                        }
                    }
                    y += line_step;
                }
            }
        }
        AppState::ImageView => {
            if let Some((cx, cy, cw, ch)) = find_canvas_screen(win) {
                // Light backing so letterboxing around the image looks intentional.
                framebuffer::fill_rect(cx, cy, cw, ch, theme::FIELD_BG);
                if let Some(img) = crate::image::hera() {
                    // Centre the image in the canvas and clip to canvas bounds.
                    let draw_w = img.width.min(cw);
                    let draw_h = img.height.min(ch);
                    let off_x = (cw - draw_w) / 2;
                    let off_y = (ch - draw_h) / 2;
                    framebuffer::blit(
                        cx + off_x,
                        cy + off_y,
                        draw_w,
                        draw_h,
                        &img.pixels,
                        img.width,
                    );
                } else {
                    let msg = "(failed to decode embedded BMP)";
                    draw_text(cx + 10, cy + ch / 2, msg, theme::WARNING, theme::FIELD_BG, cw.saturating_sub(20));
                }
            }
        }
        AppState::FileManager { entries, selected, view_mode } => {
            let (ox, oy) = win.content_origin();
            let oxi = ox.max(0) as usize;
            let oyi = oy.max(0) as usize;
            match view_mode {
                FmViewMode::List => {
                    for (i, ent) in entries.iter().take(FM_ROWS).enumerate() {
                        let row_top = FM_FILE_Y0 + i * FM_ROW_H;
                        let iy = oyi
                            .saturating_add(row_top)
                            .saturating_add((FM_ROW_H - crate::icons::ICON_SIZE) / 2);
                        let ix = oxi + FM_ICON_X;
                        let icon = if ent.ends_with('/') {
                            &crate::icons::FOLDER
                        } else {
                            &crate::icons::FILE
                        };
                        crate::icons::draw(
                            ix,
                            iy,
                            icon,
                            ICON_OUTLINE,
                            ICON_FILL,
                            ICON_ACCENT,
                        );
                    }
                }
                FmViewMode::LargeIcons => {
                    for (i, ent) in entries.iter().take(FM_LG_VISIBLE).enumerate() {
                        let col = i % FM_LG_COLS;
                        let row = i / FM_LG_COLS;
                        let cell_x = oxi + 10 + col * FM_LG_CELL_W;
                        let cell_y = oyi + FM_LG_Y0 + row * FM_LG_CELL_H;
                        let selected_here = *selected == Some(i);
                        if selected_here {
                            framebuffer::fill_rect(
                                cell_x,
                                cell_y,
                                FM_LG_CELL_W.saturating_sub(8),
                                FM_LG_CELL_H.saturating_sub(8),
                                Color::hex(0x164E63),
                            );
                            framebuffer::stroke_rect(
                                cell_x,
                                cell_y,
                                FM_LG_CELL_W.saturating_sub(8),
                                FM_LG_CELL_H.saturating_sub(8),
                                theme::ACCENT,
                            );
                        }
                        let icon = if ent.ends_with('/') {
                            &crate::icons::FOLDER
                        } else {
                            &crate::icons::FILE
                        };
                        let accent = crate::icons::accent_for_icon(icon);
                        let ix = cell_x + (FM_LG_CELL_W.saturating_sub(8) - FM_LG_ICON) / 2;
                        let iy = cell_y + 8;
                        crate::icons::draw_scaled(
                            ix,
                            iy,
                            FM_LG_ICON,
                            icon,
                            ICON_OUTLINE,
                            ICON_FILL,
                            accent,
                        );
                        let name = fm_display_name(ent);
                        let short: String = name.chars().take(10).collect();
                        let nw = short.chars().count() * CHAR_W;
                        let nx = cell_x + (FM_LG_CELL_W.saturating_sub(8).saturating_sub(nw)) / 2;
                        let ny = cell_y + 8 + FM_LG_ICON + 6;
                        let bg = if selected_here {
                            Color::hex(0x164E63)
                        } else {
                            WIN_CONTENT_BG
                        };
                        draw_text(nx, ny, &short, WIN_TEXT, bg, FM_LG_CELL_W.saturating_sub(12));
                    }
                }
            }
        }
        AppState::FileDialog { entries, selected, .. } => {
            let (ox, oy) = win.content_origin();
            let oxi = ox.max(0) as usize;
            let oyi = oy.max(0) as usize;
            for (i, ent) in entries.iter().take(FD_ROWS).enumerate() {
                let row_top = FD_LIST_Y0 + i * FM_ROW_H;
                let iy = oyi
                    .saturating_add(row_top)
                    .saturating_add((FM_ROW_H - crate::icons::ICON_SIZE) / 2);
                let ix = oxi + 10;
                if *selected == Some(i) {
                    framebuffer::fill_rect(
                        oxi + 8,
                        oyi + row_top,
                        win.w.saturating_sub(BORDER * 2 + 16),
                        FM_ROW_H,
                        Color::hex(0x164E63),
                    );
                }
                let icon = if ent.ends_with('/') {
                    &crate::icons::FOLDER
                } else {
                    &crate::icons::FILE
                };
                crate::icons::draw(ix, iy, icon, ICON_OUTLINE, ICON_FILL, ICON_ACCENT);
            }
        }
        _ => {}
    }
}

pub fn render() {
    paint_desktop();
    let wm = WM.lock();
    let top_idx = wm.topmost_idx();
    for (i, win) in wm.windows.iter().enumerate() {
        if win.minimized {
            continue;
        }
        render_window(win, Some(i) == top_idx);
    }
    drop(wm);
    // Taskbar draws on top of windows so it stays visible even when a
    // window happens to overlap the bottom of the screen.
    paint_taskbar();
}

/// Fast path for button hover: only repaint the focused window.
pub fn render_top_window() {
    let wm = WM.lock();
    if let Some(ti) = wm.topmost_idx() {
        render_window(&wm.windows[ti], true);
    }
}

// ── Launcher / app constructors ─────────────────────────────────────────────

fn set_label_text(win: &mut Window, label_idx: usize, text: &str) {
    if let Some(Widget::Label { text: t, .. }) = win.widgets.get_mut(label_idx) {
        t.clear();
        t.push_str(text);
    }
}

fn create_launcher_window() -> Window {
    // Read the live registry, not the compile-time table, so apps installed
    // this session show up the next time the launcher opens.
    let apps = crate::apps::list();
    let cols = 4usize;
    let bw = 168usize;
    let bh = 64usize;
    let gap_x = 14usize;
    let gap_y = 12usize;
    let pad_top = 44usize;
    let pad_bottom = 24usize;
    let exit_gap = 18usize;

    let rows = (apps.len() + cols - 1) / cols;
    let grid_h = rows * bh + rows.saturating_sub(1) * gap_y;
    let win_w = 16 + cols * bw + (cols - 1) * gap_x + 16;
    let win_h = TITLEBAR_H + pad_top + grid_h + exit_gap + bh + pad_bottom + FOOTER_H;

    let heading = alloc::format!("Favorites · {} apps", apps.len());
    let mut w = Window::new(48, 36, win_w, win_h, "Applications")
        .with_kind(WindowKind::Launcher)
        .with_footer("Games, learning tools, and creative apps — click to open.")
        .add(Widget::label(14, 12, &heading));

    for (i, app) in apps.iter().enumerate() {
        let col = i % cols;
        let row = i / cols;
        let bx = 16 + col * (bw + gap_x);
        let by = pad_top + row * (bh + gap_y);
        let idx = i as u8;
        let icon = crate::icons::for_app_name(&app.name);
        w = w.add(Widget::icon_button(bx, by, bw, bh, &app.name, WinAction::LaunchApp(idx), icon));
    }

    let exit_y = pad_top + grid_h + exit_gap;
    let exit_x = 16 + (cols.saturating_sub(1)) * (bw + gap_x);
    let shut_x = 16 + (cols.saturating_sub(2)) * (bw + gap_x);
    w = w
        .add(Widget::button(shut_x, exit_y, bw, bh, "Shutdown", WinAction::Shutdown))
        .add(Widget::button(exit_x, exit_y, bw, bh, "Exit desktop", WinAction::ExitGui));
    w
}

fn create_calculator_window() -> Window {
    let mut w = Window::new(260, 110, 268, 380, "Calculator")
        .with_kind(WindowKind::Calculator)
        .with_app(AppState::Calculator {
            lhs: None,
            op: None,
            entry: String::from("0"),
            new_entry: true,
        })
        .with_footer("")
        .add(Widget::label(12, 22, "                   0"));

    let grid: &[(usize, usize, WinAction, &str)] = &[
        (0, 0, WinAction::CalcClear, "C"),
        (1, 0, WinAction::CalcDigit(255), "<-"),
        (2, 0, WinAction::CalcSign, "+/-"),
        (3, 0, WinAction::CalcOp('/'), "/"),
        (0, 1, WinAction::CalcDigit(7), "7"),
        (1, 1, WinAction::CalcDigit(8), "8"),
        (2, 1, WinAction::CalcDigit(9), "9"),
        (3, 1, WinAction::CalcOp('*'), "*"),
        (0, 2, WinAction::CalcDigit(4), "4"),
        (1, 2, WinAction::CalcDigit(5), "5"),
        (2, 2, WinAction::CalcDigit(6), "6"),
        (3, 2, WinAction::CalcOp('-'), "-"),
        (0, 3, WinAction::CalcDigit(1), "1"),
        (1, 3, WinAction::CalcDigit(2), "2"),
        (2, 3, WinAction::CalcDigit(3), "3"),
        (3, 3, WinAction::CalcOp('+'), "+"),
    ];

    let pad = 12usize;
    let display_h = 52usize;
    let btn_w = 54usize;
    let btn_h = 42usize;
    let btn_gap = 8usize;
    let grid_top = pad + display_h + pad;
    for &(col, row, ref action, label) in grid {
        let x = pad + col * (btn_w + btn_gap);
        let y = grid_top + row * (btn_h + btn_gap);
        w = w.add(Widget::button(x, y, btn_w, btn_h, label, action.clone()));
    }
    let y4 = grid_top + 4 * (btn_h + btn_gap);
    let zero_w = btn_w * 2 + btn_gap;
    w = w.add(Widget::button(pad, y4, zero_w, btn_h, "0", WinAction::CalcDigit(0)));
    w = w.add(Widget::button(pad + zero_w + btn_gap, y4, btn_w, btn_h, ".", WinAction::None));
    w = w.add(Widget::button(
        pad + zero_w + btn_gap + btn_w + btn_gap,
        y4,
        btn_w,
        btn_h,
        "=",
        WinAction::CalcEq,
    ));
    w
}

fn create_terminal_window() -> Window {
    // Empty Linux-style terminal. The window content is a single full-area
    // canvas; `render_app` paints the scrollback + prompt + blinking cursor
    // over it. Keystrokes go to `AppState::Terminal`; Enter runs a command.
    let mut w = Window::new(200, 100, 620, 360, "Terminal")
        .with_kind(WindowKind::Terminal)
        .with_app(AppState::Terminal {
            scrollback: Vec::new(),
            input: String::new(),
            blink_on: true,
            last_blink_tick: 0,
        })
        .add(Widget::canvas(0, 0, 616, 334));
    // Terminal captures all keystrokes when it's the top window, regardless
    // of which widget was last clicked.
    w.focused_widget = None;
    w
}

fn create_monitor_window() -> Window {
    let mut w = Window::new(700, 260, 360, 190, "Clock & Monitor")
        .with_kind(WindowKind::Monitor)
        .with_footer("Live system stats — updates once per second.")
        .add(Widget::label(10, 10, "uptime: Boot+00:00:00"))
        .add(Widget::label(10, 32, "ticks: 0"))
        .add(Widget::label(10, 54, "heap: 0 / 0 bytes"));
    let t = crate::clock::ticks();
    let secs = t / 18;
    let h = secs / 3600;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    set_label_text(&mut w, 0, &alloc::format!("uptime: Boot+{:02}:{:02}:{:02}", h, m, s));
    set_label_text(&mut w, 1, &alloc::format!("ticks: {}", t));
    set_label_text(
        &mut w, 2,
        &alloc::format!("heap: {} / {} bytes", crate::allocator::used(), crate::allocator::size()),
    );
    w
}

fn create_my_computer_window() -> Window {
    // "My Computer" — a summary of the available disks. Double-click (or
    // single-click the labelled icon-button) to open the File Explorer.
    let mut w = Window::new(200, 100, 480, 410, "My Computer")
        .with_kind(WindowKind::MyComputer)
        .with_footer("Click a drive. /data is writable (RAM); /fat and /init are read-only.")
        .add(Widget::label(10, 10, "Devices and drives:"));

    let (fat_label, fat_ok) = match crate::fs::cmd_ls(Some("/fat")) {
        Ok(v) => (
            alloc::format!("Local Disk (/fat)  —  FAT32 demo  ({} entries)", v.len()),
            true,
        ),
        Err(_) => (String::from("Local Disk (/fat)  —  (unavailable)"), false),
    };
    let init_files = crate::fs::cmd_ls(Some("/init")).unwrap_or_default();
    let init_label = alloc::format!(
        "RAM disk (/init)  —  initramfs  ({} files)",
        init_files.len(),
    );
    let data_label = String::from(
        "Data (/data)  —  read/write RAM (folders & files, lost on reset)",
    );
    let disk_label = match crate::fs::disk_usage() {
        Some((used, total)) => alloc::format!(
            "Files ({})  —  hard disk, kept across reboots  ({} of {} KiB used)",
            crate::fs::DISK_ROOT,
            used / 1024,
            total / 1024,
        ),
        None => String::from("Files (/disk)  —  no data disk attached"),
    };
    let usb_label = match crate::fs::usb_usage() {
        Some((used, total)) => alloc::format!(
            "USB Drive ({})  —  FAT32, read/write  ({} of {} KiB used)",
            crate::fs::USB_ROOT,
            used / 1024,
            total / 1024,
        ),
        None if crate::usb::has_msc() => {
            String::from("USB Drive (/usb)  —  attached, but not FAT32 (not mounted)")
        }
        None => String::from("USB Drive (/usb)  —  no USB drive attached"),
    };

    w = w.add(Widget::icon_button(
        10, 36, 456, 44,
        &disk_label,
        if crate::fs::has_disk() { WinAction::OpenDrive(3) } else { WinAction::None },
        &crate::icons::DRIVE,
    ));
    w = w.add(Widget::icon_button(
        10, 86, 456, 44,
        &fat_label,
        if fat_ok { WinAction::OpenDrive(0) } else { WinAction::None },
        &crate::icons::DRIVE,
    ));
    w = w.add(Widget::icon_button(
        10, 136, 456, 44,
        &init_label,
        WinAction::OpenDrive(1),
        &crate::icons::DRIVE,
    ));
    w = w.add(Widget::icon_button(
        10, 186, 456, 44,
        &data_label,
        WinAction::OpenDrive(2),
        &crate::icons::DRIVE,
    ));
    w = w.add(Widget::icon_button(
        10, 236, 456, 44,
        &usb_label,
        if crate::fs::has_usb() { WinAction::OpenDrive(4) } else { WinAction::None },
        &crate::icons::DRIVE,
    ));
    w = w.add(Widget::label(10, 294, "Downloads are saved to /disk/downloads."));
    w
}

fn create_file_manager_window() -> Window {
    // Widgets 0, 1..=12, 13..=19 toolbar, 20..=21 preview — see `FM_WIDGET_BASE`.
    let path = FM_PATH.lock().clone();
    let title = if path.is_empty() {
        String::from("File Explorer")
    } else {
        alloc::format!("File Explorer — {}", path)
    };
    let mut w = Window::new(320, 80, 640, 480, &title)
        .with_kind(WindowKind::FileManager)
        .with_app(AppState::FileManager {
            entries: Vec::new(),
            selected: None,
            view_mode: FmViewMode::List,
        })
        .with_footer("List or Icons view · dbl-click opens · /disk and /data are writable.")
        .add(Widget::label(10, 8, ""));
    for i in 0..FM_ROWS {
        w = w.add(Widget::label(FM_LIST_LABEL_X, FM_FILE_Y0 + i * FM_ROW_H, ""));
    }
    w = w
        .add(Widget::button(10, 32, 52, 24, "Up", WinAction::FmUp))
        .add(Widget::button(68, 32, 92, 24, "New folder", WinAction::FmNewFolder))
        .add(Widget::button(166, 32, 80, 24, "New file", WinAction::FmNewFile))
        .add(Widget::button(252, 32, 72, 24, "Refresh", WinAction::RefreshFiles))
        .add(Widget::button(330, 32, 72, 24, "Preview", WinAction::FmPreview))
        .add(Widget::button(408, 32, 64, 24, "Icons", WinAction::FmViewIcons))
        .add(Widget::button(478, 32, 56, 24, "List", WinAction::FmViewList))
        .add(Widget::label(10, 300, "Preview:"))
        .add(Widget::label(10, 320, ""));
    w
}

/// Does this name look like a picture the OS could open?
fn looks_like_picture(name: &str) -> bool {
    let lower = name.trim_end_matches('/').to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

fn fm_close_menu(win: &mut Window) {
    if win.widgets.len() > FM_WIDGET_BASE {
        win.widgets.truncate(FM_WIDGET_BASE);
    }
}

fn refresh_file_manager(win: &mut Window) {
    let path = FM_PATH.lock().clone();
    let shown = if path.is_empty() { "/" } else { path.as_str() };
    if win.title.starts_with("File Explorer") {
        win.title = if path.is_empty() {
            String::from("File Explorer")
        } else {
            alloc::format!("File Explorer — {}", path)
        };
    }
    let files = match crate::fs::cmd_ls(Some(shown)) {
        Ok(v) => v,
        Err(e) => {
            set_label_text(win, 0, &alloc::format!("Location: {}  —  list error: {}", shown, e));
            Vec::new()
        }
    };
    let mode = match &win.app {
        AppState::FileManager { view_mode, .. } => *view_mode,
        _ => FmViewMode::List,
    };
    let visible = match mode {
        FmViewMode::List => FM_ROWS,
        FmViewMode::LargeIcons => FM_LG_VISIBLE,
    };
    let mode_label = match mode {
        FmViewMode::List => "List",
        FmViewMode::LargeIcons => "Icons",
    };
    set_label_text(
        win,
        0,
        &alloc::format!(
            "Location: {}  —  {} items  ·  {} view",
            shown,
            files.len().min(visible),
            mode_label
        ),
    );
    for i in 0..FM_ROWS {
        if mode == FmViewMode::List {
            let txt = files.get(i).map_or("", |s| s.as_str());
            set_label_text(win, 1 + i, txt);
        } else {
            // Large-icons mode paints names in render_app; hide list labels.
            set_label_text(win, 1 + i, "");
        }
    }
    if let AppState::FileManager { entries, selected, .. } = &mut win.app {
        *entries = files;
        *selected = None;
    }
}

fn fm_display_name(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    p.rsplit('/').next().unwrap_or(p)
}

fn fm_set_footer(win: &mut Window, msg: &str) {
    win.footer = Some(String::from(msg));
}

fn fm_working_parent() -> String {
    let p = FM_PATH.lock().clone();
    if p.is_empty() || p == "/" {
        String::from("/data")
    } else if fm_is_writable_path(&p) {
        p
    } else {
        String::new()
    }
}

fn fm_is_writable_path(path: &str) -> bool {
    path.starts_with("/data")
        || path.starts_with(crate::fs::DISK_ROOT)
        || path.starts_with(crate::fs::USB_ROOT)
}

/// Unique `name`, `name_2`, ... among entries in `parent` (ls).
fn fm_unique_name(parent: &str, base: &str, is_dir: bool) -> String {
    let Ok(list) = crate::fs::cmd_ls(Some(parent)) else {
        return String::from(base);
    };
    for n in 1..256u32 {
        let candidate: String = if n == 1 {
            String::from(base)
        } else if is_dir {
            alloc::format!("{}_{}", base, n)
        } else if let Some(dot) = base.rfind('.') {
            let (stem, ext) = base.split_at(dot);
            alloc::format!("{}_{}{}", stem, n, ext)
        } else {
            alloc::format!("{}_{}", base, n)
        };
        let full = if is_dir {
            alloc::format!("{}/{}/", parent.trim_end_matches('/'), candidate)
        } else {
            alloc::format!("{}/{}", parent.trim_end_matches('/'), candidate)
        };
        let hit = if is_dir {
            list.iter()
                .any(|e| e.trim_end_matches('/') == full.trim_end_matches('/'))
        } else {
            list.iter().any(|e| e == &full)
        };
        if !hit {
            return candidate;
        }
    }
    String::from(base)
}

fn file_manager_update_row_labels(win: &mut Window) {
    let (entries, sel, mode) = match &win.app {
        AppState::FileManager { entries, selected, view_mode } => {
            (entries.clone(), *selected, *view_mode)
        }
        _ => return,
    };
    if mode != FmViewMode::List {
        return;
    }
    for i in 0..FM_ROWS {
        let txt = entries.get(i).map_or("", |s| s.as_str());
        let show = if Some(i) == sel {
            alloc::format!("> {}", txt)
        } else {
            String::from(txt)
        };
        set_label_text(win, 1 + i, &show);
    }
}

fn file_manager_set_selection(win: &mut Window, row: Option<usize>) {
    if let AppState::FileManager { selected, .. } = &mut win.app {
        *selected = row;
    }
    file_manager_update_row_labels(win);
}

fn file_manager_open_context(win: &mut Window, _mx: i32, my: i32) {
    fm_close_menu(win);
    let (_ox, oy) = win.content_origin();
    let lx = 120usize;
    let mut ly = (my - oy).saturating_sub(4) as usize;
    if ly > 200 {
        ly = 200;
    }
    // Offer the wallpaper command only for a file that could plausibly be a
    // picture, so the menu does not invite an action that must fail.
    let picture = match &win.app {
        AppState::FileManager { entries, selected, .. } => selected
            .and_then(|i| entries.get(i))
            .is_some_and(|name| looks_like_picture(name)),
        _ => false,
    };
    let mut items: Vec<(&str, WinAction)> = alloc::vec![("Open", WinAction::FmOpen)];
    if picture {
        items.push(("Set as wallpaper", WinAction::FmSetWallpaper));
    }
    items.extend_from_slice(&[
        ("New folder", WinAction::FmNewFolder),
        ("New file", WinAction::FmNewFile),
        ("Delete", WinAction::FmDelete),
        ("Refresh", WinAction::RefreshFiles),
    ]);
    for (i, (lab, act)) in items.iter().enumerate() {
        win.widgets.push(Widget::button(
            lx,
            ly + i * 28,
            140,
            24,
            lab,
            *act,
        ));
    }
}

/// Content-local coords → entry index, honouring List vs Large Icons.
fn file_manager_entry_at(win: &Window, lx: i32, ly: i32) -> Option<usize> {
    let mode = match &win.app {
        AppState::FileManager { view_mode, .. } => *view_mode,
        _ => FmViewMode::List,
    };
    match mode {
        FmViewMode::List => {
            if ly < FM_FILE_Y0 as i32 {
                return None;
            }
            let r = (ly as usize - FM_FILE_Y0) / FM_ROW_H;
            if r < FM_ROWS {
                Some(r)
            } else {
                None
            }
        }
        FmViewMode::LargeIcons => {
            if ly < FM_LG_Y0 as i32 || lx < 10 {
                return None;
            }
            let col = ((lx as usize).saturating_sub(10)) / FM_LG_CELL_W;
            let row = ((ly as usize).saturating_sub(FM_LG_Y0)) / FM_LG_CELL_H;
            if col >= FM_LG_COLS {
                return None;
            }
            let idx = row * FM_LG_COLS + col;
            if idx < FM_LG_VISIBLE {
                Some(idx)
            } else {
                None
            }
        }
    }
}

/// Returns (handled, deferred_double_open). `deferred_double_open` means the
/// row is selected; call `file_manager_open_entry_internal` only **after** the
/// caller drops `WM` — otherwise `file_manager_open_entry_internal` re-locks
/// `WM` and deadlocks a `spin::Mutex`.
fn file_manager_handle_mouse(
    win: &mut Window,
    mx: i32,
    my: i32,
    left_edge: bool,
    right_edge: bool,
    dbl: bool,
) -> (bool, bool) {
    let (ox, oy) = win.content_origin();
    let lx = mx - ox;
    let ly = my - oy;

    if right_edge {
        if win.widget_at(mx, my).is_some() {
            return (false, false);
        }
        fm_close_menu(win);
        if let Some(row) = file_manager_entry_at(win, lx, ly) {
            if let AppState::FileManager { entries, .. } = &mut win.app {
                if row < entries.len() {
                    file_manager_set_selection(win, Some(row));
                } else {
                    file_manager_set_selection(win, None);
                }
            }
        } else {
            file_manager_set_selection(win, None);
        }
        file_manager_open_context(win, mx, my);
        return (true, false);
    }

    if !left_edge {
        return (false, false);
    }

    if let Some(wi) = win.widget_at(mx, my) {
        if win.widgets.len() > FM_WIDGET_BASE && wi < FM_WIDGET_BASE {
            fm_close_menu(win);
        }
        return (false, false);
    }

    if left_edge && (win.widgets.len() > FM_WIDGET_BASE) {
        fm_close_menu(win);
    }

    if let Some(row) = file_manager_entry_at(win, lx, ly) {
        let ent_len = if let AppState::FileManager { entries, .. } = &win.app {
            entries.len()
        } else {
            0
        };
        if row < ent_len {
            file_manager_set_selection(win, Some(row));
            let defer_open = dbl;
            return (true, defer_open);
        }
        file_manager_set_selection(win, None);
        return (true, false);
    }
    file_manager_set_selection(win, None);
    (true, false)
}

// ── Wallpaper ───────────────────────────────────────────────────────────────

/// Decode a picture from the filesystem and hang it on the desktop.
///
/// The choice is written to the data disk as well, so the next boot puts the
/// same picture back rather than reverting to the drawn scene.
fn set_wallpaper_from(path: &str) -> Result<(), String> {
    let bytes = crate::fs::cmd_cat(path).map_err(String::from)?;
    let format = crate::image::format_name(&bytes).unwrap_or("unrecognised");
    let image = crate::image::decode(&bytes)
        .ok_or_else(|| alloc::format!("{} is a {} file this OS cannot decode", path, format))?;
    crate::wallpaper::set_picture(path, alloc::sync::Arc::new(image));
    if let Err(e) = crate::fs::remember_wallpaper(path) {
        // Not fatal: the wallpaper is on screen, it just will not come back.
        crate::println!("[wallpaper] could not record the choice: {}", e);
    }
    refresh_settings_wallpaper_line();
    request_redraw();
    Ok(())
}

/// Load the wallpaper the user chose last time, if the note survived.
pub fn restore_wallpaper() {
    let Some(path) = crate::fs::remembered_wallpaper() else {
        return;
    };
    match crate::fs::cmd_cat(&path).ok().and_then(|b| crate::image::decode(&b)) {
        Some(image) => crate::wallpaper::set_picture(&path, alloc::sync::Arc::new(image)),
        None => crate::warn_line(&alloc::format!(
            "Saved wallpaper {} could not be loaded — using the drawn one",
            path
        )),
    }
}

fn file_manager_set_wallpaper() {
    let selected = {
        let wm = WM.lock();
        wm.topmost_idx().and_then(|t| {
            let win = &wm.windows[t];
            match &win.app {
                AppState::FileManager { entries, selected, .. } => {
                    selected.and_then(|i| entries.get(i)).cloned()
                }
                _ => None,
            }
        })
    };
    fm_close_top_file_menu();
    let message = match selected {
        None => String::from("Select a picture first."),
        Some(path) => match set_wallpaper_from(&path) {
            Ok(()) => alloc::format!("Wallpaper set from {}", path),
            Err(e) => e,
        },
    };
    let mut wm = WM.lock();
    if let Some(t) = wm.topmost_idx() {
        if wm.windows[t].kind == WindowKind::FileManager {
            fm_set_footer(&mut wm.windows[t], &message);
        }
    }
}

fn file_manager_open_entry_internal() {
    let path: String;
    let is_dir: bool;
    {
        let wm = WM.lock();
        let Some(top) = wm.topmost_idx() else { return; };
        let win = &wm.windows[top];
        if win.kind != WindowKind::FileManager {
            return;
        }
        let AppState::FileManager { entries, selected, .. } = &win.app else {
            return;
        };
        let Some(i) = *selected else { return; };
        let Some(ent) = entries.get(i) else { return; };
        path = ent.clone();
        is_dir = ent.ends_with('/');
    }
    if is_dir {
        *FM_PATH.lock() = path.trim_end_matches('/').to_string();
        let mut wm = WM.lock();
        if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
            refresh_file_manager(&mut wm.windows[idx]);
        }
    } else {
        // Text and source files pop open in Notepad, their own window — the
        // File Manager stays put behind it. Anything that is not plain UTF-8
        // text (pictures, binaries) falls back to the inline preview pane,
        // which already knows how to describe those.
        match crate::fs::cmd_cat(&path) {
            Ok(bytes) if crate::image::format_name(&bytes).is_none() => {
                match core::str::from_utf8(&bytes) {
                    Ok(text) if is_c_source_path(&path) => open_path_in_code_editor(&path, text),
                    Ok(text) => open_path_in_notepad(&path, text),
                    Err(_) => do_file_preview(&path),
                }
            }
            _ => do_file_preview(&path),
        }
    }
}

/// `.c` / `.h` files open in the Code Editor (syntax highlighting, Build,
/// Run); every other text file opens in Notepad.
fn is_c_source_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".c") || lower.ends_with(".h")
}

/// How many characters fit on one on-screen row of a `CodeArea` of width `w`.
/// Shared by rendering, click hit-testing, and arrow-key column math so they
/// can never disagree about where a character lands.
fn code_area_max_chars(w: usize) -> usize {
    let inner_w = w.saturating_sub(6 + 4);
    (inner_w / CHAR_W.max(1)).max(1)
}

/// Wraps `chars` into on-screen rows exactly like the `CodeArea` canvas does:
/// split on `\n`, then hard-wrap every `max_chars` characters (no word
/// break). Each entry is the `[start, end)` char range of that row, `end`
/// exclusive of any `\n` that ends it. Always has at least one row.
fn wrap_rows(chars: &[char], max_chars: usize) -> Vec<(usize, usize)> {
    let max_chars = max_chars.max(1);
    let mut rows = Vec::new();
    let mut row_start = 0usize;
    let mut col = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            rows.push((row_start, i));
            row_start = i + 1;
            col = 0;
            continue;
        }
        col += 1;
        if col >= max_chars {
            rows.push((row_start, i + 1));
            row_start = i + 1;
            col = 0;
        }
    }
    rows.push((row_start, chars.len()));
    rows
}

/// Locates a char-index cursor within `rows`, returning `(row, column)`.
fn cursor_row_col(rows: &[(usize, usize)], cursor: usize) -> (usize, usize) {
    for (r, &(start, end)) in rows.iter().enumerate() {
        if cursor <= end {
            return (r, cursor - start);
        }
    }
    let last = rows.len() - 1;
    (last, rows[last].1 - rows[last].0)
}

/// Converts a char index into `s` to the corresponding byte index, for use
/// with `String` methods that require a UTF-8 char boundary.
fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Maps a click at `(lx, ly)` local to a `CodeArea` of width `w` to the
/// nearest character index, clamping to the nearest row/column so clicks
/// past the end of a line or below the last line still land somewhere sane.
fn code_area_cursor_at(text: &str, w: usize, lx: i32, ly: i32) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let max_chars = code_area_max_chars(w);
    let rows = wrap_rows(&chars, max_chars);
    let line_step = (ROW_H + 2) as i32;
    let top = 4i32;
    let row = (((ly - top).max(0)) / line_step.max(1)) as usize;
    let row = row.min(rows.len() - 1);
    let pad_left = 6i32;
    let col_px = (lx - pad_left).max(0);
    let col = (col_px / CHAR_W.max(1) as i32) as usize;
    let (start, end) = rows[row];
    start + col.min(end - start)
}

/// Classifies a char for double-click word selection: identifiers, runs of
/// whitespace, and runs of punctuation/symbols each form their own "word" so
/// double-clicking an operator like `->` selects it rather than the closest
/// identifier.
fn word_char_class(c: char) -> u8 {
    if c.is_alphanumeric() || c == '_' {
        0
    } else if c.is_whitespace() {
        1
    } else {
        2
    }
}

/// Expands a double-click at char index `idx` to the `[start, end)` bounds of
/// the word/run under (or immediately before, if `idx` is at the end of one)
/// the click.
fn word_bounds_at(chars: &[char], idx: usize) -> (usize, usize) {
    if chars.is_empty() {
        return (0, 0);
    }
    let probe = if idx < chars.len() { idx } else { idx - 1 };
    let class = word_char_class(chars[probe]);
    let mut start = probe;
    while start > 0 && word_char_class(chars[start - 1]) == class {
        start -= 1;
    }
    let mut end = probe + 1;
    while end < chars.len() && word_char_class(chars[end]) == class {
        end += 1;
    }
    (start, end)
}

/// Normalizes a `CodeArea`'s (cursor, selection) pair into an ordered
/// `[lo, hi)` char range, or `None` when there is no real selection (either
/// there isn't one, or it has collapsed to a single point).
fn code_area_selected_range(cursor: usize, selection: Option<usize>) -> Option<(usize, usize)> {
    let a = selection?;
    if a == cursor { return None; }
    Some((a.min(cursor), a.max(cursor)))
}

/// Deletes the active selection (if any) in place, moving the cursor to
/// where it started and clearing it. Returns whether anything was deleted,
/// so callers can fall back to their normal single-character behaviour.
fn code_area_delete_selection(text: &mut String, cursor: &mut usize, selection: &mut Option<usize>) -> bool {
    let Some((lo, hi)) = code_area_selected_range(*cursor, *selection) else {
        *selection = None;
        return false;
    };
    let byte_lo = char_to_byte_idx(text, lo);
    let byte_hi = char_to_byte_idx(text, hi);
    text.replace_range(byte_lo..byte_hi, "");
    *cursor = lo;
    *selection = None;
    true
}

/// Open `path` in the Notepad window, spawning it if it is not already open.
/// Notepad is a single-instance window like the rest of this desktop's apps
/// (see [`raise_or_spawn`]), so opening a second file while one is already
/// open replaces its content rather than stacking a second Notepad window.
fn open_path_in_notepad(path: &str, text: &str) {
    *NOTEPAD_SCRATCH.lock() = String::from(text);
    *NOTEPAD_PATH.lock() = String::from(path);
    let mut wm = WM.lock();
    match wm.windows.iter().position(|w| w.kind == WindowKind::Notepad) {
        Some(idx) => {
            wm.raise(idx);
            let win = &mut wm.windows[idx];
            if let Some(Widget::TextArea { text: t, .. }) = win.widgets.get_mut(4) {
                t.clear();
                t.push_str(text);
            }
            notepad_refresh_status(win);
        }
        None => wm.spawn(create_notepad_window()),
    }
    drop(wm);
    request_redraw();
}

fn do_file_preview(path: &str) {
    let mut wm = WM.lock();
    let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) else {
        return;
    };
    let win = &mut wm.windows[idx];
    let body = match crate::fs::cmd_cat(path) {
        Ok(v) => match crate::image::format_name(&v) {
            // A picture previewed as text is a screenful of noise, so say what
            // it is instead — and point at what can be done with it.
            Some(kind) => match crate::image::decode(&v) {
                Some(image) => alloc::format!(
                    "{} picture — {} × {} pixels, {} bytes. \
                     Right-click it to set it as the desktop wallpaper.",
                    kind, image.width, image.height, v.len(),
                ),
                None => alloc::format!(
                    "{} picture, {} bytes — this OS cannot decode this variant.",
                    kind, v.len(),
                ),
            },
            None => {
                let s = core::str::from_utf8(&v).unwrap_or("");
                if s.chars().count() > 900 {
                    s.chars().take(900).collect::<String>() + " …"
                } else {
                    String::from(s)
                }
            }
        },
        Err(e) => alloc::format!("(cannot read: {})", e),
    };
    set_label_text(win, FM_PREVIEW_BODY, &body);
    let short = if path.chars().count() > 48 {
        path.chars().take(45).collect::<String>() + "…"
    } else {
        path.to_string()
    };
    fm_set_footer(win, &alloc::format!("Preview: {}", short));
}

fn file_manager_open_selected() {
    file_manager_open_entry_internal();
}

fn fm_close_top_file_menu() {
    let mut wm = WM.lock();
    if let Some(t) = wm.topmost_idx() {
        if wm.windows[t].kind == WindowKind::FileManager {
            fm_close_menu(&mut wm.windows[t]);
        }
    }
}

fn file_manager_preview_readme() {
    let path = FM_PATH.lock().clone();
    let shown = if path.is_empty() { "/fat" } else { path.as_str() };
    let readme_path = alloc::format!("{}/readme.txt", shown.trim_end_matches('/'));
    let fallback = alloc::format!("{}/welcome.txt", shown.trim_end_matches('/'));
    let attempt = |p: &str| match crate::fs::cmd_cat(p) {
        Ok(v) => core::str::from_utf8(&v)
            .map(|s| s.replace('\n', " | "))
            .unwrap_or_else(|_| String::from("(binary)")),
        Err(e) => alloc::format!("{}: {}", p, e),
    };
    let preview_body = if crate::fs::cmd_cat(&readme_path).is_ok() {
        attempt(&readme_path)
    } else {
        attempt(&fallback)
    };
    let mut wm = WM.lock();
    if let Some(t) = wm.topmost_idx() {
        let win = &mut wm.windows[t];
        if win.kind == WindowKind::FileManager {
            set_label_text(win, 19, &preview_body);
            fm_set_footer(win, "Preview: README / welcome (no file selected).");
        }
    }
}

fn file_manager_preview_selection_or_readme() {
    let sel = {
        let wm = WM.lock();
        let Some(t) = wm.topmost_idx() else {
            return;
        };
        let win = &wm.windows[t];
        if win.kind != WindowKind::FileManager {
            return;
        }
        if let AppState::FileManager { entries, selected, .. } = &win.app {
            selected.and_then(|i| entries.get(i).cloned())
        } else {
            return;
        }
    };
    if let Some(p) = sel {
        if p.ends_with('/') {
            fm_set_footer_top("Select a file, or open the folder with double-click.");
        } else {
            do_file_preview(&p);
        }
    } else {
        file_manager_preview_readme();
    }
}

/// Go to parent in FM_PATH and refresh.
fn file_manager_go_up() {
    let p = FM_PATH.lock().clone();
    if p.is_empty() || p == "/" {
        fm_set_footer_top("Already at top (virtual root).");
        return;
    }
    let par = crate::fs::path_parent(&p);
    *FM_PATH.lock() = if par == "/" { String::new() } else { par };
    let mut wm = WM.lock();
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
        refresh_file_manager(&mut wm.windows[idx]);
    }
}

fn fm_set_footer_top(msg: &str) {
    let mut wm = WM.lock();
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
        fm_set_footer(&mut wm.windows[idx], msg);
    }
}

fn file_manager_new_folder() {
    let parent = fm_working_parent();
    if parent.is_empty() || !fm_is_writable_path(&parent) {
        fm_set_footer_top("Create folders on /data only. Open the Data drive.");
        return;
    }
    let name = fm_unique_name(&parent, "New folder", true);
    let full = alloc::format!("{}/{}", parent.trim_end_matches('/'), name);
    match crate::fs::cmd_mkdir(&full) {
        Ok(()) => {
            fm_set_footer_top(&alloc::format!("Created {}", full));
            let mut wm = WM.lock();
            if let Some(i) = wm
                .windows
                .iter()
                .position(|w| w.kind == WindowKind::FileManager)
            {
                refresh_file_manager(&mut wm.windows[i]);
            }
        }
        Err(e) => fm_set_footer_top(&alloc::format!("mkdir: {}", e)),
    }
}

fn file_manager_new_file() {
    let parent = fm_working_parent();
    if parent.is_empty() || !fm_is_writable_path(&parent) {
        fm_set_footer_top("Create files on /data only.");
        return;
    }
    let name = fm_unique_name(&parent, "untitled.txt", false);
    let full = alloc::format!("{}/{}", parent.trim_end_matches('/'), name);
    match crate::fs::cmd_create_file(&full) {
        Ok(()) => {
            fm_set_footer_top(&alloc::format!("Created {}", full));
            let mut wm = WM.lock();
            if let Some(i) = wm
                .windows
                .iter()
                .position(|w| w.kind == WindowKind::FileManager)
            {
                refresh_file_manager(&mut wm.windows[i]);
            }
        }
        Err(e) => fm_set_footer_top(&alloc::format!("file: {}", e)),
    }
}

fn file_manager_delete() {
    let target = {
        let wm = WM.lock();
        let Some(top) = wm.topmost_idx() else {
            return;
        };
        let win = &wm.windows[top];
        if win.kind != WindowKind::FileManager {
            return;
        }
        if let AppState::FileManager { entries, selected, .. } = &win.app {
            selected.and_then(|i| entries.get(i).cloned())
        } else {
            return;
        }
    };
    let Some(path) = target else { return; };
    if !fm_is_writable_path(&path) {
        fm_set_footer_top("Delete only on /data (read-only else).");
        return;
    }
    let path = path.trim_end_matches('/').to_string();
    match crate::fs::cmd_remove(&path) {
        Ok(()) => {
            fm_set_footer_top("Deleted.");
            let mut wm = WM.lock();
            if let Some(i) = wm
                .windows
                .iter()
                .position(|w| w.kind == WindowKind::FileManager)
            {
                refresh_file_manager(&mut wm.windows[i]);
            }
        }
        Err(e) => fm_set_footer_top(&alloc::format!("remove: {}", e)),
    }
}

fn create_notepad_window() -> Window {
    // Classic notepad layout: menu strip, ruled paper page, status footer.
    let paper_w = 620usize;
    let paper_h = 340usize;
    let menu_h = 34usize;
    let win_w = paper_w + 24;
    let win_h = TITLEBAR_H + menu_h + paper_h + 16 + FOOTER_H + BORDER;
    let path = NOTEPAD_PATH.lock().clone();
    let path_show = if path.is_empty() {
        String::from("(unsaved)")
    } else {
        path
    };
    let status = notepad_status("");
    let mut w = Window::new(180, 50, win_w, win_h, "Notepad")
        .with_kind(WindowKind::Notepad)
        .with_footer(status.as_str())
        .add(Widget::button(10, 6, 88, 26, "Save", WinAction::NotepadSave))
        .add(Widget::button(104, 6, 88, 26, "Load", WinAction::NotepadLoad))
        .add(Widget::button(198, 6, 88, 26, "Clear", WinAction::NotepadClear))
        .add(Widget::label(300, 10, &alloc::format!("File  ·  {}", path_show)))
        .add(Widget::textarea(10, menu_h + 4, paper_w, paper_h));
    if let Some(Widget::TextArea { text, .. }) = w.widgets.get_mut(4) {
        text.push_str(&NOTEPAD_SCRATCH.lock());
        let st = notepad_status(text);
        w.footer = Some(st);
    }
    w.focused_widget = Some(4);
    w
}

fn notepad_status(text: &str) -> String {
    let lines = text.matches('\n').count() + 1;
    let chars = text.chars().count();
    let col = text.rsplit('\n').next().map(|s| s.chars().count()).unwrap_or(0) + 1;
    let path = NOTEPAD_PATH.lock().clone();
    let path_show = if path.is_empty() { "(unsaved)" } else { path.as_str() };
    alloc::format!("Ln {}  Col {}  |  {} chars  |  {}", lines, col, chars, path_show)
}

fn notepad_refresh_status(win: &mut Window) {
    if win.kind != WindowKind::Notepad {
        return;
    }
    if let Some(Widget::TextArea { text, .. }) = win.widgets.get(4) {
        win.footer = Some(notepad_status(text));
    }
    let path = NOTEPAD_PATH.lock().clone();
    let path_show = if path.is_empty() {
        String::from("(unsaved)")
    } else {
        path
    };
    set_label_text(win, 3, &alloc::format!("File  ·  {}", path_show));
}

fn notepad_set_path_label() {
    with_top(WindowKind::Notepad, |win| {
        notepad_refresh_status(win);
    });
}

// ── Code Editor ─────────────────────────────────────────────────────────────
//
// Widget layout: 0=New 1=Open 2=Save 3=Build 4=Run 5=path label
// 6=CodeArea (source) 7=OutputArea (build/run output).

const CODE_TEMPLATE: &str =
    "#include <stdio.h>\n\nint main(void) {\n    printf(\"Hello, OS101!\\n\");\n    return 0;\n}\n";

fn create_code_editor_window() -> Window {
    let editor_w = 760usize;
    let editor_h = 340usize;
    let output_h = 120usize;
    let menu_h = 34usize;
    let win_w = editor_w + 24;
    let win_h = TITLEBAR_H + menu_h + editor_h + 8 + output_h + 16 + FOOTER_H + BORDER;
    let path = CODE_PATH.lock().clone();
    let path_show = if path.is_empty() { String::from("(unsaved)") } else { path };
    let mut w = Window::new(140, 30, win_w, win_h, "Code Editor")
        .with_kind(WindowKind::CodeEditor)
        .with_footer("Write C, then Build to compile or Run to compile + execute.")
        .add(Widget::button(10, 6, 68, 26, "New", WinAction::CodeNew))
        .add(Widget::button(82, 6, 68, 26, "Open", WinAction::CodeOpen))
        .add(Widget::button(154, 6, 68, 26, "Save", WinAction::CodeSave))
        .add(Widget::button(226, 6, 78, 26, "Build", WinAction::CodeBuild))
        .add(Widget::button(308, 6, 68, 26, "Run", WinAction::CodeRun))
        .add(Widget::label(388, 10, &alloc::format!("File  ·  {}", path_show)))
        .add(Widget::code_area(10, menu_h + 4, editor_w, editor_h))
        .add(Widget::output_area(10, menu_h + editor_h + 12, editor_w, output_h));
    if let Some(Widget::CodeArea { text, cursor, .. }) = w.widgets.get_mut(6) {
        let scratch = CODE_SCRATCH.lock().clone();
        if scratch.is_empty() && CODE_PATH.lock().is_empty() {
            text.push_str(CODE_TEMPLATE);
        } else {
            text.push_str(&scratch);
        }
        *cursor = text.chars().count();
    }
    w.focused_widget = Some(6);
    w
}

fn code_editor_refresh_status(win: &mut Window) {
    if win.kind != WindowKind::CodeEditor {
        return;
    }
    if let Some(Widget::CodeArea { text, .. }) = win.widgets.get(6) {
        let lines = text.matches('\n').count() + 1;
        let chars = text.chars().count();
        win.footer = Some(alloc::format!(
            "{} lines  ·  {} chars  ·  Build compiles, Run compiles + executes.",
            lines, chars
        ));
    }
    let path = CODE_PATH.lock().clone();
    let path_show = if path.is_empty() { String::from("(unsaved)") } else { path };
    set_label_text(win, 5, &alloc::format!("File  ·  {}", path_show));
}

fn code_editor_set_path_label() {
    with_top(WindowKind::CodeEditor, |win| {
        code_editor_refresh_status(win);
    });
}

/// Open `path` in the Code Editor window, spawning it if it is not already
/// open. Single-instance, same as Notepad (see [`raise_or_spawn`]).
fn open_path_in_code_editor(path: &str, text: &str) {
    *CODE_SCRATCH.lock() = String::from(text);
    *CODE_PATH.lock() = String::from(path);
    let mut wm = WM.lock();
    match wm.windows.iter().position(|w| w.kind == WindowKind::CodeEditor) {
        Some(idx) => {
            wm.raise(idx);
            let win = &mut wm.windows[idx];
            if let Some(Widget::CodeArea { text: t, cursor, selection, .. }) = win.widgets.get_mut(6) {
                t.clear();
                t.push_str(text);
                *cursor = t.chars().count();
                *selection = None;
            }
            if let Some(Widget::OutputArea { text, error, .. }) = win.widgets.get_mut(7) {
                text.clear();
                *error = false;
            }
            code_editor_refresh_status(win);
        }
        None => wm.spawn(create_code_editor_window()),
    }
    drop(wm);
    request_redraw();
}

fn code_editor_new() {
    *CODE_PATH.lock() = String::new();
    *CODE_SCRATCH.lock() = String::new();
    with_top(WindowKind::CodeEditor, |win| {
        if let Some(Widget::CodeArea { text, cursor, selection, .. }) = win.widgets.get_mut(6) {
            text.clear();
            text.push_str(CODE_TEMPLATE);
            *cursor = text.chars().count();
            *selection = None;
        }
        if let Some(Widget::OutputArea { text, error, .. }) = win.widgets.get_mut(7) {
            text.clear();
            *error = false;
        }
        code_editor_refresh_status(win);
    });
}

fn code_editor_save() {
    with_top(WindowKind::CodeEditor, |win| {
        if let Some(Widget::CodeArea { text, .. }) = win.widgets.get(6) {
            *SAVE_AS_PENDING.lock() = text.clone();
            *CODE_SCRATCH.lock() = text.clone();
        }
    });
    open_file_dialog(FileDialogMode::Save, FileDialogTarget::CodeEditor);
}

fn code_editor_load() {
    open_file_dialog(FileDialogMode::Open, FileDialogTarget::CodeEditor);
}

fn code_editor_set_output(text: &str, error: bool) {
    with_top(WindowKind::CodeEditor, |win| {
        if let Some(Widget::OutputArea { text: t, error: e, .. }) = win.widgets.get_mut(7) {
            t.clear();
            t.push_str(text);
            *e = error;
        }
    });
}

/// Writes the current buffer to disk (auto-naming an unsaved buffer as
/// `/disk/untitled.c`, mirroring the Save As dialog's own default) and
/// returns the path it landed at. Both Build and Run call this first so
/// they always compile what is on screen, saved or not.
fn code_editor_persist() -> Result<String, &'static str> {
    let text = with_top_ret(WindowKind::CodeEditor, |win| {
        if let Some(Widget::CodeArea { text, .. }) = win.widgets.get(6) {
            text.clone()
        } else {
            String::new()
        }
    })
    .ok_or("Code Editor is not open")?;

    let mut path = CODE_PATH.lock().clone();
    if path.is_empty() {
        let dir = if crate::fs::has_disk() { "/disk" } else { "/data" };
        let _ = crate::fs::cmd_mkdir(dir);
        path = alloc::format!("{}/untitled.c", dir);
        *CODE_PATH.lock() = path.clone();
    }
    *CODE_SCRATCH.lock() = text.clone();
    crate::fs::cmd_write_file(&path, text.into_bytes())?;
    code_editor_set_path_label();
    Ok(path)
}

/// Compile the on-screen buffer. Returns the ELF path on success; on
/// failure the diagnostics are already shown in the output pane.
fn code_editor_build() -> Option<String> {
    let src = match code_editor_persist() {
        Ok(p) => p,
        Err(e) => {
            code_editor_set_output(&alloc::format!("Could not save source: {}", e), true);
            return None;
        }
    };
    let elf_path = src.strip_suffix(".c").unwrap_or(&src).to_string();
    let result = crate::tcc::compile(&["-o", &elf_path, &src]);
    if result.ok {
        let mut msg = alloc::format!("Build succeeded → {}\n", result.output_path.as_deref().unwrap_or(&elf_path));
        if !result.diagnostics.is_empty() {
            msg.push_str(&result.diagnostics);
            msg.push('\n');
        }
        code_editor_set_output(&msg, false);
        Some(elf_path)
    } else {
        let diag = if result.diagnostics.is_empty() {
            String::from("(no diagnostics — check the source compiles with `cc` from the terminal)")
        } else {
            result.diagnostics
        };
        code_editor_set_output(&alloc::format!("Build failed:\n{}", diag), true);
        None
    }
}

/// Build, then run the resulting binary, capturing its stdout into the
/// output pane. A single [`crate::process::run_scheduler_once`] call matches
/// the shell's own `run` command: cooperative programs that never yield or
/// block run to completion inside that one call.
fn code_editor_run() {
    let Some(elf_path) = code_editor_build() else {
        return;
    };
    let bytes = match crate::fs::cmd_cat(&elf_path) {
        Ok(b) => b,
        Err(e) => {
            code_editor_set_output(&alloc::format!("Run failed: could not read {}: {}", elf_path, e), true);
            return;
        }
    };
    let before = crate::process::live_space_count();
    crate::syscall::begin_capture();
    let spawned = crate::process::spawn_elf_bytes(&bytes);
    let output = match spawned {
        Ok(_) => {
            crate::process::run_scheduler_once();
            crate::syscall::end_capture()
        }
        Err(e) => {
            crate::syscall::end_capture();
            alloc::format!("spawn failed: {}", e)
        }
    };
    let after = crate::process::live_space_count();
    let mut report = alloc::format!("Build succeeded → {}\n--- Program output ---\n", elf_path);
    if output.is_empty() {
        report.push_str("(no output)\n");
    } else {
        report.push_str(&output);
        if !output.ends_with('\n') {
            report.push('\n');
        }
    }
    if after > before {
        report.push_str("--- still running (yielded) ---");
    } else {
        report.push_str("--- exited ---");
    }
    code_editor_set_output(&report, false);
}

fn create_paint_window() -> Window {
    const CW: usize = 500;
    const CH: usize = 270;

    let mut win = Window::new(240, 110, 520, 470, "Paint")
        .with_kind(WindowKind::Paint)
        .with_footer("Drag to draw.  Pick a colour, brush size, or tool below.")
        .with_app(AppState::Paint {
            canvas: vec![CANVAS_BG; CW * CH],
            cw: CW,
            ch: CH,
            drawing: false,
            last: None,
            color: PAINT_PALETTE[4],
            brush: 3,
            tool: PaintTool::Brush,
        })
        .add(Widget::canvas(10, 10, CW, CH));

    // Palette: two rows of eight chips under the canvas.
    const CHIP: usize = 26;
    const GAP: usize = 4;
    for (i, colour) in PAINT_PALETTE.iter().enumerate() {
        let col = i % 8;
        let row = i / 8;
        let mut sw = Widget::swatch(
            10 + col * (CHIP + GAP),
            292 + row * (CHIP + GAP),
            CHIP, CHIP,
            *colour,
            WinAction::PaintColor(i as u8),
        );
        if i == 4 {
            if let Widget::Swatch { selected, .. } = &mut sw {
                *selected = true;
            }
        }
        win = win.add(sw);
    }

    // Brush sizes and tools to the right of the palette.
    let bx = 10 + 8 * (CHIP + GAP) + 12;
    win = win
        .add(Widget::button(bx, 292, 38, 26, "S", WinAction::PaintBrush(1)))
        .add(Widget::button(bx + 42, 292, 38, 26, "M", WinAction::PaintBrush(3)))
        .add(Widget::button(bx + 84, 292, 38, 26, "L", WinAction::PaintBrush(7)))
        .add(Widget::button(bx, 322, 38, 26, "XL", WinAction::PaintBrush(14)));

    win.add(Widget::button(10, 356, 90, 28, "Brush", WinAction::PaintTool(PaintTool::Brush)))
        .add(Widget::button(106, 356, 90, 28, "Eraser", WinAction::PaintTool(PaintTool::Eraser)))
        .add(Widget::button(202, 356, 90, 28, "Fill", WinAction::PaintTool(PaintTool::Fill)))
        .add(Widget::button(298, 356, 90, 28, "Clear", WinAction::PaintClear))
}

fn create_snake_window() -> Window {
    let grid_w: u8 = 14;
    let grid_h: u8 = 10;
    let cell = 24usize;
    let canvas_w = cell * grid_w as usize;
    let canvas_h = cell * grid_h as usize;
    let pad_y = canvas_h + 44;
    let mut w = Window::new(
        260, 80,
        canvas_w + 24, canvas_h + 200,
        "Snake",
    )
    .with_kind(WindowKind::Snake)
    .with_footer("Arrows or big buttons.  R restarts.")
    .with_app(AppState::Snake {
        grid_w, grid_h,
        snake: vec![(7, 5), (6, 5), (5, 5)],
        dir: (1, 0),
        pending_dir: (1, 0),
        food: (10, 5),
        game_over: false,
        score: 0,
        rng: 0xDEAD_BEEF,
        last_step_ticks: 0,
    })
    .add(Widget::label(12, 10, "Eat the yellow apples!"))
    .add(Widget::canvas(12, 36, canvas_w, canvas_h))
    .add(Widget::label(12, pad_y, "Score: 0"))
    .add(Widget::button(12, pad_y + 28, 56, 44, "Up", WinAction::SnakeDir(0, -1)))
    .add(Widget::button(72, pad_y + 28, 56, 44, "Left", WinAction::SnakeDir(-1, 0)))
    .add(Widget::button(132, pad_y + 28, 56, 44, "Down", WinAction::SnakeDir(0, 1)))
    .add(Widget::button(192, pad_y + 28, 56, 44, "Right", WinAction::SnakeDir(1, 0)))
    .add(Widget::button(260, pad_y + 28, 100, 44, "Restart", WinAction::SnakeRestart));
    w.focused_widget = None;
    w
}

/// Stretch the kids-game canvas (and its control strip) to fill the window.
///
/// Called after maximise / restore so Breakout and Space Invaders actually use
/// the bigger screen instead of sitting in a postage-stamp canvas.
fn layout_kids_playfield(win: &mut Window) {
    match win.kind {
        WindowKind::Breakout | WindowKind::Invaders | WindowKind::Racing => {}
        _ => return,
    }

    let content_w = win.w.saturating_sub(BORDER * 2);
    let content_h = win.h.saturating_sub(TITLEBAR_H + FOOTER_H + BORDER);
    let ctrl_h: usize = if win.kind == WindowKind::Racing { 130 } else { 90 };
    let canvas_w = content_w.saturating_sub(24).max(200);
    let canvas_h = content_h.saturating_sub(36 + ctrl_h).max(160);
    let pad_y = canvas_h + 44;

    if let Some(Widget::Canvas { w, h, .. }) = win.widgets.get_mut(1) {
        *w = canvas_w;
        *h = canvas_h;
    }
    if let Some(Widget::Label { y, .. }) = win.widgets.get_mut(2) {
        *y = pad_y;
    }

    match win.kind {
        WindowKind::Breakout => {
            for (i, x) in [(3, 12usize), (4, 100), (5, 188), (6, 268), (7, 348)] {
                if let Some(Widget::Button { y, x: bx, .. }) = win.widgets.get_mut(i) {
                    *y = pad_y + 28;
                    *bx = x.min(canvas_w.saturating_sub(72));
                }
            }
            if let AppState::Breakout(state) = &mut win.app {
                state.resize(canvas_w, canvas_h);
            }
        }
        WindowKind::Invaders => {
            for (i, x) in [(3, 12usize), (4, 84), (5, 180), (6, 252), (7, 332), (8, 412)] {
                if let Some(Widget::Button { y, x: bx, .. }) = win.widgets.get_mut(i) {
                    *y = pad_y + 28;
                    *bx = x.min(canvas_w.saturating_sub(72));
                }
            }
            if let AppState::Invaders(state) = &mut win.app {
                state.resize(canvas_w, canvas_h);
            }
        }
        WindowKind::Racing => {
            for (i, (x, yoff)) in [
                (3, (12usize, 28usize)),
                (4, (92, 28)),
                (5, (172, 28)),
                (6, (252, 28)),
                (7, (12, 76)),
            ] {
                if let Some(Widget::Button { y, x: bx, .. }) = win.widgets.get_mut(i) {
                    *y = pad_y + yoff;
                    *bx = x.min(canvas_w.saturating_sub(72));
                }
            }
            if let AppState::Racing(state) = &mut win.app {
                state.resize(canvas_w, canvas_h);
            }
        }
        _ => {}
    }
}

fn create_breakout_window() -> Window {
    use crate::kids::breakout::{CANVAS_H, CANVAS_W};
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let canvas_w = ((sw * 55) / 100).clamp(CANVAS_W, sw.saturating_sub(80));
    let canvas_h = ((sh * 50) / 100).clamp(CANVAS_H, sh.saturating_sub(220));
    let pad_y = canvas_h + 44;
    let mut state = crate::kids::breakout::State::new();
    state.resize(canvas_w, canvas_h);
    let mut w = Window::new(
        ((sw.saturating_sub(canvas_w + 24)) / 2) as i32,
        40,
        canvas_w + 24,
        canvas_h + 160,
        "Breakout",
    )
    .with_kind(WindowKind::Breakout)
    .with_footer("← → paddle   ↑ faster   ↓ slower   Space unused")
    .with_app(AppState::Breakout(state))
    .add(Widget::label(12, 10, "Break the rainbow bricks!"))
    .add(Widget::canvas(12, 36, canvas_w, canvas_h))
    .add(Widget::label(12, pad_y, "Score 0   Hearts 3   Speed 3  (↑↓)"))
    .add(Widget::button(12, pad_y + 28, 80, 40, "<<", WinAction::BreakoutLeft))
    .add(Widget::button(100, pad_y + 28, 80, 40, ">>", WinAction::BreakoutRight))
    .add(Widget::button(188, pad_y + 28, 72, 40, "Speed+", WinAction::BreakoutFaster))
    .add(Widget::button(268, pad_y + 28, 72, 40, "Speed-", WinAction::BreakoutSlower))
    .add(Widget::button(348, pad_y + 28, 100, 40, "Restart", WinAction::BreakoutRestart));
    w.focused_widget = None;
    w
}

fn create_abc_window() -> Window {
    use crate::kids::abc::{CANVAS_H, CANVAS_W};
    let state = crate::kids::abc::State::new();
    let a = state.choice_label(0);
    let b = state.choice_label(1);
    let c = state.choice_label(2);
    let status = state.status_line();
    let pad_y = CANVAS_H + 44;
    let mut w = Window::new(
        240, 70,
        CANVAS_W + 24, CANVAS_H + 180,
        "ABC Fun",
    )
    .with_kind(WindowKind::Abc)
    .with_footer("Tap the letter that matches the big one.")
    .with_app(AppState::Abc(state))
    .add(Widget::label(12, 10, "Learn your letters!"))
    .add(Widget::canvas(12, 36, CANVAS_W, CANVAS_H))
    .add(Widget::label(12, pad_y, status.as_str()))
    .add(Widget::button(12, pad_y + 28, 100, 56, a.as_str(), WinAction::AbcPick(0)))
    .add(Widget::button(120, pad_y + 28, 100, 56, b.as_str(), WinAction::AbcPick(1)))
    .add(Widget::button(228, pad_y + 28, 100, 56, c.as_str(), WinAction::AbcPick(2)));
    w.focused_widget = None;
    w
}

fn create_racing_window() -> Window {
    use crate::kids::racing::{CANVAS_H, CANVAS_W};
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let canvas_w = CANVAS_W.max((sw * 22) / 100).min(360);
    let canvas_h = CANVAS_H.max((sh * 45) / 100).min(520);
    let pad_y = canvas_h + 44;
    let mut state = crate::kids::racing::State::new();
    state.resize(canvas_w, canvas_h);
    let mut w = Window::new(
        ((sw.saturating_sub(canvas_w + 24)) / 2) as i32,
        30,
        canvas_w + 24,
        canvas_h + 210,
        "Race Cars",
    )
    .with_kind(WindowKind::Racing)
    .with_footer("← → steer   ↑ faster   ↓ slower")
    .with_app(AppState::Racing(state))
    .add(Widget::label(12, 10, "Go go go!"))
    .add(Widget::canvas(12, 36, canvas_w, canvas_h))
    .add(Widget::label(12, pad_y, "Score 0   Speed 4  (↑ faster ↓ slower)"))
    .add(Widget::button(12, pad_y + 28, 72, 40, "<<", WinAction::RacingLeft))
    .add(Widget::button(92, pad_y + 28, 72, 40, ">>", WinAction::RacingRight))
    .add(Widget::button(172, pad_y + 28, 72, 40, "Speed+", WinAction::RacingFaster))
    .add(Widget::button(252, pad_y + 28, 72, 40, "Speed-", WinAction::RacingSlower))
    .add(Widget::button(12, pad_y + 76, 110, 40, "Restart", WinAction::RacingRestart));
    w.focused_widget = None;
    w
}

fn create_invaders_window() -> Window {
    use crate::kids::invaders::{CANVAS_H, CANVAS_W};
    let (sw, sh) = framebuffer::screen_size().unwrap_or((1280, 720));
    let canvas_w = ((sw * 55) / 100).clamp(CANVAS_W, sw.saturating_sub(80));
    let canvas_h = ((sh * 50) / 100).clamp(CANVAS_H, sh.saturating_sub(230));
    let pad_y = canvas_h + 44;
    let mut state = crate::kids::invaders::State::new();
    state.resize(canvas_w, canvas_h);
    let mut w = Window::new(
        ((sw.saturating_sub(canvas_w + 24)) / 2) as i32,
        30,
        canvas_w + 24,
        canvas_h + 170,
        "Space Invaders",
    )
    .with_kind(WindowKind::Invaders)
    .with_footer("← → move   Space = rapid fire   ↑ faster   ↓ slower")
    .with_app(AppState::Invaders(state))
    .add(Widget::label(12, 10, "Protect the planet!"))
    .add(Widget::canvas(12, 36, canvas_w, canvas_h))
    .add(Widget::label(12, pad_y, "Score 0   Lives 3   Speed 3  (↑↓)"))
    .add(Widget::button(12, pad_y + 28, 64, 40, "<<", WinAction::InvadersLeft))
    .add(Widget::button(84, pad_y + 28, 88, 40, "Fire!", WinAction::InvadersFire))
    .add(Widget::button(180, pad_y + 28, 64, 40, ">>", WinAction::InvadersRight))
    .add(Widget::button(252, pad_y + 28, 72, 40, "Speed+", WinAction::InvadersFaster))
    .add(Widget::button(332, pad_y + 28, 72, 40, "Speed-", WinAction::InvadersSlower))
    .add(Widget::button(412, pad_y + 28, 96, 40, "Restart", WinAction::InvadersRestart));
    w.focused_widget = None;
    w
}

fn create_image_view_window() -> Window {
    let caption = match crate::image::hera() {
        Some(img) => alloc::format!("hera.bmp  ({}×{}, 24-bit colour)", img.width, img.height),
        None => alloc::format!("hera.bmp  (decode failed)"),
    };
    let mut w = Window::new(340, 160, 420, 360, "Image Viewer")
        .with_kind(WindowKind::ImageView)
        .with_footer("Embedded bitmap preview.")
        .with_app(AppState::ImageView)
        .add(Widget::label(10, 8, ""))
        .add(Widget::canvas(10, 36, 400, 280));
    set_label_text(&mut w, 0, &caption);
    w
}

fn create_settings_window() -> Window {
    let cb = CURSOR_BLINK.load(Ordering::Relaxed);
    let dt = DARK_THEME.load(Ordering::Relaxed);
    let wallpaper = match crate::wallpaper::picture_path() {
        Some(path) => alloc::format!("Wallpaper: {}", path),
        None => String::from("Wallpaper: the drawn dusk scene"),
    };
    Window::new(340, 160, 420, 260, "Settings")
        .with_kind(WindowKind::Settings)
        .with_footer("The wallpaper choice is kept on the data disk; the toggles are not.")
        .with_app(AppState::Settings)
        .add(Widget::label(10, 8, "Appearance & Behaviour"))
        .add(Widget::checkbox(10, 40, 350, 30, "Blink shell cursor", cb, WinAction::ToggleCursorBlink))
        .add(Widget::checkbox(10, 76, 350, 30, "Dark desktop theme", dt, WinAction::ToggleDarkTheme))
        .add(Widget::label(10, 118, wallpaper.as_str()))
        .add(Widget::button(10, 142, 190, 30, "Use the drawn scene", WinAction::ResetWallpaper))
        .add(Widget::label(10, 184, "Right-click a picture in Files to set it."))
}

/// Keep the Settings window honest about which wallpaper is in use.
///
/// The wallpaper can also be changed from Files or the browser while Settings
/// is open, so this is called from every path that changes it.
fn refresh_settings_wallpaper_line() {
    const WALLPAPER_LABEL: usize = 3;
    let text = match crate::wallpaper::picture_path() {
        Some(path) => alloc::format!("Wallpaper: {}", path),
        None => String::from("Wallpaper: the drawn dusk scene"),
    };
    let mut wm = WM.lock();
    for win in wm.windows.iter_mut() {
        if win.kind == WindowKind::Settings {
            set_label_text(win, WALLPAPER_LABEL, &text);
        }
    }
}

fn create_about_window() -> Window {
    let version = alloc::format!("OS101  ·  v{}", env!("CARGO_PKG_VERSION"));
    Window::new(260, 40, 560, 420, "About OS101")
        .with_kind(WindowKind::About)
        .with_app(AppState::ImageView)
        .with_footer("Made with care for Inaaya & Aayan.")
        .add(Widget::canvas(16, 16, 180, 180))
        .add(Widget::label(214, 20, "Developed by"))
        .add(Widget::label(214, 44, "SM Mamunur Rahaman Hera"))
        .add(Widget::label(214, 68, "(Father of: Inaaya & Aayan)"))
        .add(Widget::label(214, 100, "Software Engineer, Bangladesh"))
        .add(Widget::label(214, 124, version.as_str()))
        .add(Widget::label(214, 156, "linkedin.com/in/sm-mamunur-rahman"))
        .add(Widget::label(16, 210, "A tiny OS that wants to grow up — neon desktop,"))
        .add(Widget::label(16, 232, "kid games, C apps, and a browser that tries."))
        .add(Widget::label(16, 262, "https://www.linkedin.com/in/sm-mamunur-rahman/"))
        .add(Widget::button(16, 300, 140, 36, "Close", WinAction::Close))
}

fn create_installer_window() -> Window {
    let disks = crate::install::list_disks();
    let source = crate::install::default_source(&disks).unwrap_or(crate::install::DiskId::AtaMaster);
    let target_infos = crate::install::install_targets(&disks, source);
    let targets: Vec<crate::install::DiskId> = target_infos.iter().map(|d| d.id).collect();
    let status = if targets.is_empty() {
        String::from("No other disk found. Attach a second IDE disk or USB stick.")
    } else {
        String::from("This live USB/ISO can install OS101 onto another disk permanently.")
    };
    let mut w = Window::new(200, 60, 560, 420, "Install OS101")
        .with_kind(WindowKind::Installer)
        .with_app(AppState::Installer {
            step: 0,
            source,
            targets,
            selected: None,
            status: status.clone(),
        })
        .with_footer("Copies this system onto the chosen disk. Existing data is erased.");
    rebuild_installer_widgets(&mut w);
    w
}

fn rebuild_installer_widgets(win: &mut Window) {
    let (step, source, targets, selected, status) = match &win.app {
        AppState::Installer {
            step,
            source,
            targets,
            selected,
            status,
        } => (*step, *source, targets.clone(), *selected, status.clone()),
        _ => return,
    };
    win.widgets.clear();
    win.widgets.push(Widget::label(16, 12, "Install OS101 permanently"));
    win.widgets
        .push(Widget::label(16, 36, &alloc::format!("Source: {}", source.label())));
    win.widgets.push(Widget::label(16, 60, &status));

    match step {
        0 => {
            win.widgets.push(Widget::label(
                16,
                100,
                "Booted from this install medium. Next: pick the disk to erase.",
            ));
            win.widgets.push(Widget::label(
                16,
                124,
                "Supports ATA/IDE disks and UHCI USB sticks (not NVMe/AHCI yet).",
            ));
            win.widgets
                .push(Widget::button(16, 200, 120, 36, "Next", WinAction::InstallerNext));
            win.widgets
                .push(Widget::button(148, 200, 100, 36, "Close", WinAction::Close));
        }
        1 => {
            win.widgets
                .push(Widget::label(16, 96, "Choose the target disk (will be wiped):"));
            let mut y = 128usize;
            if targets.is_empty() {
                win.widgets.push(Widget::label(
                    16,
                    y,
                    "No eligible target. Add another disk and reopen Installer.",
                ));
            } else {
                for (i, id) in targets.iter().enumerate() {
                    let mark = if selected == Some(i) { "[*] " } else { "[ ] " };
                    let label = alloc::format!("{}{}", mark, id.label());
                    win.widgets.push(Widget::button(
                        16,
                        y,
                        480,
                        32,
                        &label,
                        WinAction::InstallerPick(i as u8),
                    ));
                    y += 40;
                }
            }
            win.widgets
                .push(Widget::button(16, 340, 100, 36, "Back", WinAction::InstallerBack));
            win.widgets
                .push(Widget::button(128, 340, 120, 36, "Next", WinAction::InstallerNext));
        }
        2 => {
            let tgt = selected
                .and_then(|i| targets.get(i).copied())
                .map(|d| d.label())
                .unwrap_or("(none)");
            win.widgets.push(Widget::label(
                16,
                96,
                &alloc::format!("About to ERASE: {}", tgt),
            ));
            win.widgets.push(Widget::label(
                16,
                120,
                "Type ERASE below, then click Install.",
            ));
            win.widgets.push(Widget::textbox(16, 156, 200, 28));
            win.focused_widget = Some(win.widgets.len() - 1);
            win.replace_on_type = true;
            win.widgets
                .push(Widget::button(16, 220, 100, 36, "Back", WinAction::InstallerBack));
            win.widgets.push(Widget::button(
                128,
                220,
                140,
                36,
                "Install",
                WinAction::InstallerStart,
            ));
        }
        _ => {
            win.widgets.push(Widget::label(
                16,
                120,
                "You can remove the install USB and reboot from the target disk.",
            ));
            win.widgets.push(Widget::button(
                16,
                200,
                140,
                36,
                "Reboot",
                WinAction::InstallerReboot,
            ));
            win.widgets
                .push(Widget::button(168, 200, 100, 36, "Close", WinAction::Close));
        }
    }
}

fn installer_set_status(win: &mut Window, text: &str) {
    if let AppState::Installer { status, .. } = &mut win.app {
        status.clear();
        status.push_str(text);
    }
    // Keep the status label (index 2) in sync without a full rebuild mid-copy.
    if let Some(Widget::Label { text: t, .. }) = win.widgets.get_mut(2) {
        t.clear();
        t.push_str(text);
    }
}

fn installer_progress(done: u32, total: u32) {
    let pct = if total == 0 {
        100
    } else {
        ((done as u64 * 100) / total as u64) as u32
    };
    // Throttle UI redraws: every ~2% or at the end.
    if pct % 2 != 0 && done != total {
        return;
    }
    let msg = alloc::format!("Installing… {}% ({} / {} sectors)", pct, done, total);
    crate::serial_println!("{}", msg);
    {
        let mut wm = WM.lock();
        if let Some(win) = wm.windows.iter_mut().find(|w| w.kind == WindowKind::Installer) {
            installer_set_status(win, &msg);
            render_window(win, true);
        }
    }
    crate::framebuffer::present();
}

// ── Entry/exit ──────────────────────────────────────────────────────────────

/// Switch to the desktop.
///
/// Any windows already open stay open, so launching an app from the shell
/// (`pkg run <name>`) and then typing `gui` shows it. The desktop still comes
/// up empty in the usual flow because `exit_gui_mode` clears the window list
/// on the way out.
pub fn enter_gui_mode() {
    GUI_MODE.store(true, Ordering::Release);
    LAST_TICK_SEC.store(u64::MAX, Ordering::Release);
    request_redraw();
}

pub fn exit_gui_mode() {
    GUI_MODE.store(false, Ordering::Release);
    GUI_DIRTY.store(false, Ordering::Release);
    {
        let mut wm = WM.lock();
        wm.clear();
    }
    framebuffer::clear_screen();
}

// ── Event dispatch ──────────────────────────────────────────────────────────

pub fn handle_mouse_move(x: usize, y: usize) {
    let mut wm = WM.lock();
    wm.last_mouse = (x as i32, y as i32);

    // Active window drag?
    if let Some((idx, ox, oy)) = wm.dragging {
        if idx < wm.windows.len() {
            wm.windows[idx].x = x as i32 - ox;
            wm.windows[idx].y = y as i32 - oy;
        }
        drop(wm);
        request_redraw();
        return;
    }

    // Paint-canvas drag?
    if wm.mouse_left_down {
        if try_paint_drag(&mut wm, x as i32, y as i32) {
            drop(wm);
            request_redraw();
            return;
        }
        if try_code_area_drag_select(&mut wm, x as i32, y as i32) {
            drop(wm);
            request_redraw();
            return;
        }
    }

    // Hover tracking — only redraw when a button's hover state actually flips.
    if let Some(ti) = wm.topmost_idx() {
        let win = &mut wm.windows[ti];
        let hit = win.widget_at(x as i32, y as i32);
        let mut changed = false;
        for (i, widget) in win.widgets.iter_mut().enumerate() {
            if let Widget::Button { state, .. } = widget {
                let desired = if *state == BtnState::Pressed {
                    BtnState::Pressed
                } else if hit == Some(i) {
                    BtnState::Hover
                } else if *state == BtnState::Hover {
                    BtnState::Normal
                } else {
                    *state
                };
                if desired != *state {
                    *state = desired;
                    changed = true;
                }
            }
        }
        if changed {
            drop(wm);
            request_hover_redraw();
        }
    }
}

fn try_paint_drag(wm: &mut WindowManager, x: i32, y: i32) -> bool {
    let Some(ti) = wm.topmost_idx() else { return false; };
    let win = &mut wm.windows[ti];
    if win.kind != WindowKind::Paint { return false; }
    let (ox, oy) = win.content_origin();
    let mut canvas: Option<(i32, i32, i32, i32)> = None;
    for wgt in &win.widgets {
        if let Widget::Canvas { x: cx, y: cy, w: cw, h: ch } = wgt {
            canvas = Some((*cx as i32, *cy as i32, *cw as i32, *ch as i32));
            break;
        }
    }
    let Some((cx, cy, cw, ch)) = canvas else { return false; };
    let lx = x - ox;
    let ly = y - oy;
    if lx < cx || lx >= cx + cw || ly < cy || ly >= cy + ch { return false; }
    let rel_x = (lx - cx) as u16;
    let rel_y = (ly - cy) as u16;
    let AppState::Paint {
        canvas, cw: bw, ch: bh, drawing, last, color, brush, tool,
    } = &mut win.app else { return false; };
    if !*drawing { return false; }

    let ink = if *tool == PaintTool::Eraser { CANVAS_BG } else { *color };
    match *last {
        Some(prev) => paint_stroke(canvas, *bw, *bh, prev, (rel_x, rel_y), *brush, ink),
        None => paint_stamp(canvas, *bw, *bh, rel_x as i32, rel_y as i32, *brush, ink),
    }
    *last = Some((rel_x, rel_y));
    true
}

/// Click-and-drag text selection for the Code Editor: while the left button
/// is held and a `CodeArea` has focus (i.e. the drag started there — set by
/// the mousedown handler), each move extends the selection by moving the
/// cursor to the new position while leaving the anchor alone.
fn try_code_area_drag_select(wm: &mut WindowManager, x: i32, y: i32) -> bool {
    let Some(ti) = wm.topmost_idx() else { return false; };
    let win = &mut wm.windows[ti];
    let Some(wi) = win.focused_widget else { return false; };
    let (ox, oy) = win.content_origin();
    let Some(Widget::CodeArea { x: cx, y: cy, w, h: _, text, cursor, selection }) = win.widgets.get_mut(wi) else {
        return false;
    };
    if selection.is_none() {
        return false;
    }
    let lx = (x - ox) - *cx as i32;
    let ly = (y - oy) - *cy as i32;
    *cursor = code_area_cursor_at(text, *w, lx, ly);
    true
}

/// Right-click, list selection, and double-click (before the generic button path).
fn file_manager_intercept(
    mx: i32,
    my: i32,
    left: bool,
    prev_left: bool,
    right: bool,
    prev_right: bool,
    dbl: bool,
) -> bool {
    let le = left && !prev_left;
    let re = right && !prev_right;
    if !le && !re && !dbl {
        return false;
    }
    let mut wm = WM.lock();
    let Some(idx) = wm.topmost_at(mx, my) else {
        return false;
    };
    if wm.windows[idx].kind != WindowKind::FileManager {
        return false;
    }
    wm.raise(idx);
    let top = wm.windows.len() - 1;
    let win = &mut wm.windows[top];
    if win.in_close_btn(mx, my) || win.in_titlebar(mx, my) {
        return false;
    }
    let (handled, dbl_defer) = file_manager_handle_mouse(win, mx, my, le, re, dbl);
    drop(wm);
    if dbl_defer {
        file_manager_open_entry_internal();
    }
    if handled {
        request_redraw();
        return true;
    }
    false
}

/// List selection / double-click for Save As and Open dialogs.
fn file_dialog_intercept(
    mx: i32,
    my: i32,
    left: bool,
    prev_left: bool,
    dbl: bool,
) -> bool {
    let le = left && !prev_left;
    if !le && !dbl {
        return false;
    }
    let mut wm = WM.lock();
    let Some(idx) = wm.topmost_at(mx, my) else {
        return false;
    };
    if wm.windows[idx].kind != WindowKind::FileDialog {
        return false;
    }
    wm.raise(idx);
    let top = wm.windows.len() - 1;
    let win = &mut wm.windows[top];
    if win.in_close_btn(mx, my) || win.in_titlebar(mx, my) {
        return false;
    }
    let (handled, dbl_defer) = file_dialog_handle_mouse(win, mx, my, le || dbl, dbl);
    drop(wm);
    if dbl_defer {
        file_dialog_activate_selected();
    }
    if handled {
        request_redraw();
        return true;
    }
    false
}

pub fn handle_mouse_button(
    left: bool,
    prev_left: bool,
    right: bool,
    prev_right: bool,
    dbl: bool,
) -> Option<WinAction> {
    let (mx, my) = WM.lock().last_mouse;

    if file_dialog_intercept(mx, my, left, prev_left, dbl) {
        return Some(WinAction::None);
    }

    if file_manager_intercept(mx, my, left, prev_left, right, prev_right, dbl) {
        return Some(WinAction::None);
    }

    if browser_intercept(mx, my, right && !prev_right) {
        return Some(WinAction::None);
    }

    if left && !prev_left {
        // Restore a minimized window from the taskbar before other hit tests.
        if let Some(idx) = taskbar_minimized_at(mx, my) {
            let mut wm = WM.lock();
            wm.raise(idx);
            drop(wm);
            request_redraw();
            return Some(WinAction::None);
        }

        let mut wm = WM.lock();
        wm.mouse_left_down = true;
        let hit = wm.topmost_at(mx, my);
        match hit {
            Some(idx) => {
                wm.raise(idx);
                let top = wm.windows.len() - 1;
                let win = &mut wm.windows[top];

                if win.in_close_btn(mx, my) {
                    let closed = wm.windows.pop();
                    // Dragging holds an index; removing a window shifts every
                    // later one, so a stale index would move the wrong window.
                    wm.dragging = None;
                    drop(wm);
                    if let Some(pid) = closed.and_then(|w| w.owner_pid) {
                        // Closing the window of an ELF app terminates it.
                        // Without this the process lived on forever holding a
                        // handle to a window that no longer exists.
                        crate::process::terminate(pid);
                    }
                    request_redraw();
                    return Some(WinAction::None);
                }
                if win.in_min_btn(mx, my) {
                    win.apply_minimize();
                    wm.dragging = None;
                    drop(wm);
                    request_redraw();
                    return Some(WinAction::None);
                }
                if win.in_mid_btn(mx, my) {
                    win.apply_restore_mid();
                    wm.dragging = None;
                    drop(wm);
                    request_redraw();
                    return Some(WinAction::None);
                }
                if win.in_max_btn(mx, my) {
                    win.apply_maximize();
                    wm.dragging = None;
                    drop(wm);
                    request_redraw();
                    return Some(WinAction::None);
                }
                if win.in_titlebar(mx, my) {
                    // Don't start a drag while maximized — mid first.
                    if win.maximized {
                        drop(wm);
                        request_redraw();
                        return Some(WinAction::None);
                    }
                    let ox = mx - win.x;
                    let oy = my - win.y;
                    wm.dragging = Some((top, ox, oy));
                    drop(wm);
                    request_redraw();
                    return Some(WinAction::None);
                }

                let hit_w = win.widget_at(mx, my);
                if let Some(wi) = hit_w {
                    let (ox, oy) = win.content_origin();
                    match &mut win.widgets[wi] {
                        Widget::TextBox { .. } => {
                            // Clicking into a one-line field selects it, the
                            // way an address bar does everywhere else.
                            let refocused = win.focused_widget != Some(wi);
                            win.focused_widget = Some(wi);
                            win.replace_on_type = refocused;
                            // The address bar and a field on the page cannot
                            // both be what the keyboard is talking to.
                            browser_blur_field(win);
                        }
                        Widget::TextArea { .. } => {
                            win.focused_widget = Some(wi);
                            win.replace_on_type = false;
                        }
                        Widget::CodeArea { x, y, w, h: _, text, cursor, selection } => {
                            win.focused_widget = Some(wi);
                            win.replace_on_type = false;
                            let lx = (mx - ox) - *x as i32;
                            let ly = (my - oy) - *y as i32;
                            let at = code_area_cursor_at(text, *w, lx, ly);
                            if dbl {
                                let chars: Vec<char> = text.chars().collect();
                                let (start, end) = word_bounds_at(&chars, at);
                                *selection = Some(start);
                                *cursor = end;
                            } else {
                                *cursor = at;
                                *selection = Some(at);
                            }
                        }
                        Widget::OutputArea { .. } => {}
                        Widget::Button { state, action, .. } => {
                            *state = BtnState::Pressed;
                            let act = *action;
                            drop(wm);
                            request_redraw();
                            return Some(act);
                        }
                        Widget::Checkbox { checked, action, .. } => {
                            *checked = !*checked;
                            let act = *action;
                            drop(wm);
                            request_redraw();
                            return Some(act);
                        }
                        Widget::Swatch { action, .. } => {
                            let act = *action;
                            drop(wm);
                            request_redraw();
                            return Some(act);
                        }
                        Widget::Canvas { x, y, w, h } => {
                            // Clicking the page takes focus off the address
                            // bar, so the arrow keys scroll rather than being
                            // swallowed by a text field the user has finished
                            // with.
                            win.focused_widget = None;
                            let cx = *x as i32;
                            let cy = *y as i32;
                            let cw = *w as i32;
                            let ch = *h as i32;
                            let lx = mx - ox;
                            let ly = my - oy;
                            if lx >= cx && lx < cx + cw && ly >= cy && ly < cy + ch {
                                let rel_x = (lx - cx) as u16;
                                let rel_y = (ly - cy) as u16;
                                if matches!(win.app, AppState::Browser { .. }) {
                                    // Follow a link if the click landed on one.
                                    // Navigation blocks on the network, so it
                                    // must happen after the manager lock is
                                    // dropped.
                                    browser_close_menu(win);
                                    let (px, py) = (rel_x as usize, rel_y as usize);
                                    drop(wm);
                                    if let Some(target) = browser_click(px, py) {
                                        browser_navigate(&target, true);
                                    }
                                    request_redraw();
                                    return Some(WinAction::None);
                                }
                                if let AppState::Paint {
                                    canvas, cw, ch, drawing, last, color, brush, tool,
                                } = &mut win.app
                                {
                                    match tool {
                                        PaintTool::Fill => {
                                            paint_fill(canvas, *cw, *ch,
                                                       rel_x as usize, rel_y as usize, *color);
                                        }
                                        PaintTool::Brush | PaintTool::Eraser => {
                                            let ink = if *tool == PaintTool::Eraser {
                                                CANVAS_BG
                                            } else {
                                                *color
                                            };
                                            paint_stamp(canvas, *cw, *ch,
                                                        rel_x as i32, rel_y as i32, *brush, ink);
                                            *drawing = true;
                                            *last = Some((rel_x, rel_y));
                                        }
                                    }
                                }
                            }
                        }
                        Widget::Label { .. } => {}
                    }
                } else {
                    win.focused_widget = None;
                }
                drop(wm);
                request_redraw();
                return Some(WinAction::None);
            }
            None => {
                // No window hit — check the desktop chrome. The taskbar's
                // Applications button opens the Applications window; the
                // desktop icons open their respective windows on double
                // click (real-desktop feel).
                drop(wm);
                if taskbar_apps_btn_at(mx, my) {
                    return Some(WinAction::OpenApplications);
                }
                if taskbar_shutdown_btn_at(mx, my) {
                    return Some(WinAction::Shutdown);
                }
                if dbl {
                    if let Some(i) = desktop_icon_at(mx, my) {
                        return Some(match DESKTOP_ICONS.get(i).map(|(_, _, kind)| *kind) {
                            Some(WindowKind::MyComputer) => WinAction::OpenMyComputer,
                            Some(WindowKind::FileManager) => WinAction::OpenDrive(3),
                            Some(WindowKind::Browser) => WinAction::OpenBrowser,
                            Some(WindowKind::Launcher) => WinAction::OpenApplications,
                            Some(WindowKind::Installer) => WinAction::OpenInstaller,
                            _ => WinAction::None,
                        });
                    }
                }
                return Some(WinAction::None);
            }
        }
    }

    if !left && prev_left {
        let mut wm = WM.lock();
        wm.mouse_left_down = false;
        wm.dragging = None;
        let mut changed = false;
        for win in wm.windows.iter_mut() {
            for widget in win.widgets.iter_mut() {
                if let Widget::Button { state, .. } = widget {
                    if *state == BtnState::Pressed {
                        *state = BtnState::Normal;
                        changed = true;
                    }
                }
            }
            if let AppState::Paint { drawing, last, .. } = &mut win.app {
                if *drawing {
                    *drawing = false;
                    *last = None;
                }
            }
        }
        if changed { drop(wm); request_redraw(); }
        return Some(WinAction::None);
    }

    Some(WinAction::None)
}

/// Route a wheel movement to whatever is under it.
///
/// Only the browser scrolls today. The wheel reports up as negative, which is
/// the opposite of the scroll direction, hence the negation.
pub fn handle_mouse_wheel(delta: i8) -> Option<WinAction> {
    let wm = WM.lock();
    let top = wm.topmost_idx()?;
    if wm.windows[top].kind != WindowKind::Browser {
        return None;
    }
    drop(wm);
    Some(WinAction::BrowserScroll(-delta.clamp(-3, 3)))
}

pub fn handle_key(k: DecodedKey) -> Option<WinAction> {
    // ESC exits GUI mode.
    if let DecodedKey::Unicode('\x1b') = k {
        return Some(WinAction::ExitGui);
    }
    if let DecodedKey::RawKey(KeyCode::Escape) = k {
        return Some(WinAction::ExitGui);
    }

    // F2 opens the app launcher from anywhere, the way a desktop key does on
    // a real system. Checked before the per-window routing below so it works
    // even when an app has focus.
    if let DecodedKey::RawKey(KeyCode::F2) = k {
        launch_builtin(WindowKind::Launcher);
        return Some(WinAction::None);
    }

    let mut wm = WM.lock();
    let top = match wm.topmost_idx() { Some(i) => i, None => return None };
    let win = &mut wm.windows[top];

    // Snake-specific key routing (arrow keys, R to restart).
    if win.kind == WindowKind::Snake {
        let mut consumed = false;
        match k {
            DecodedKey::RawKey(KeyCode::ArrowUp) => {
                if let AppState::Snake { pending_dir, dir, .. } = &mut win.app {
                    if dir.1 != 1 { *pending_dir = (0, -1); consumed = true; }
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) => {
                if let AppState::Snake { pending_dir, dir, .. } = &mut win.app {
                    if dir.1 != -1 { *pending_dir = (0, 1); consumed = true; }
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowLeft) => {
                if let AppState::Snake { pending_dir, dir, .. } = &mut win.app {
                    if dir.0 != 1 { *pending_dir = (-1, 0); consumed = true; }
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowRight) => {
                if let AppState::Snake { pending_dir, dir, .. } = &mut win.app {
                    if dir.0 != -1 { *pending_dir = (1, 0); consumed = true; }
                }
            }
            DecodedKey::Unicode('r') | DecodedKey::Unicode('R') => {
                drop(wm);
                snake_restart();
                return Some(WinAction::None);
            }
            _ => {}
        }
        if consumed {
            drop(wm);
            request_redraw();
            return Some(WinAction::None);
        }
    }

    // Breakout paddle + speed.
    if win.kind == WindowKind::Breakout {
        let mut consumed = false;
        match k {
            DecodedKey::RawKey(KeyCode::ArrowLeft) | DecodedKey::Unicode('a') | DecodedKey::Unicode('A') => {
                if let AppState::Breakout(state) = &mut win.app {
                    state.nudge_paddle(-1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowRight) | DecodedKey::Unicode('d') | DecodedKey::Unicode('D') => {
                if let AppState::Breakout(state) = &mut win.app {
                    state.nudge_paddle(1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowUp) | DecodedKey::Unicode('w') | DecodedKey::Unicode('W') => {
                if let AppState::Breakout(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) | DecodedKey::Unicode('s') | DecodedKey::Unicode('S') => {
                if let AppState::Breakout(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::Unicode('r') | DecodedKey::Unicode('R') => {
                drop(wm);
                breakout_restart();
                return Some(WinAction::None);
            }
            _ => {}
        }
        if consumed {
            drop(wm);
            request_redraw();
            return Some(WinAction::None);
        }
    }

    // Race Cars steering + speed.
    if win.kind == WindowKind::Racing {
        let mut consumed = false;
        match k {
            DecodedKey::RawKey(KeyCode::ArrowLeft) | DecodedKey::Unicode('a') | DecodedKey::Unicode('A') => {
                if let AppState::Racing(state) = &mut win.app {
                    state.nudge(-1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowRight) | DecodedKey::Unicode('d') | DecodedKey::Unicode('D') => {
                if let AppState::Racing(state) = &mut win.app {
                    state.nudge(1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowUp) | DecodedKey::Unicode('w') | DecodedKey::Unicode('W') => {
                if let AppState::Racing(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) | DecodedKey::Unicode('s') | DecodedKey::Unicode('S') => {
                if let AppState::Racing(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::Unicode('r') | DecodedKey::Unicode('R') => {
                drop(wm);
                racing_restart();
                return Some(WinAction::None);
            }
            _ => {}
        }
        if consumed {
            drop(wm);
            request_redraw();
            return Some(WinAction::None);
        }
    }

    // Space Invaders.
    if win.kind == WindowKind::Invaders {
        let mut consumed = false;
        match k {
            DecodedKey::RawKey(KeyCode::ArrowLeft) | DecodedKey::Unicode('a') | DecodedKey::Unicode('A') => {
                if let AppState::Invaders(state) = &mut win.app {
                    state.nudge(-1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowRight) | DecodedKey::Unicode('d') | DecodedKey::Unicode('D') => {
                if let AppState::Invaders(state) = &mut win.app {
                    state.nudge(1);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowUp) | DecodedKey::Unicode('w') | DecodedKey::Unicode('W') => {
                if let AppState::Invaders(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::RawKey(KeyCode::ArrowDown) | DecodedKey::Unicode('s') | DecodedKey::Unicode('S') => {
                if let AppState::Invaders(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                    consumed = true;
                }
            }
            DecodedKey::Unicode(' ') => {
                if let AppState::Invaders(state) = &mut win.app {
                    state.fire();
                    consumed = true;
                }
            }
            DecodedKey::Unicode('r') | DecodedKey::Unicode('R') => {
                drop(wm);
                invaders_restart();
                return Some(WinAction::None);
            }
            _ => {}
        }
        if consumed {
            drop(wm);
            request_redraw();
            return Some(WinAction::None);
        }
    }

    // Terminal routing: the Terminal window owns its own input buffer.
    // All keystrokes go to it regardless of `focused_widget`.
    if win.kind == WindowKind::Terminal {
        let submit_cmd: Option<String> = match k {
            DecodedKey::Unicode('\x08') => {
                if let AppState::Terminal { input, blink_on, .. } = &mut win.app {
                    input.pop();
                    *blink_on = true;
                }
                None
            }
            DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                if let AppState::Terminal { input, blink_on, .. } = &mut win.app {
                    *blink_on = true;
                    let cmd = input.clone();
                    input.clear();
                    Some(cmd)
                } else {
                    None
                }
            }
            DecodedKey::Unicode(c) if (c >= ' ' && c != '\x7f') => {
                if let AppState::Terminal { input, blink_on, .. } = &mut win.app {
                    if input.chars().count() < 240 {
                        input.push(c);
                    }
                    *blink_on = true;
                }
                None
            }
            _ => { drop(wm); return None; }
        };
        if let Some(cmd) = submit_cmd {
            let trimmed = cmd.trim().to_string();
            if let AppState::Terminal { scrollback, .. } = &mut win.app {
                scrollback.push(alloc::format!("{} {}", TERM_PROMPT, trimmed));
            }
            if !trimmed.is_empty() {
                let output = run_terminal_command(&trimmed);
                if output == "\x01CLEAR\x01" {
                    if let AppState::Terminal { scrollback, .. } = &mut win.app {
                        scrollback.clear();
                    }
                } else if !output.is_empty() {
                    if let AppState::Terminal { scrollback, .. } = &mut win.app {
                        for line in output.split('\n') {
                            scrollback.push(String::from(line));
                        }
                    }
                }
            }
            // Cap scrollback to a reasonable budget.
            if let AppState::Terminal { scrollback, .. } = &mut win.app {
                let cap = 300usize;
                if scrollback.len() > cap {
                    let drop_n = scrollback.len() - cap;
                    scrollback.drain(0..drop_n);
                }
            }
        }
        drop(wm);
        request_redraw();
        return Some(WinAction::None);
    }

    // A field on the page comes before the page's own keys: while the caret is
    // in one, the space bar types a space rather than scrolling a screenful.
    if win.kind == WindowKind::Browser && win.focused_widget != Some(BROWSER_ADDRESS_BAR) {
        if let Some(action) = browser_field_key(win, k) {
            drop(wm);
            request_redraw();
            return Some(action);
        }
    }

    // With no caret on the page, the keystroke is still the page's to hear
    // about: a page with keyboard shortcuts listens on the document, and one that
    // calls `preventDefault` means "that key was mine", which is what stops the
    // arrows from also scrolling.
    if win.kind == WindowKind::Browser && win.focused_widget != Some(BROWSER_ADDRESS_BAR) {
        if let AppState::Browser { session: Some(session), .. } = &mut win.app {
            let detail = browser_key_detail(k);
            if !session.dispatch_on_document("keydown", &detail).allows_default() {
                drop(wm);
                request_redraw();
                return Some(WinAction::None);
            }
        }
    }

    // Browser scrolling. The address bar keeps the arrow keys when it has
    // focus, so this only fires when the page itself is what the user is
    // looking at.
    if win.kind == WindowKind::Browser && win.focused_widget != Some(BROWSER_ADDRESS_BAR) {
        let pages = |n: i8| Some(WinAction::BrowserScroll(n));
        let scroll = match k {
            DecodedKey::RawKey(KeyCode::ArrowDown) => pages(1),
            DecodedKey::RawKey(KeyCode::ArrowUp) => pages(-1),
            DecodedKey::RawKey(KeyCode::PageDown) => pages(3),
            DecodedKey::RawKey(KeyCode::PageUp) => pages(-3),
            DecodedKey::Unicode(' ') => pages(3),
            DecodedKey::RawKey(KeyCode::Home) => Some(WinAction::BrowserScrollTo(0)),
            DecodedKey::RawKey(KeyCode::End) => Some(WinAction::BrowserScrollTo(usize::MAX)),
            _ => None,
        };
        if scroll.is_some() {
            drop(wm);
            return scroll;
        }
    }

    let Some(fw) = win.focused_widget else { drop(wm); return None; };
    let win_kind = win.kind;
    // Any keystroke ends the just-clicked state, whether or not it replaces
    // anything — one keypress, one chance.
    let replace = core::mem::take(&mut win.replace_on_type);
    match &mut win.widgets[fw] {
        Widget::TextBox { text, w, .. } => {
            if replace && matches!(k, DecodedKey::Unicode(c) if c != '\n' && c != '\r') {
                text.clear();
            }
            match k {
                DecodedKey::Unicode('\x08') => { text.pop(); }
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                    // Enter in the address bar loads the page, as it would in
                    // any browser.
                    if win_kind == WindowKind::Browser {
                        drop(wm);
                        return Some(WinAction::BrowserGo);
                    }
                }
                DecodedKey::Unicode(c) => {
                    let max_chars = (w.saturating_sub(8)) / CHAR_W.max(1);
                    if text.chars().count() < max_chars.saturating_mul(4) {
                        text.push(c);
                    }
                }
                _ => { drop(wm); return None; }
            }
            drop(wm);
            request_redraw();
            Some(WinAction::None)
        }
        Widget::TextArea { text, .. } => {
            match k {
                DecodedKey::Unicode('\x08') => { text.pop(); }
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => { text.push('\n'); }
                DecodedKey::Unicode(c) => {
                    if text.len() < 4096 { text.push(c); }
                }
                _ => { drop(wm); return None; }
            }
            if win.kind == WindowKind::Notepad {
                notepad_refresh_status(win);
            }
            drop(wm);
            request_redraw();
            Some(WinAction::None)
        }
        Widget::CodeArea { w, text, cursor, selection, .. } => {
            const CAP: usize = 16384;
            // A stale cursor (e.g. left over from longer text that was just
            // replaced by New/Open) is clamped rather than trusted, so a
            // keystroke can never index past the current buffer.
            let char_len = text.chars().count();
            if *cursor > char_len { *cursor = char_len; }
            if let Some(a) = selection { if *a > char_len { *a = char_len; } }
            let max_chars = code_area_max_chars(*w);

            // Ctrl chords: checked before the plain-character arms below so
            // e.g. Ctrl+C copies instead of typing a literal "c" (`Ignore`
            // mode means the letter still decodes the same either way — see
            // `keyboard::ctrl_held`'s doc comment).
            if crate::keyboard::ctrl_held() {
                if let DecodedKey::Unicode(c) = k {
                    match c.to_ascii_lowercase() {
                        'c' | 'x' => {
                            if let Some((lo, hi)) = code_area_selected_range(*cursor, *selection) {
                                let byte_lo = char_to_byte_idx(text, lo);
                                let byte_hi = char_to_byte_idx(text, hi);
                                *TEXT_CLIPBOARD.lock() = String::from(&text[byte_lo..byte_hi]);
                                if c == 'x' {
                                    text.replace_range(byte_lo..byte_hi, "");
                                    *cursor = lo;
                                    *selection = None;
                                }
                            }
                            code_editor_refresh_status(win);
                            drop(wm);
                            request_redraw();
                            return Some(WinAction::None);
                        }
                        'v' => {
                            code_area_delete_selection(text, cursor, selection);
                            let clip = TEXT_CLIPBOARD.lock().clone();
                            if !clip.is_empty() && text.chars().count() + clip.chars().count() < CAP {
                                let byte_idx = char_to_byte_idx(text, *cursor);
                                text.insert_str(byte_idx, &clip);
                                *cursor += clip.chars().count();
                            }
                            code_editor_refresh_status(win);
                            drop(wm);
                            request_redraw();
                            return Some(WinAction::None);
                        }
                        'a' => {
                            *selection = Some(0);
                            *cursor = text.chars().count();
                            drop(wm);
                            request_redraw();
                            return Some(WinAction::None);
                        }
                        _ => {}
                    }
                }
            }

            match k {
                DecodedKey::Unicode('\x08') => {
                    if !code_area_delete_selection(text, cursor, selection) && *cursor > 0 {
                        let byte_idx = char_to_byte_idx(text, *cursor - 1);
                        text.remove(byte_idx);
                        *cursor -= 1;
                    }
                }
                DecodedKey::RawKey(KeyCode::Delete) => {
                    if !code_area_delete_selection(text, cursor, selection) && *cursor < char_len {
                        let byte_idx = char_to_byte_idx(text, *cursor);
                        text.remove(byte_idx);
                    }
                }
                DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
                    code_area_delete_selection(text, cursor, selection);
                    if text.chars().count() < CAP {
                        let byte_idx = char_to_byte_idx(text, *cursor);
                        text.insert(byte_idx, '\n');
                        *cursor += 1;
                    }
                }
                DecodedKey::Unicode('\t') => {
                    code_area_delete_selection(text, cursor, selection);
                    if text.chars().count() + 4 < CAP {
                        let byte_idx = char_to_byte_idx(text, *cursor);
                        text.insert_str(byte_idx, "    ");
                        *cursor += 4;
                    }
                }
                DecodedKey::Unicode(c) => {
                    code_area_delete_selection(text, cursor, selection);
                    if text.chars().count() < CAP {
                        let byte_idx = char_to_byte_idx(text, *cursor);
                        text.insert(byte_idx, c);
                        *cursor += 1;
                    }
                }
                DecodedKey::RawKey(KeyCode::ArrowLeft) => {
                    match code_area_selected_range(*cursor, *selection) {
                        Some((lo, _)) => *cursor = lo,
                        None => *cursor = cursor.saturating_sub(1),
                    }
                    *selection = None;
                }
                DecodedKey::RawKey(KeyCode::ArrowRight) => {
                    match code_area_selected_range(*cursor, *selection) {
                        Some((_, hi)) => *cursor = hi,
                        None => *cursor = (*cursor + 1).min(char_len),
                    }
                    *selection = None;
                }
                DecodedKey::RawKey(KeyCode::ArrowUp) | DecodedKey::RawKey(KeyCode::ArrowDown) => {
                    *selection = None;
                    let chars: Vec<char> = text.chars().collect();
                    let rows = wrap_rows(&chars, max_chars);
                    let (row, col) = cursor_row_col(&rows, *cursor);
                    let target = if matches!(k, DecodedKey::RawKey(KeyCode::ArrowUp)) {
                        row.checked_sub(1)
                    } else if row + 1 < rows.len() {
                        Some(row + 1)
                    } else {
                        None
                    };
                    if let Some(r) = target {
                        let (start, end) = rows[r];
                        *cursor = (start + col).min(end);
                    }
                }
                DecodedKey::RawKey(KeyCode::Home) => {
                    *selection = None;
                    let chars: Vec<char> = text.chars().collect();
                    let rows = wrap_rows(&chars, max_chars);
                    let (row, _) = cursor_row_col(&rows, *cursor);
                    *cursor = rows[row].0;
                }
                DecodedKey::RawKey(KeyCode::End) => {
                    *selection = None;
                    let chars: Vec<char> = text.chars().collect();
                    let rows = wrap_rows(&chars, max_chars);
                    let (row, _) = cursor_row_col(&rows, *cursor);
                    *cursor = rows[row].1;
                }
                _ => { drop(wm); return None; }
            }
            code_editor_refresh_status(win);
            drop(wm);
            request_redraw();
            Some(WinAction::None)
        }
        _ => { drop(wm); None }
    }
}

fn raise_or_spawn(wm: &mut WindowManager, kind: WindowKind, make: fn() -> Window) {
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == kind) {
        wm.raise(idx);
        return;
    }
    wm.spawn(make());
}

fn run_terminal_command(cmd: &str) -> String {
    let c = cmd.trim();
    if c.is_empty() {
        return String::new();
    }
    if c == "help" {
        return String::from(
            "Commands:\n  help                 show this message\n  clear                clear the screen\n  ls [path]            list files (/, /fat, /init)\n  cat <path>           print a file\n  uptime               seconds since boot\n  mem                  heap usage\n  version              OS101 version\n  echo <text>          print text",
        );
    }
    if c == "clear" {
        // Special sentinel handled by the caller.
        return String::from("\x01CLEAR\x01");
    }
    if c == "uptime" {
        let t = crate::clock::ticks();
        let secs = t / 18;
        let h = secs / 3600;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        return alloc::format!("up {:02}:{:02}:{:02}  ({} ticks)", h, m, s, t);
    }
    if c == "mem" {
        return alloc::format!(
            "heap: {} / {} bytes used",
            crate::allocator::used(),
            crate::allocator::size(),
        );
    }
    if c == "version" {
        return alloc::format!("OS101 v{}", env!("CARGO_PKG_VERSION"));
    }
    if c == "ls" {
        return match crate::fs::cmd_ls(Some("/")) {
            Ok(v) => v.join("\n"),
            Err(e) => alloc::format!("ls: {}", e),
        };
    }
    if let Some(path) = c.strip_prefix("ls ") {
        return match crate::fs::cmd_ls(Some(path.trim())) {
            Ok(v) => v.join("\n"),
            Err(e) => alloc::format!("ls: {}", e),
        };
    }
    if let Some(path) = c.strip_prefix("cat ") {
        return match crate::fs::cmd_cat(path.trim()) {
            Ok(v) => match core::str::from_utf8(&v) {
                Ok(t) => String::from(t.trim_end_matches('\n')),
                Err(_) => alloc::format!("binary: {} bytes", v.len()),
            },
            Err(e) => alloc::format!("cat: {}", e),
        };
    }
    if c == "echo" {
        return String::new();
    }
    if let Some(rest) = c.strip_prefix("echo ") {
        return String::from(rest);
    }
    alloc::format!("unknown command: {} — try `help`", c)
}

// ── Per-tick updates: clock/monitor + snake game-step ───────────────────────

pub fn tick(now_ticks: u64) {
    let mut wm = WM.lock();
    let sec = now_ticks / 18;
    let prev_sec = LAST_TICK_SEC.load(Ordering::Acquire);
    let mut changed = false;

    if sec != prev_sec {
        for win in wm.windows.iter_mut() {
            if win.kind == WindowKind::Monitor {
                let h = sec / 3600;
                let m = (sec / 60) % 60;
                let s = sec % 60;
                set_label_text(win, 0, &alloc::format!("uptime: Boot+{:02}:{:02}:{:02}", h, m, s));
                set_label_text(win, 1, &alloc::format!("ticks: {}", now_ticks));
                set_label_text(
                    win, 2,
                    &alloc::format!("heap: {} / {} bytes", crate::allocator::used(), crate::allocator::size()),
                );
                changed = true;
            }
        }
        LAST_TICK_SEC.store(sec, Ordering::Release);
    }

    // Blink the Terminal cursor every ~9 ticks (≈500ms at 18Hz).
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Terminal { continue; }
        if let AppState::Terminal { blink_on, last_blink_tick, .. } = &mut win.app {
            if now_ticks.saturating_sub(*last_blink_tick) >= 9 {
                *blink_on = !*blink_on;
                *last_blink_tick = now_ticks;
                changed = true;
            }
        }
    }

    // Give every open page's script engine its turn: the timers that are due,
    // the animation callbacks that are waiting, and any promise job the last
    // handler queued. Without this a page that continues in a `setTimeout` or a
    // `.then` would only ever continue while something else was calling into it.
    //
    // The pump is skipped entirely when nothing is queued, which is the usual
    // case, so a static page costs one flag read per pass rather than a call
    // into the engine.
    let mut script_navigation: Option<String> = None;
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Browser {
            continue;
        }
        let AppState::Browser { session: Some(session), scroll, .. } = &mut win.app else {
            continue;
        };
        // Taken before anything else, and whether or not there is a timer to run:
        // this is the catch-all for a handler that assigned to `location` from
        // somewhere the caller does not check afterwards — a keystroke, say.
        // Navigating has to happen after the lock is dropped, so it is carried out
        // rather than followed here.
        if script_navigation.is_none() {
            script_navigation = session.take_pending_navigation();
        }
        if !session.has_pending_work() {
            continue;
        }
        if session.pump() {
            // A timer may have shortened the page out from under the viewport.
            let limit = session.page.height() as usize;
            let viewport = BROWSER_CANVAS_H.saturating_sub(2 * BROWSER_PADDING);
            *scroll = (*scroll).min(limit.saturating_sub(viewport));
            changed = true;
        }
        if script_navigation.is_none() {
            script_navigation = session.take_pending_navigation();
        }
    }

    // Step Snake windows (~5 timer ticks ≈ 275ms — gentler for little hands).
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Snake || win.minimized { continue; }
        let step_needed = if let AppState::Snake { last_step_ticks, game_over, .. } = &win.app {
            !*game_over && now_ticks.saturating_sub(*last_step_ticks) >= 5
        } else { false };
        if step_needed {
            snake_step(win, now_ticks);
            changed = true;
        }
    }

    // Step Breakout (~every tick for smooth ball).
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Breakout || win.minimized { continue; }
        let step_needed = if let AppState::Breakout(state) = &win.app {
            !state.game_over && !state.won && now_ticks.saturating_sub(state.last_step_ticks) >= 1
        } else {
            false
        };
        if step_needed {
            if let AppState::Breakout(state) = &mut win.app {
                state.last_step_ticks = now_ticks;
                state.step();
                let line = state.status_line();
                set_label_text(win, 2, &line);
            }
            changed = true;
        }
    }

    // Step Race Cars.
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Racing || win.minimized { continue; }
        let step_needed = if let AppState::Racing(state) = &win.app {
            !state.game_over && now_ticks.saturating_sub(state.last_step_ticks) >= 2
        } else {
            false
        };
        if step_needed {
            if let AppState::Racing(state) = &mut win.app {
                state.last_step_ticks = now_ticks;
                state.step();
                let line = state.status_line();
                set_label_text(win, 2, &line);
            }
            changed = true;
        }
    }

    // Step Space Invaders.
    for win in wm.windows.iter_mut() {
        if win.kind != WindowKind::Invaders || win.minimized { continue; }
        let step_needed = if let AppState::Invaders(state) = &win.app {
            !state.game_over && !state.won && now_ticks.saturating_sub(state.last_step_ticks) >= 2
        } else {
            false
        };
        if step_needed {
            if let AppState::Invaders(state) = &mut win.app {
                state.last_step_ticks = now_ticks;
                state.step();
                let line = state.status_line();
                set_label_text(win, 2, &line);
            }
            changed = true;
        }
    }

    drop(wm);
    if changed {
        request_redraw();
    }
    // Last, and outside the lock: navigating fetches, and fetching takes the
    // lock again to report progress.
    if let Some(url) = script_navigation {
        browser_navigate(&url, true);
    }
}

fn snake_step(win: &mut Window, now_ticks: u64) {
    let new_label = {
        let AppState::Snake {
            grid_w, grid_h, snake, dir, pending_dir, food,
            game_over, score, rng, last_step_ticks,
        } = &mut win.app else { return; };
        *last_step_ticks = now_ticks;

        // Accept pending direction unless it's a direct reversal.
        if !(pending_dir.0 + dir.0 == 0 && pending_dir.1 + dir.1 == 0) {
            *dir = *pending_dir;
        }

        let (hx, hy) = snake[0];
        let nhx = hx + dir.0 as i16;
        let nhy = hy + dir.1 as i16;

        let mut end_game = false;
        if nhx < 0 || nhy < 0 || nhx >= *grid_w as i16 || nhy >= *grid_h as i16 {
            end_game = true;
        } else if snake.iter().any(|s| *s == (nhx, nhy)) {
            end_game = true;
        } else {
            let ate = (nhx, nhy) == *food;
            snake.insert(0, (nhx, nhy));
            if ate {
                *score += 1;
                crate::sound::blip();
                // Respawn food on an empty cell so it never hides under the snake.
                for _ in 0..64 {
                    *rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    let fx = (*rng % *grid_w as u32) as i16;
                    let fy = ((*rng / 7) % *grid_h as u32) as i16;
                    if !snake.iter().any(|s| *s == (fx, fy)) {
                        *food = (fx, fy);
                        break;
                    }
                }
            } else {
                snake.pop();
            }
        }
        if end_game {
            *game_over = true;
            crate::sound::boom();
        }

        if *game_over {
            alloc::format!("Score: {}   Oh no!", *score)
        } else {
            alloc::format!("Score: {}", *score)
        }
    };
    set_label_text(win, 2, &new_label);
}

// ── Apply action ────────────────────────────────────────────────────────────

fn with_top<F: FnOnce(&mut Window)>(kind: WindowKind, f: F) {
    let mut wm = WM.lock();
    if let Some(idx) = wm.windows.iter().rposition(|w| w.kind == kind) {
        f(&mut wm.windows[idx]);
    }
    drop(wm);
    request_redraw();
}

/// Like [`with_top`], but passes a value back out.
///
/// The window manager lock is released before returning, so the caller is
/// free to do more work — including anything that locks it again.
fn with_top_ret<T, F: FnOnce(&mut Window) -> T>(kind: WindowKind, f: F) -> Option<T> {
    let mut wm = WM.lock();
    let result = wm
        .windows
        .iter()
        .rposition(|w| w.kind == kind)
        .map(|idx| f(&mut wm.windows[idx]));
    drop(wm);
    request_redraw();
    result
}


fn apply_op(a: f64, b: f64, op: char) -> f64 {
    match op {
        '+' => a + b,
        '-' => a - b,
        '*' => a * b,
        '/' => if b == 0.0 { 0.0 } else { a / b },
        _ => b,
    }
}

fn format_f64(v: f64) -> String {
    let mut s = alloc::format!("{:.6}", v);
    if s.contains('.') {
        while s.ends_with('0') { s.pop(); }
        if s.ends_with('.') { s.pop(); }
    }
    s
}

fn calc_apply_i64(a: i64, b: i64, op: char) -> Option<i64> {
    match op {
        '+' => a.checked_add(b),
        '-' => a.checked_sub(b),
        '*' => a.checked_mul(b),
        '/' => {
            if b == 0 { None } else { a.checked_div(b) }
        }
        _ => Some(b),
    }
}

fn calc_refresh(win: &mut Window) {
    let (lhs, op, entry) = match &win.app {
        AppState::Calculator { lhs, op, entry, .. } => (*lhs, *op, entry.clone()),
        _ => return,
    };
    let shown = if entry.chars().count() < 20 {
        let mut s = String::new();
        for _ in 0..(20 - entry.chars().count()) {
            s.push(' ');
        }
        s.push_str(&entry);
        s
    } else {
        entry
    };
    set_label_text(win, 0, &shown);
    let footer = if let (Some(a), Some(p)) = (lhs, op) {
        alloc::format!("{} {} ...", a, p)
    } else {
        String::new()
    };
    win.footer = Some(footer);
}

fn notepad_save() {
    // Copy editor text, then open Save As so the user picks name + folder.
    with_top(WindowKind::Notepad, |win| {
        if let Some(Widget::TextArea { text, .. }) = win.widgets.get(4) {
            *SAVE_AS_PENDING.lock() = text.clone();
            *NOTEPAD_SCRATCH.lock() = text.clone();
        }
    });
    open_file_dialog(FileDialogMode::Save, FileDialogTarget::Notepad);
}

fn notepad_load() {
    open_file_dialog(FileDialogMode::Open, FileDialogTarget::Notepad);
}

fn notepad_clear() {
    with_top(WindowKind::Notepad, |win| {
        if let Some(Widget::TextArea { text, .. }) = win.widgets.get_mut(4) {
            text.clear();
        }
        notepad_refresh_status(win);
    });
}

fn open_file_dialog(mode: FileDialogMode, target: FileDialogTarget) {
    *FILE_DIALOG_TARGET.lock() = target;
    {
        let mut path = FILE_DIALOG_PATH.lock();
        if path.is_empty() || !fm_is_writable_path(path.as_str()) {
            *path = if crate::fs::has_disk() {
                String::from("/disk")
            } else {
                String::from("/data")
            };
        }
    }
    let mut wm = WM.lock();
    // One dialog at a time.
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
        wm.windows.remove(idx);
    }
    wm.spawn(create_file_dialog_window(mode));
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
        refresh_file_dialog(&mut wm.windows[idx]);
    }
    drop(wm);
    request_redraw();
}

fn create_file_dialog_window(mode: FileDialogMode) -> Window {
    let title = match mode {
        FileDialogMode::Save => "Save As",
        FileDialogMode::Open => "Open File",
    };
    let confirm = match mode {
        FileDialogMode::Save => "Save",
        FileDialogMode::Open => "Open",
    };
    let confirm_act = match mode {
        FileDialogMode::Save => WinAction::FileDialogSave,
        FileDialogMode::Open => WinAction::FileDialogOpen,
    };
    let default_name = match mode {
        FileDialogMode::Save => {
            let target = *FILE_DIALOG_TARGET.lock();
            let (p, fallback) = match target {
                FileDialogTarget::Notepad => (NOTEPAD_PATH.lock().clone(), "notepad.c"),
                FileDialogTarget::CodeEditor => (CODE_PATH.lock().clone(), "untitled.c"),
            };
            if p.is_empty() {
                String::from(fallback)
            } else {
                String::from(fm_display_name(&p))
            }
        }
        FileDialogMode::Open => String::new(),
    };
    let mut w = Window::new(300, 100, 480, 340, title)
        .with_kind(WindowKind::FileDialog)
        .with_app(AppState::FileDialog {
            mode,
            entries: Vec::new(),
            selected: None,
        })
        .with_footer("Pick a folder, type a name, then Save / Open. Writable: /disk /data")
        .add(Widget::label(10, 8, "Location:"));
    for i in 0..FD_ROWS {
        w = w.add(Widget::label(30, FD_LIST_Y0 + i * FM_ROW_H, ""));
    }
    w = w
        .add(Widget::button(10, 28, 56, 22, "Up", WinAction::FileDialogUp))
        .add(Widget::textbox(10, 220, 280, 28))
        .add(Widget::button(300, 218, 80, 32, confirm, confirm_act))
        .add(Widget::button(388, 218, 80, 32, "Cancel", WinAction::FileDialogCancel));
    if let Some(Widget::TextBox { text, .. }) = w.widgets.get_mut(FD_NAME_BOX) {
        text.push_str(&default_name);
    }
    w.focused_widget = Some(FD_NAME_BOX);
    w.replace_on_type = true;
    w
}

fn refresh_file_dialog(win: &mut Window) {
    let path = FILE_DIALOG_PATH.lock().clone();
    let shown = if path.is_empty() { "/" } else { path.as_str() };
    let files = match crate::fs::cmd_ls(Some(shown)) {
        Ok(v) => v,
        Err(e) => {
            set_label_text(win, 0, &alloc::format!("Location: {} — error: {}", shown, e));
            Vec::new()
        }
    };
    set_label_text(
        win,
        0,
        &alloc::format!("Location: {}  —  {} items", shown, files.len().min(FD_ROWS)),
    );
    for i in 0..FD_ROWS {
        let txt = files
            .get(i)
            .map(|s| fm_display_name(s))
            .unwrap_or("");
        let mark = if matches!(&win.app, AppState::FileDialog { selected: Some(s), .. } if *s == i) {
            "> "
        } else {
            "  "
        };
        set_label_text(win, 1 + i, &alloc::format!("{}{}", mark, txt));
    }
    if let AppState::FileDialog { entries, selected, .. } = &mut win.app {
        *entries = files;
        *selected = None;
    }
}

fn file_dialog_up() {
    let mut path = FILE_DIALOG_PATH.lock();
    *path = crate::fs::path_parent(path.as_str());
    if path.is_empty() {
        *path = String::from("/");
    }
    drop(path);
    let mut wm = WM.lock();
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
        refresh_file_dialog(&mut wm.windows[idx]);
    }
    drop(wm);
    request_redraw();
}

fn file_dialog_close() {
    let mut wm = WM.lock();
    if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
        wm.windows.remove(idx);
        wm.dragging = None;
    }
    drop(wm);
    request_redraw();
}

fn file_dialog_confirm_save() {
    let dir = FILE_DIALOG_PATH.lock().clone();
    let mut name = String::new();
    {
        let wm = WM.lock();
        let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) else {
            return;
        };
        if let Some(Widget::TextBox { text, .. }) = wm.windows[idx].widgets.get(FD_NAME_BOX) {
            name.push_str(text.trim());
        }
    }
    if name.is_empty() || name.contains('/') {
        with_top(WindowKind::FileDialog, |win| {
            win.footer = Some(String::from("Enter a file name (no slashes)."));
        });
        request_redraw();
        return;
    }
    let parent = if dir.is_empty() || dir == "/" {
        if crate::fs::has_disk() {
            String::from("/disk")
        } else {
            String::from("/data")
        }
    } else {
        dir.trim_end_matches('/').to_string()
    };
    if !fm_is_writable_path(&parent) {
        with_top(WindowKind::FileDialog, |win| {
            win.footer = Some(String::from("Folder is read-only. Use /disk or /data."));
        });
        request_redraw();
        return;
    }
    let full = alloc::format!("{}/{}", parent, name);
    let data = SAVE_AS_PENDING.lock().clone();
    let _ = crate::fs::cmd_mkdir(&parent);
    match crate::fs::cmd_write_file(&full, data.as_bytes().to_vec()) {
        Ok(()) => {
            match *FILE_DIALOG_TARGET.lock() {
                FileDialogTarget::Notepad => {
                    *NOTEPAD_PATH.lock() = full.clone();
                    *NOTEPAD_SCRATCH.lock() = data;
                    file_dialog_close();
                    notepad_set_path_label();
                    with_top(WindowKind::Notepad, |win| {
                        win.footer = Some(alloc::format!("Saved to {}", full));
                    });
                }
                FileDialogTarget::CodeEditor => {
                    *CODE_PATH.lock() = full.clone();
                    *CODE_SCRATCH.lock() = data;
                    file_dialog_close();
                    code_editor_set_path_label();
                    with_top(WindowKind::CodeEditor, |win| {
                        win.footer = Some(alloc::format!("Saved to {}", full));
                    });
                }
            }
            request_redraw();
        }
        Err(e) => {
            with_top(WindowKind::FileDialog, |win| {
                win.footer = Some(alloc::format!("Save failed: {}", e));
            });
            request_redraw();
        }
    }
}

fn file_dialog_confirm_open() {
    enum Next {
        Navigate(String),
        Open(String),
        None,
    }
    let next = {
        let wm = WM.lock();
        let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) else {
            return;
        };
        let win = &wm.windows[idx];
        let AppState::FileDialog { entries, selected, .. } = &win.app else {
            return;
        };
        if let Some(i) = *selected {
            if let Some(ent) = entries.get(i) {
                if ent.ends_with('/') {
                    Next::Navigate(ent.trim_end_matches('/').to_string())
                } else {
                    Next::Open(ent.clone())
                }
            } else {
                Next::None
            }
        } else {
            let mut name = String::new();
            if let Some(Widget::TextBox { text, .. }) = win.widgets.get(FD_NAME_BOX) {
                name.push_str(text.trim());
            }
            if name.is_empty() {
                Next::None
            } else {
                let dir = FILE_DIALOG_PATH.lock().clone();
                let parent = if dir.is_empty() || dir == "/" {
                    String::from("/disk")
                } else {
                    dir.trim_end_matches('/').to_string()
                };
                Next::Open(alloc::format!("{}/{}", parent, name))
            }
        }
    };
    match next {
        Next::Navigate(p) => {
            *FILE_DIALOG_PATH.lock() = p;
            let mut wm = WM.lock();
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
                refresh_file_dialog(&mut wm.windows[idx]);
            }
            request_redraw();
        }
        Next::Open(p) => file_dialog_open_path(&p),
        Next::None => {
            with_top(WindowKind::FileDialog, |win| {
                win.footer = Some(String::from("Select a file or type a name."));
            });
            request_redraw();
        }
    }
}

fn file_dialog_open_path(path: &str) {
    if path.ends_with('/') {
        *FILE_DIALOG_PATH.lock() = path.trim_end_matches('/').to_string();
        let mut wm = WM.lock();
        if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
            refresh_file_dialog(&mut wm.windows[idx]);
        }
        drop(wm);
        request_redraw();
        return;
    }
    let body = match crate::fs::cmd_cat(path) {
        Ok(bytes) => match core::str::from_utf8(&bytes) {
            Ok(s) => String::from(s),
            Err(_) => {
                with_top(WindowKind::FileDialog, |win| {
                    win.footer = Some(String::from("File is not UTF-8 text."));
                });
                request_redraw();
                return;
            }
        },
        Err(e) => {
            with_top(WindowKind::FileDialog, |win| {
                win.footer = Some(alloc::format!("Open failed: {}", e));
            });
            request_redraw();
            return;
        }
    };
    match *FILE_DIALOG_TARGET.lock() {
        FileDialogTarget::Notepad => {
            *NOTEPAD_SCRATCH.lock() = body.clone();
            *NOTEPAD_PATH.lock() = String::from(path);
            with_top(WindowKind::Notepad, |win| {
                if let Some(Widget::TextArea { text, .. }) = win.widgets.get_mut(4) {
                    text.clear();
                    text.push_str(&body);
                }
                notepad_refresh_status(win);
            });
        }
        FileDialogTarget::CodeEditor => {
            *CODE_SCRATCH.lock() = body.clone();
            *CODE_PATH.lock() = String::from(path);
            with_top(WindowKind::CodeEditor, |win| {
                if let Some(Widget::CodeArea { text, cursor, selection, .. }) = win.widgets.get_mut(6) {
                    text.clear();
                    text.push_str(&body);
                    *cursor = text.chars().count();
                    *selection = None;
                }
                if let Some(Widget::OutputArea { text, error, .. }) = win.widgets.get_mut(7) {
                    text.clear();
                    *error = false;
                }
                code_editor_refresh_status(win);
            });
        }
    }
    file_dialog_close();
}

fn file_dialog_handle_mouse(win: &mut Window, mx: i32, my: i32, left_edge: bool, dbl: bool) -> (bool, bool) {
    if !left_edge {
        return (false, false);
    }
    if win.widget_at(mx, my).is_some() {
        return (false, false);
    }
    let (_ox, oy) = win.content_origin();
    let ly = my - oy;
    if ly < FD_LIST_Y0 as i32 {
        return (true, false);
    }
    let row = ((ly as usize).saturating_sub(FD_LIST_Y0)) / FM_ROW_H;
    if row >= FD_ROWS {
        return (true, false);
    }
    let (ent, labels) = {
        let AppState::FileDialog { entries, selected, .. } = &mut win.app else {
            return (true, false);
        };
        if row >= entries.len() {
            *selected = None;
            return (true, false);
        }
        *selected = Some(row);
        let ent = entries[row].clone();
        let labels: Vec<String> = (0..FD_ROWS)
            .map(|i| {
                let txt = entries
                    .get(i)
                    .map(|s| fm_display_name(s))
                    .unwrap_or("");
                let mark = if Some(i) == *selected { "> " } else { "  " };
                alloc::format!("{}{}", mark, txt)
            })
            .collect();
        (ent, labels)
    };
    if !ent.ends_with('/') {
        if let Some(Widget::TextBox { text, .. }) = win.widgets.get_mut(FD_NAME_BOX) {
            text.clear();
            text.push_str(fm_display_name(&ent));
        }
    }
    for (i, lab) in labels.iter().enumerate() {
        set_label_text(win, 1 + i, lab);
    }
    (true, dbl)
}

/// Double-click open for the file dialog (called after WM lock dropped).
fn file_dialog_activate_selected() {
    let path = {
        let wm = WM.lock();
        let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) else {
            return;
        };
        let win = &wm.windows[idx];
        let AppState::FileDialog { entries, selected, mode } = &win.app else {
            return;
        };
        let Some(i) = *selected else { return; };
        let Some(ent) = entries.get(i) else { return; };
        if ent.ends_with('/') {
            let p = ent.trim_end_matches('/').to_string();
            drop(wm);
            *FILE_DIALOG_PATH.lock() = p;
            let mut wm = WM.lock();
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileDialog) {
                refresh_file_dialog(&mut wm.windows[idx]);
            }
            request_redraw();
            return;
        }
        if *mode == FileDialogMode::Open {
            Some(ent.clone())
        } else {
            None
        }
    };
    if let Some(p) = path {
        file_dialog_open_path(&p);
    }
}

// ── Web browser ─────────────────────────────────────────────────────────────

const BROWSER_HOME: &str = crate::browser::search::HOME;
/// Widgets past this index belong to the context menu, and are thrown away
/// when it closes — the same trick the file manager uses.
const BROWSER_WIDGET_BASE: usize = 8;
/// What the browser's context menu was opened over: the address of a picture,
/// a link, or nothing. Only one menu can be open, so one slot is enough.
static BROWSER_MENU: Mutex<BrowserMenu> = Mutex::new(BrowserMenu {
    image: None,
    link: None,
});

struct BrowserMenu {
    image: Option<String>,
    link: Option<String>,
}

/// Where downloaded pictures go. A real folder on the data disk, so they are
/// still there after a reboot.
const DOWNLOADS: &str = "/disk/downloads";

/// Most pictures a single page will fetch. Each one is a blocking HTTP request
/// on the same thread as the GUI, so a page full of them would otherwise wedge
/// the machine for a minute at a time.
const MAX_PAGE_IMAGES: usize = 12;
/// Canvas geometry. The page is laid out to [`BROWSER_VIEWPORT_W`], which is
/// the canvas minus the inner padding and the scrollbar gutter.
const BROWSER_CANVAS_W: usize = 92 * CHAR_W + 8;
const BROWSER_CANVAS_H: usize = 26 * ROW_H + 8;
const BROWSER_PADDING: usize = 4;
const BROWSER_GUTTER: usize = 8;
const BROWSER_VIEWPORT_W: usize = BROWSER_CANVAS_W - 2 * BROWSER_PADDING - BROWSER_GUTTER;
/// How far the Up/Down buttons and the wheel move the page.
const BROWSER_SCROLL_STEP: usize = 4 * ROW_H;

const BROWSER_PAGE_BG: Color = Color::hex(0xFBFCFD);

/// Form controls on a page. The canvas is a light surface whichever theme the
/// desktop is wearing, so these come from the same palette as the user-agent
/// stylesheet rather than from [`theme`], whose fields are drawn for dark
/// chrome and would be all but invisible here.
const BROWSER_FIELD_BG: Color = Color::hex(0xFFFFFF);
const BROWSER_FIELD_EDGE: Color = Color::hex(0x94A3B8);
const BROWSER_FIELD_FOCUS: Color = Color::hex(0x2F6FEB);
const BROWSER_FIELD_FLAT: Color = Color::hex(0xF1F5F9);
const BROWSER_BUTTON_BG: Color = Color::hex(0xE2E8F0);
const BROWSER_FIELD_TEXT: Color = Color::hex(0x0F172A);
/// Index of the address bar in the browser window's widget list.
const BROWSER_ADDRESS_BAR: usize = 2;

/// Font metrics of the page canvas, shared by layout and painting.
const BROWSER_METRICS: crate::browser::Metrics = crate::browser::Metrics {
    char_w: CHAR_W as f32,
    line_h: ROW_H as f32,
};

/// What `vw`, `vh` and the font-relative units resolve against.
const BROWSER_VIEWPORT: crate::browser::Viewport = crate::browser::Viewport {
    width: BROWSER_VIEWPORT_W as f32,
    height: (BROWSER_CANVAS_H - 2 * BROWSER_PADDING) as f32,
    char_w: CHAR_W as f32,
    line_h: ROW_H as f32,
};

fn create_browser_window() -> Window {
    let w = BROWSER_CANVAS_W + 20;
    // Address bar, page, scroll buttons, then the chrome around them.
    let content_h = 42 + BROWSER_CANVAS_H + 6 + 26 + 8;
    let h = TITLEBAR_H + content_h + FOOTER_H + BORDER;

    let mut win = Window::new(120, 60, w, h, "Web Browser")
        .with_kind(WindowKind::Browser)
        .with_footer("Type words to search, or an address to go there. Right-click a picture to save it.")
        .with_app(AppState::Browser {
            session: None,
            scroll: 0,
            status: String::from("Ready."),
            history: Vec::new(),
            current: String::new(),
        })
        .add(Widget::button(10, 8, 54, 26, "Back", WinAction::BrowserBack))
        .add(Widget::button(68, 8, 54, 26, "Home", WinAction::BrowserHome))
        .add(Widget::textbox(126, 8, w.saturating_sub(126 + 152), 26))
        .add(Widget::button(w.saturating_sub(146), 8, 70, 26, "Images", WinAction::BrowserImages))
        .add(Widget::button(w.saturating_sub(72), 8, 54, 26, "Go", WinAction::BrowserGo))
        .add(Widget::canvas(10, 42, BROWSER_CANVAS_W, BROWSER_CANVAS_H));

    let bar_y = 42 + BROWSER_CANVAS_H + 6;
    win = win
        .add(Widget::button(10, bar_y, 54, 26, "Up", WinAction::BrowserScroll(-1)))
        .add(Widget::button(68, bar_y, 54, 26, "Down", WinAction::BrowserScroll(1)));

    // Pre-fill the address bar so Go works straight away.
    if let Some(Widget::TextBox { text, .. }) = win.widgets.get_mut(BROWSER_ADDRESS_BAR) {
        text.push_str(BROWSER_HOME);
    }
    win
}

/// Read the URL currently in the address bar.
fn browser_url(win: &Window) -> String {
    match win.widgets.get(BROWSER_ADDRESS_BAR) {
        Some(Widget::TextBox { text, .. }) => text.clone(),
        _ => String::new(),
    }
}

fn browser_set_url(win: &mut Window, url: &str) {
    if let Some(Widget::TextBox { text, .. }) = win.widgets.get_mut(BROWSER_ADDRESS_BAR) {
        text.clear();
        text.push_str(url);
    }
}

/// Fetch and display a URL in the topmost browser window.
///
/// The fetch blocks: the network stack is polled, not threaded, so the whole
/// GUI pauses until the page arrives or the request times out. The status
/// line is painted first so the window does not look frozen.
fn browser_navigate(url: &str, record_history: bool) {
    let url = url.trim().to_string();
    if url.is_empty() {
        return;
    }

    // Show "Loading" before the blocking fetch starts.
    let previous = with_top_ret(WindowKind::Browser, |win| {
        let prev = match &win.app {
            AppState::Browser { current, .. } => current.clone(),
            _ => String::new(),
        };
        if let AppState::Browser { status, .. } = &mut win.app {
            *status = alloc::format!("Loading {} ...", url);
            win.footer = Some(status.clone());
        }
        prev
    });
    render();
    framebuffer::present();

    // A page the browser writes itself needs no network at all.
    if let Some(html) = crate::browser::search::internal_page(&url) {
        browser_show(&url, html, None, record_history, previous);
        return;
    }

    if !crate::net::is_configured() {
        // Bring the interface up on first use rather than making the user
        // run `net up` in the shell before opening the browser.
        let _ = crate::net::autoconfigure();
    }

    let result = browser_fetch_page(&url);

    match result {
        Ok(Fetched { response, notice }) => {
            let is_image = response.content_type.starts_with("image/");
            let decoded = if is_image {
                crate::image::decode(&response.body).map(alloc::sync::Arc::new)
            } else {
                None
            };
            match decoded {
                Some(image) => {
                    // Opening a picture directly: build a page around it and
                    // hand over the pixels we already have rather than asking
                    // the network for them a second time.
                    let html = browser_image_document(&response.final_url, &image);
                    browser_show(
                        &response.final_url,
                        html,
                        Some((response.final_url.clone(), image)),
                        record_history,
                        previous,
                    );
                }
                None if is_image => {
                    let detail = alloc::format!(
                        "{} is a {} picture, which this browser cannot decode.",
                        url, response.content_type,
                    );
                    let document =
                        crate::browser::error_document("Unsupported picture", &detail);
                    browser_show(&url, document, None, record_history, previous);
                }
                None => {
                    let status = alloc::format!("HTTP {}", response.status);
                    let mut body = response.body_text();
                    if let Some(notice) = &notice {
                        body = crate::browser::search::with_notice(&body, notice);
                    }
                    browser_show_document(
                        &response.final_url,
                        body,
                        None,
                        record_history,
                        previous,
                        &status,
                    );
                }
            }
        }
        Err(e) => {
            // Show the failure as a page, the way a real browser does, so the
            // previous document does not stay on screen implying it is still
            // what the address bar points at.
            let detail = alloc::format!("{} could not be loaded: {}", url, e);
            let document = crate::browser::error_document("Cannot reach this page", &detail);
            browser_show_document(
                &url,
                document,
                None,
                record_history,
                previous,
                &alloc::format!("Could not load {}: {}", url, e),
            );
        }
    }

    // Reflect redirects in the address bar.
    with_top(WindowKind::Browser, |win| {
        let final_url = match &win.app {
            AppState::Browser { current, .. } => current.clone(),
            _ => String::new(),
        };
        if !final_url.is_empty() {
            browser_set_url(win, &final_url);
        }
    });
}

/// A fetched page, and a note about it if it is not what was asked for.
struct Fetched {
    response: crate::net::http::Response,
    /// Set when the content came from somewhere other than the address
    /// requested. Rendered as a banner above the page, because silently
    /// substituting one site's answer for another's would be a lie.
    notice: Option<alloc::string::String>,
}

/// Fetch a page, with the two recoveries today's web needs.
///
/// The first is for a host with no TLS at all: a bare hostname is taken to
/// mean HTTPS, so somewhere like `info.cern.ch` — still happily serving the
/// first website over plain HTTP — would otherwise be unreachable by name.
///
/// The second is Google. Its results page is a JavaScript program, not a
/// document, so when one arrives the same query is run somewhere that answers
/// in HTML and the result is labelled as such. See [`crate::browser::search`]
/// for what was tried before settling on this.
fn browser_fetch_page(url: &str) -> Result<Fetched, alloc::string::String> {
    use crate::browser::search;

    let response = match crate::net::http::get(url) {
        Ok(response) => response,
        Err(secure_error) => match search::plain_fallback(url) {
            // Report the original failure if the retry fails too: the secure
            // attempt is the one the user asked for, so its error is the
            // informative one.
            Some(plain) => crate::net::http::get(&plain).map_err(|_| secure_error)?,
            None => return Err(secure_error),
        },
    };

    if !response.content_type.starts_with("image/") && search::is_google_page(&response.final_url) {
        return Ok(google_page(response));
    }

    Ok(Fetched { response, notice: None })
}

/// Ask Google what the user might mean by `query`.
///
/// Google Suggest is the last part of Google that answers a browser like this
/// one, so it is worth a request of its own: it is the only genuinely Google
/// content on the page. A failure here is not worth reporting — the page is
/// perfectly usable without completions.
fn google_suggestions(query: &str) -> alloc::vec::Vec<String> {
    if query.is_empty() {
        return alloc::vec::Vec::new();
    }
    match crate::net::http::get(&crate::browser::search::suggest_url(query)) {
        Ok(reply) => crate::browser::search::suggestions(&reply.body_text()),
        Err(_) => alloc::vec::Vec::new(),
    }
}

/// Turn what Google sent into something a reader can use.
///
/// Google's homepage and its results page are both JavaScript programs that
/// build themselves in the browser, and neither carries a single result in its
/// HTML — this was checked against every request shape that used to produce a
/// server-rendered page, including Google's own no-JavaScript retry flow, and
/// Google's internal result endpoints answer a client like this one with a
/// captcha. So the response is fetched from Google, over TLS, and then stood
/// in for: the homepage becomes OS101's Google, and a search becomes the same
/// query answered by an engine that still serves HTML, under a Google header
/// that says exactly that.
fn google_page(response: crate::net::http::Response) -> Fetched {
    use crate::browser::search;

    let query = search::query_of(&response.final_url).unwrap_or_default();

    // Each of these is a TLS handshake and a round trip on a network stack
    // that is polled from this same thread, so the window would otherwise sit
    // frozen with "Loading" on it for the length of all three.
    if !query.is_empty() {
        browser_status("Google needs JavaScript — asking it what you might mean ...");
    }
    let suggestions = google_suggestions(&query);

    // No query means the homepage, which has nothing to fall back to: there
    // are no results to show, only a box to type in.
    if query.is_empty() {
        return Fetched {
            response: crate::net::http::Response {
                status: response.status,
                content_type: String::from("text/html"),
                body: search::google_page("", &suggestions).into_bytes(),
                final_url: response.final_url,
            },
            notice: None,
        };
    }

    browser_status(&alloc::format!("Searching for {} where results are HTML ...", query));
    match crate::net::http::get(&search::fallback_search_url(&query)) {
        Ok(results) => {
            let notice =
                search::google_header(&query, &suggestions, "DuckDuckGo", &response.final_url);
            Fetched { response: results, notice: Some(notice) }
        }
        // The other engine is unreachable too, so show the box and the
        // completions rather than Google's empty program.
        Err(_) => Fetched {
            response: crate::net::http::Response {
                status: response.status,
                content_type: String::from("text/html"),
                body: search::google_page(&query, &suggestions).into_bytes(),
                final_url: response.final_url,
            },
            notice: None,
        },
    }
}

/// Open the browser, with its start page already on screen.
///
/// An empty canvas under a filled-in address bar reads as broken. The start
/// page is written by the kernel, not fetched, so drawing it immediately costs
/// nothing and no network.
fn open_browser() {
    let mut wm = WM.lock();
    raise_or_spawn(&mut wm, WindowKind::Browser, create_browser_window);
    let blank = wm.windows.iter().any(|w| {
        w.kind == WindowKind::Browser && matches!(&w.app, AppState::Browser { session: None, .. })
    });
    drop(wm);
    if blank {
        browser_navigate(crate::browser::search::HOME, false);
    }
    request_redraw();
}

fn browser_go() {
    let typed = with_top_ret(WindowKind::Browser, |win| browser_url(win));
    if let Some(typed) = typed {
        browser_navigate(&crate::browser::search::address_to_url(&typed), true);
    }
}

/// Search for pictures matching whatever the address bar holds.
fn browser_images() {
    let typed = with_top_ret(WindowKind::Browser, |win| browser_url(win)).unwrap_or_default();
    // An address in the bar is not a search term; fall back to a general
    // request rather than searching for "http://example.com".
    let query = if crate::browser::search::looks_like_url(&typed) {
        String::new()
    } else {
        typed
    };
    browser_navigate(&crate::browser::search::image_search_url(&query), true);
}

fn browser_reload() {
    let current = with_top_ret(WindowKind::Browser, |win| match &win.app {
        AppState::Browser { current, .. } => current.clone(),
        _ => String::new(),
    })
    .unwrap_or_default();
    if !current.is_empty() {
        browser_navigate(&current, false);
    }
}

// ── Pictures on a page ──────────────────────────────────────────────────────

/// Show a document: parse it, run its scripts, put it on screen, and then go
/// back for its pictures.
///
/// `preloaded` is a picture the caller already has in hand, which is how
/// opening an image URL avoids fetching the same bytes twice.
fn browser_show(
    url: &str,
    html: String,
    preloaded: Option<(String, alloc::sync::Arc<crate::image::Image>)>,
    record_history: bool,
    previous: Option<String>,
) {
    browser_show_document(url, html, preloaded, record_history, previous, "");
}

fn browser_show_document(
    url: &str,
    html: String,
    preloaded: Option<(String, alloc::sync::Arc<crate::image::Image>)>,
    record_history: bool,
    previous: Option<String>,
    note: &str,
) {
    let page = crate::browser::render(&html, BROWSER_VIEWPORT, BROWSER_METRICS);
    let mut session = crate::browser::script::Session::new(page, url);

    // Scripts run once the document is parsed, and may rewrite it before it is
    // ever shown. This is also where the engine was created — four frames below
    // the main loop — because the stack budget QuickJS enforces is measured from
    // wherever that happened.
    // One deadline for all of this page's external scripts between them, because
    // the count limit alone does not bound the wait: Wikipedia's startup module is
    // a quarter of a megabyte over a TLS stack that is polled from this very
    // thread, and four of those is a minute of frozen window.
    let script_deadline = crate::clock::micros().saturating_add(SCRIPT_FETCH_BUDGET_MICROS);
    session.run_scripts_with(|url| browser_fetch_script(url, script_deadline));
    let errors = core::mem::take(&mut session.errors);

    // A page that fetched fine and then paints nothing after its scripts have
    // run is one whose content this browser genuinely cannot assemble. Saying so
    // beats handing the user a blank rectangle and letting them conclude the
    // browser is broken.
    if !html.is_empty() && !session.page.has_visible_content() {
        let explained = crate::browser::search::with_notice(
            &html,
            "Nothing on this page is written in the HTML, and running its \
             JavaScript did not produce anything either — it is probably built \
             from data this browser cannot fetch. \
             <b>Type what you want in the address bar above</b> to search instead.",
        );
        let page = crate::browser::render(&explained, BROWSER_VIEWPORT, BROWSER_METRICS);
        session = crate::browser::script::Session::new(page, url);
        session.run_scripts();
    }

    if let Some((src, image)) = preloaded {
        session.page.images.insert(&src, image);
        session.page.relayout();
    }

    let title = session.page.title.clone();
    let rows = (session.page.height() as usize) / ROW_H;
    let summary = match (errors.first(), title.is_empty()) {
        (Some(e), _) => alloc::format!("{} — script error: {}", note, e),
        (None, true) => alloc::format!("{} — {} lines", note, rows),
        (None, false) => alloc::format!("{} — {}, {} lines", title, note, rows),
    };
    let summary = summary.trim_start_matches(" — ").to_string();

    with_top(WindowKind::Browser, |win| {
        browser_close_menu(win);
        let AppState::Browser { session: slot, scroll, status, history, current } = &mut win.app
        else {
            return;
        };
        *slot = Some(session);
        *scroll = 0;
        if record_history {
            if let Some(prev) = previous.as_ref().filter(|p| !p.is_empty()) {
                if history.len() < 32 {
                    history.push(prev.clone());
                }
            }
        }
        *current = String::from(url);
        *status = summary.clone();
        win.footer = Some(status.clone());
    });

    // Reflect redirects and internal pages in the address bar.
    with_top(WindowKind::Browser, |win| browser_set_url(win, url));

    browser_fetch_page_images(url, &summary);
    // A load-time script may have assigned to `location`, which cannot navigate
    // from inside the engine — the window manager's lock is held there — so it
    // was recorded and is followed now.
    browser_follow_script_navigation();
}

/// How long a page's external scripts may take to arrive, all of them together.
///
/// Six seconds is about two round trips to a slow host on this network stack, and
/// well inside what a person will wait for a page. Past it the remaining scripts
/// are skipped and said to have been skipped — a page whose script arrives late is
/// a page missing a feature, whereas a browser that stops answering is a broken
/// machine.
const SCRIPT_FETCH_BUDGET_MICROS: u64 = 6_000_000;

/// Fetch a `<script src>` over the same HTTP/TLS client the page came through,
/// and therefore under the same per-request timeouts.
///
/// The address has already been resolved against the document's by the session.
/// A non-JavaScript answer is refused rather than evaluated: a login page served
/// where a script was expected would otherwise be a syntax error attributed to
/// the site.
fn browser_fetch_script(url: &str, deadline: u64) -> Option<String> {
    if crate::clock::micros() >= deadline {
        return None;
    }
    browser_status(&alloc::format!("Fetching script {} ...", url));
    let response = crate::net::http::get(url).ok()?;
    if response.content_type.starts_with("text/html") || response.content_type.starts_with("image/") {
        return None;
    }
    Some(response.body_text())
}

/// Follow a navigation a script asked for, once the window manager's lock is
/// free.
///
/// `location.href = ...` cannot navigate from inside the engine: the engine is
/// running under the lock and `browser_navigate` takes it again. So a native
/// records the address and every path that runs script calls this afterwards.
fn browser_follow_script_navigation() {
    let target = with_top_ret(WindowKind::Browser, |win| match &mut win.app {
        AppState::Browser { session: Some(session), .. } => session.take_pending_navigation(),
        _ => None,
    })
    .flatten();
    let Some(url) = target else { return };

    // A page whose load-time script assigns to `location` sends us back through
    // `browser_navigate`, which lands here again. Left alone, a pair of pages
    // that redirect to each other would recurse until the kernel stack ran out;
    // this is the same ceiling the HTTP client puts on redirects.
    if SCRIPT_NAV_DEPTH.load(Ordering::Relaxed) >= MAX_SCRIPT_NAV {
        browser_status("A script kept redirecting this page, so the browser stopped following it.");
        return;
    }
    SCRIPT_NAV_DEPTH.fetch_add(1, Ordering::Relaxed);
    browser_navigate(&url, true);
    SCRIPT_NAV_DEPTH.fetch_sub(1, Ordering::Relaxed);
}

/// How deep a chain of script-driven navigations may go before the browser stops
/// following it. Each level is a real frame on the kernel stack.
const MAX_SCRIPT_NAV: usize = 3;
static SCRIPT_NAV_DEPTH: AtomicUsize = AtomicUsize::new(0);

/// Fetch and decode the pictures the page on screen asks for, then lay it out
/// again so they appear.
///
/// This blocks, exactly as the page fetch does, because the network stack is
/// polled on this same thread. Hence the cap on how many pictures one page may
/// load, and the running count in the status line: without it the machine
/// would simply look frozen.
fn browser_fetch_page_images(base: &str, summary: &str) {
    let sources = with_top_ret(WindowKind::Browser, |win| match &win.app {
        AppState::Browser { session: Some(s), .. } => s.page.image_sources(),
        _ => Vec::new(),
    })
    .unwrap_or_default();
    if sources.is_empty() {
        return;
    }

    let wanted: Vec<String> = sources.into_iter().take(MAX_PAGE_IMAGES).collect();
    let mut fetched = Vec::with_capacity(wanted.len());
    for (i, src) in wanted.iter().enumerate() {
        browser_status(&alloc::format!(
            "{} — picture {} of {} ...",
            summary,
            i + 1,
            wanted.len()
        ));
        let resolved = crate::browser::resolve_url(base, src);
        fetched.push((src.clone(), browser_fetch_image(&resolved)));
    }

    let shown = fetched.iter().filter(|(_, img)| img.is_some()).count();
    with_top(WindowKind::Browser, |win| {
        let AppState::Browser { session: Some(session), status, scroll, .. } = &mut win.app else {
            return;
        };
        for (src, image) in fetched {
            match image {
                Some(image) => session.page.images.insert(&src, image),
                None => session.page.images.fail(&src),
            }
        }
        session.page.relayout();
        *scroll = 0;
        *status = alloc::format!("{} — {} of {} pictures", summary, shown, wanted.len());
        win.footer = Some(status.clone());
    });
    request_redraw();
}

/// Fetch one picture and decode it, going through the proxy when it cannot be
/// had over plain HTTP or arrives in a format the kernel cannot read.
fn browser_fetch_image(url: &str) -> Option<alloc::sync::Arc<crate::image::Image>> {
    let direct = url.starts_with("http://");
    let first = if direct {
        String::from(url)
    } else {
        crate::browser::search::image_gateway(url, BROWSER_VIEWPORT_W)
    };

    if let Some(image) = browser_decode_url(&first) {
        return Some(image);
    }
    if direct {
        // A plain-HTTP picture that would not decode is usually a GIF or a
        // WebP. Asking the proxy for it again with `output=jpg` turns it into
        // something the kernel does understand, which is cheaper by far than
        // carrying a decoder for every format the web has accumulated.
        return browser_decode_url(&crate::browser::search::image_gateway(url, BROWSER_VIEWPORT_W));
    }
    None
}

fn browser_decode_url(url: &str) -> Option<alloc::sync::Arc<crate::image::Image>> {
    let response = crate::net::http::get(url).ok()?;
    if response.status != 200 {
        return None;
    }
    crate::image::decode(&response.body).map(alloc::sync::Arc::new)
}

/// Wrap a picture that was navigated to directly in a page of its own, the way
/// a browser does when you open an image URL.
fn browser_image_document(url: &str, image: &crate::image::Image) -> String {
    alloc::format!(
        "<html><head><title>{}</title></head>\
         <body style=\"background-color:#0F172A; color:#94A3B8; padding:12px\">\
         <p>{} × {} — right-click to save it or make it the wallpaper.</p>\
         <img src=\"{}\" alt=\"picture\"></body></html>",
        crate::browser::escape(&crate::browser::search::filename_for(url, "jpg")),
        image.width,
        image.height,
        crate::browser::escape(url),
    )
}

// ── The browser's context menu ──────────────────────────────────────────────

fn browser_close_menu(win: &mut Window) {
    if win.widgets.len() > BROWSER_WIDGET_BASE {
        win.widgets.truncate(BROWSER_WIDGET_BASE);
    }
}

fn browser_close_top_menu() {
    let mut wm = WM.lock();
    if let Some(t) = wm.topmost_idx() {
        if wm.windows[t].kind == WindowKind::Browser {
            browser_close_menu(&mut wm.windows[t]);
        }
    }
}

/// Open the context menu at a point inside the page canvas.
fn browser_open_menu(win: &mut Window, mx: i32, my: i32, page_x: usize, page_y: usize) {
    browser_close_menu(win);

    let (image, link) = match &win.app {
        AppState::Browser { session: Some(s), current, .. } => {
            let x = page_x as f32;
            let y = page_y as f32;
            let image = s
                .page
                .image_at(x, y)
                .map(|src| crate::browser::resolve_url(current, src));
            let link = s.page.link_at(x, y).map(String::from);
            (image, link)
        }
        _ => (None, None),
    };

    let mut items: Vec<(&str, WinAction)> = Vec::new();
    if image.is_some() {
        items.push(("Save picture", WinAction::BrowserSaveImage));
        items.push(("Set as wallpaper", WinAction::BrowserImageWallpaper));
        items.push(("Open picture", WinAction::BrowserOpenImage));
    } else if link.is_some() {
        items.push(("Open link", WinAction::BrowserOpenImage));
    }
    items.push(("Reload", WinAction::BrowserReload));
    items.push(("Back", WinAction::BrowserBack));
    items.push(("Top of page", WinAction::BrowserScrollTo(0)));

    *BROWSER_MENU.lock() = BrowserMenu { image, link };

    // Keep the menu inside the window: opening it near the bottom edge would
    // otherwise put half of it out of reach.
    let (ox, oy) = win.content_origin();
    let item_h = 26usize;
    let height = items.len() * item_h;
    let lx = (mx - ox).max(0) as usize;
    let ly = (my - oy).max(0) as usize;
    let lx = lx.min(win.w.saturating_sub(170));
    let ly = ly.min(win.h.saturating_sub(height + TITLEBAR_H + FOOTER_H + 8));

    for (i, (label, action)) in items.iter().enumerate() {
        win.widgets.push(Widget::button(lx, ly + i * item_h, 160, item_h - 2, label, *action));
    }
}

/// Right-click inside a browser page opens the menu. Returns true if the
/// click was consumed.
fn browser_intercept(mx: i32, my: i32, right_edge: bool) -> bool {
    if !right_edge {
        return false;
    }
    let mut wm = WM.lock();
    let Some(idx) = wm.topmost_at(mx, my) else {
        return false;
    };
    if wm.windows[idx].kind != WindowKind::Browser {
        return false;
    }
    wm.raise(idx);
    let top = wm.windows.len() - 1;
    let win = &mut wm.windows[top];
    if win.in_close_btn(mx, my) || win.in_titlebar(mx, my) {
        return false;
    }

    // Only over the page itself: a right-click on the toolbar should dismiss
    // the menu rather than pop up page commands.
    let Some((cx, cy, cw, ch)) = find_canvas_screen(win) else {
        drop(wm);
        return false;
    };
    if mx < cx as i32 || mx >= (cx + cw) as i32 || my < cy as i32 || my >= (cy + ch) as i32 {
        browser_close_menu(win);
        drop(wm);
        request_redraw();
        return true;
    }

    let scroll = match &win.app {
        AppState::Browser { scroll, .. } => *scroll,
        _ => 0,
    };
    let page_x = (mx - cx as i32).max(0) as usize;
    let page_y = (my - cy as i32).max(0) as usize + scroll;
    let page_x = page_x.saturating_sub(BROWSER_PADDING);
    let page_y = page_y.saturating_sub(BROWSER_PADDING);

    browser_open_menu(win, mx, my, page_x, page_y);
    drop(wm);
    request_redraw();
    true
}

/// Save the picture the menu was opened over. Returns where it went.
fn browser_save_menu_image() -> Result<String, String> {
    let url = BROWSER_MENU
        .lock()
        .image
        .clone()
        .ok_or_else(|| String::from("no picture selected"))?;
    browser_status(&alloc::format!("Saving {} ...", url));

    let response = crate::net::http::get(&url).map_err(|e| e)?;
    if response.status != 200 {
        return Err(alloc::format!("the server answered {}", response.status));
    }
    let extension = if response.content_type.contains("png") {
        "png"
    } else if response.content_type.contains("gif") {
        "gif"
    } else {
        "jpg"
    };
    let name = crate::browser::search::filename_for(&url, extension);
    crate::fs::save_download(DOWNLOADS, &name, &response.body).map_err(String::from)
}

fn browser_status(message: &str) {
    with_top(WindowKind::Browser, |win| {
        if let AppState::Browser { status, .. } = &mut win.app {
            *status = String::from(message);
            win.footer = Some(status.clone());
        }
    });
    render();
    framebuffer::present();
}

fn browser_save_image_action() {
    browser_close_top_menu();
    let message = match browser_save_menu_image() {
        Ok(path) => alloc::format!("Saved to {}", path),
        Err(e) => alloc::format!("Could not save the picture: {}", e),
    };
    browser_status(&message);
    request_redraw();
}

fn browser_wallpaper_action() {
    browser_close_top_menu();
    let message = match browser_save_menu_image() {
        Ok(path) => match set_wallpaper_from(&path) {
            Ok(()) => alloc::format!("Wallpaper set from {}", path),
            Err(e) => alloc::format!("Saved to {}, but it could not be shown: {}", path, e),
        },
        Err(e) => alloc::format!("Could not save the picture: {}", e),
    };
    browser_status(&message);
    request_redraw();
}

/// Open whatever the menu was raised over — the picture, or the link.
fn browser_open_menu_target() {
    browser_close_top_menu();
    let target = {
        let menu = BROWSER_MENU.lock();
        menu.image.clone().or_else(|| menu.link.clone())
    };
    if let Some(target) = target {
        browser_navigate(&target, true);
    }
}

fn browser_back() {
    let previous = with_top_ret(WindowKind::Browser, |win| {
        match &mut win.app {
            AppState::Browser { history, .. } => history.pop(),
            _ => None,
        }
    })
    .flatten();

    match previous {
        Some(url) => browser_navigate(&url, false),
        None => {
            with_top(WindowKind::Browser, |win| {
                if let AppState::Browser { status, .. } = &mut win.app {
                    *status = String::from("No page to go back to.");
                    win.footer = Some(status.clone());
                }
            });
        }
    }
}

/// Scroll the page. `dir` is negative for up, positive for down; the
/// magnitude multiplies the step.
fn browser_scroll(dir: i8) {
    with_top(WindowKind::Browser, |win| {
        let AppState::Browser { session, scroll, .. } = &mut win.app else { return };
        let step = BROWSER_SCROLL_STEP * (dir.unsigned_abs() as usize).max(1);
        let target = if dir < 0 {
            scroll.saturating_sub(step)
        } else {
            scroll.saturating_add(step)
        };
        *scroll = target.min(browser_scroll_limit(session.as_ref()));
    });
}

/// Jump to an absolute offset. `usize::MAX` lands at the bottom of the page.
fn browser_scroll_to(offset: usize) {
    with_top(WindowKind::Browser, |win| {
        let AppState::Browser { session, scroll, .. } = &mut win.app else { return };
        *scroll = offset.min(browser_scroll_limit(session.as_ref()));
    });
}

/// How far the page can scroll before its last line is at the bottom.
fn browser_scroll_limit(session: Option<&crate::browser::script::Session>) -> usize {
    let total = session.map(|s| s.page.height() as usize).unwrap_or(0);
    let viewport = BROWSER_CANVAS_H.saturating_sub(2 * BROWSER_PADDING);
    total.saturating_sub(viewport)
}

/// Handle a click inside the page area.
///
/// An editable form control takes the click ahead of everything else: it puts
/// the caret where the pointer is and never follows a link that happens to lie
/// under it. Otherwise the click is dispatched as a real DOM event, through the
/// capture and bubble phases, and what happens next is decided the way a browser
/// decides it: the link is followed, or the form submitted, unless a handler
/// called `preventDefault`.
///
/// That is a change from the previous engine, which treated any handler running
/// as consuming the click. A page with a click listener on `document` — an
/// analytics tag, a menu closer — used to have every one of its links broken by
/// it.
///
/// Coordinates are relative to the canvas widget.
fn browser_click(rel_x: usize, rel_y: usize) -> Option<String> {
    let target = with_top_ret(WindowKind::Browser, |win| {
        let (target, message) = {
            let AppState::Browser { session, scroll, current, .. } = &mut win.app else {
                return None;
            };
            let session = session.as_mut()?;
            let x = rel_x.saturating_sub(BROWSER_PADDING) as f32;
            let y = (rel_y.saturating_sub(BROWSER_PADDING) + *scroll) as f32;
            let base = current.clone();

            let field = session.page.field_at(x, y).copied();
            match field {
                Some(field) if field.kind.editable() => {
                    let column = (x - field.rect.x - crate::browser::forms::INSET)
                        / (field.size.char_w().max(1) as f32);
                    session.page.forms.focus_at(field.node, column.max(0.0) as usize);
                    return None;
                }
                Some(field) => {
                    session.page.forms.blur();
                    let outcome = session.dispatch_click_at(field.node, x, y);
                    if outcome.allows_default() {
                        let to =
                            session.page.forms.submission(&session.page.dom, field.node, &base);
                        (to, None)
                    } else {
                        (None, session.log.last().cloned())
                    }
                }
                None => {
                    // Clicking the page anywhere else puts the caret away, the
                    // way it does in every other browser.
                    session.page.forms.blur();
                    let hit = session.page.hit(x, y)?;
                    let node = hit.node;
                    let href = hit
                        .target
                        .and_then(|t| session.page.link_targets.get(t))
                        .cloned();

                    let outcome = session.dispatch_click_at(node, x, y);
                    if outcome.allows_default() {
                        (href.map(|h| crate::browser::resolve_url(&base, &h)), None)
                    } else {
                        (None, session.log.last().cloned())
                    }
                }
            }
        };

        // A handler may have shortened the page out from under the viewport.
        let limit = match &win.app {
            AppState::Browser { session, .. } => browser_scroll_limit(session.as_ref()),
            _ => 0,
        };
        if let AppState::Browser { scroll, status, .. } = &mut win.app {
            *scroll = (*scroll).min(limit);
            if let Some(message) = message {
                *status = message;
            }
            win.footer = Some(status.clone());
        }

        target
    })
    .flatten();

    // A handler may have asked to go somewhere; that wins over the link, which
    // it would have had to `preventDefault` to reach this point anyway.
    browser_follow_script_navigation();
    target
}

/// Submit the form the caret is in, which is what Enter in a field means.
///
/// The form's `submit` event is dispatched first, so a page that validates in
/// JavaScript — or that sends the form itself with `fetch` and calls
/// `preventDefault` — gets its say before the browser navigates.
///
/// Navigation blocks on the network, so the address is worked out under the
/// window manager's lock and followed once it has been dropped.
fn browser_submit_field() {
    let target = with_top_ret(WindowKind::Browser, |win| {
        let AppState::Browser { session, current, .. } = &mut win.app else { return None };
        let session = session.as_mut()?;
        let focused = session.page.forms.focused()?;
        let form = session.page.forms.get(focused).map(|control| control.form);

        if let Some(form) = form.filter(|form| *form != crate::browser::dom::NO_NODE) {
            if !session.dispatch(form, "submit", "").allows_default() {
                return None;
            }
        }
        let current = current.clone();
        session.page.forms.submission(&session.page.dom, focused, &current)
    })
    .flatten();

    browser_follow_script_navigation();
    if let Some(target) = target {
        browser_navigate(&target, true);
    }
}

/// Give a keystroke to the control the caret is in.
///
/// The page hears about it first: `keydown` is dispatched before the character
/// goes in, so a handler that calls `preventDefault` can refuse it — which is how
/// a page restricts a field to digits — and `input` follows once the value has
/// changed.
///
/// Returns nothing when the page has no focused control, so that the keys the
/// page itself uses — the arrows and the space bar, which scroll — carry on
/// working while a form is on screen but not being typed into.
fn browser_field_key(win: &mut Window, k: DecodedKey) -> Option<WinAction> {
    let AppState::Browser { session, .. } = &mut win.app else { return None };
    let session = session.as_mut()?;
    let focused = session.page.forms.focused()?;

    if !session.dispatch(focused, "keydown", &browser_key_detail(k)).allows_default() {
        return Some(WinAction::None);
    }

    // Looked up again after the dispatch: a handler may have rebuilt the form,
    // in which case there is no longer a control to type into.
    let control = session.page.forms.focused_control_mut()?;
    match k {
        DecodedKey::Unicode('\x08') => control.backspace(),
        DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => {
            // A line break belongs in a textarea and nowhere else; in a
            // one-line field Enter is what sends the form.
            if control.kind == crate::browser::forms::Kind::Area {
                control.insert('\n');
            } else {
                return Some(WinAction::BrowserSubmit);
            }
        }
        DecodedKey::Unicode(c) => control.insert(c),
        DecodedKey::RawKey(KeyCode::ArrowLeft) => control.left(),
        DecodedKey::RawKey(KeyCode::ArrowRight) => control.right(),
        DecodedKey::RawKey(KeyCode::Home) => control.home(),
        DecodedKey::RawKey(KeyCode::End) => control.end(),
        _ => return None,
    }

    // `input` fires on every edit; `change` is what fires when the field is
    // finished with, and this browser has no better moment for it than the same
    // one, since a page may never blur the field at all.
    session.dispatch(focused, "input", "");
    session.dispatch(focused, "change", "");
    Some(WinAction::None)
}

/// The `key` and `keyCode` a page's handler expects, as a JSON object.
///
/// Built rather than formatted into a source string: it is passed to the engine
/// as a value, so nothing here can become code.
fn browser_key_detail(k: DecodedKey) -> String {
    let (name, code) = match k {
        DecodedKey::Unicode('\x08') => (String::from("Backspace"), 8u32),
        DecodedKey::Unicode('\n') | DecodedKey::Unicode('\r') => (String::from("Enter"), 13),
        DecodedKey::Unicode('\t') => (String::from("Tab"), 9),
        DecodedKey::Unicode('\x1b') => (String::from("Escape"), 27),
        DecodedKey::Unicode(c) => (alloc::format!("{}", c), c as u32),
        DecodedKey::RawKey(KeyCode::ArrowLeft) => (String::from("ArrowLeft"), 37),
        DecodedKey::RawKey(KeyCode::ArrowRight) => (String::from("ArrowRight"), 39),
        DecodedKey::RawKey(KeyCode::ArrowUp) => (String::from("ArrowUp"), 38),
        DecodedKey::RawKey(KeyCode::ArrowDown) => (String::from("ArrowDown"), 40),
        DecodedKey::RawKey(KeyCode::Home) => (String::from("Home"), 36),
        DecodedKey::RawKey(KeyCode::End) => (String::from("End"), 35),
        DecodedKey::RawKey(other) => (alloc::format!("{:?}", other), 0),
    };
    // Only a quote and a backslash can break a JSON string, and a key name is
    // one character or an identifier — but the character *could* be a quote.
    let escaped: String = name
        .chars()
        .flat_map(|c| match c {
            '"' => alloc::vec!['\\', '"'],
            '\\' => alloc::vec!['\\', '\\'],
            other => alloc::vec![other],
        })
        .collect();
    alloc::format!("{{\"key\":\"{}\",\"keyCode\":{}}}", escaped, code)
}

/// Take the caret out of any field on the page.
///
/// Used when something in the window chrome takes focus: the address bar and a
/// field on the page cannot both be what the keyboard is talking to.
fn browser_blur_field(win: &mut Window) {
    if let AppState::Browser { session: Some(session), .. } = &mut win.app {
        session.page.forms.blur();
    }
}

/// Draw one form control: its box, and for a field the contents and the caret.
///
/// The contents are not in the display list. A keystroke has to appear without
/// the page being laid out again, which is the same reason a picture's pixels
/// are looked up here rather than carried in the command. A button's label, by
/// contrast, comes from the document and arrives as an ordinary text command
/// drawn over this box.
fn browser_draw_field(
    field: &crate::browser::FieldBox,
    forms: &crate::browser::Forms,
    x: usize,
    top: isize,
    w: usize,
    clip: (isize, isize),
) {
    use crate::browser::forms::Kind;

    let (clip_top, clip_bottom) = clip;
    let h = field.rect.height.max(1.0) as usize;
    let visible_top = top.max(clip_top);
    let visible_bottom = (top + h as isize).min(clip_bottom);
    if visible_bottom <= visible_top {
        return;
    }
    let span = (visible_bottom - visible_top) as usize;

    let focused = forms.focused() == Some(field.node);
    let fill = match field.kind {
        Kind::Submit | Kind::Push => BROWSER_BUTTON_BG,
        // A control with no interface is drawn flat, so that it reads as
        // something this browser will not let you touch rather than as an empty
        // field somebody forgot to fill in.
        Kind::Unsupported => BROWSER_FIELD_FLAT,
        _ => BROWSER_FIELD_BG,
    };
    let edge = if focused { BROWSER_FIELD_FOCUS } else { BROWSER_FIELD_EDGE };

    framebuffer::fill_rect(x, visible_top as usize, w, span, fill);
    // The frame is drawn edge by edge rather than stroked, so that a control
    // straddling the top of the view keeps its sides and loses only the edge
    // that is actually off screen.
    framebuffer::fill_rect(x, visible_top as usize, 1, span, edge);
    framebuffer::fill_rect(x + w - 1, visible_top as usize, 1, span, edge);
    for y in [top, top + h as isize - 1] {
        if y >= clip_top && y < clip_bottom {
            framebuffer::fill_rect(x, y as usize, w, 1, edge);
        }
    }

    if !field.kind.editable() {
        return;
    }
    let Some(control) = forms.get(field.node) else { return };

    let inset = crate::browser::forms::INSET as usize;
    let char_w = field.size.char_w().max(1);
    let row_h = field.size.row_h().max(1);
    let inner_w = w.saturating_sub(2 * inset);
    let view = crate::browser::forms::view(
        &control.value,
        control.caret,
        inner_w / char_w,
        h.saturating_sub(2 * inset) / row_h,
        field.kind.masked(),
    );

    let rows = (clip_top.max(0) as usize, clip_bottom.max(0) as usize);
    for (i, line) in view.rows.iter().enumerate() {
        framebuffer::draw_text_styled(
            x + inset,
            top + (inset + i * row_h) as isize,
            line,
            BROWSER_FIELD_TEXT,
            fill,
            inner_w,
            field.size,
            false,
            Some(rows),
        );
    }

    if focused {
        let caret_x = x + inset + view.caret_col * char_w;
        let caret_y = top + (inset + view.caret_row * row_h) as isize;
        let from = caret_y.max(clip_top);
        let to = (caret_y + row_h as isize).min(clip_bottom);
        if to > from && caret_x < x + w {
            framebuffer::fill_rect(caret_x, from as usize, 1, (to - from) as usize, BROWSER_FIELD_TEXT);
        }
    }
}

fn render_browser(win: &Window) {
    let AppState::Browser { session, scroll, .. } = &win.app else { return };
    let Some((cx, cy, cw, ch)) = find_canvas_screen(win) else { return };

    let Some(page) = session.as_ref().map(|s| &s.page) else {
        framebuffer::fill_rect(cx, cy, cw, ch, BROWSER_PAGE_BG);
        draw_text(cx + 4, cy + 4,
                  "Type words to search, or an address to go there.",
                  theme::TEXT_MUTED, BROWSER_PAGE_BG, cw.saturating_sub(8));
        draw_text(cx + 4, cy + 4 + ROW_H,
                  "Run `net up` in the shell first if nothing loads.",
                  theme::TEXT_MUTED, BROWSER_PAGE_BG, cw.saturating_sub(8));
        return;
    };

    let background = page.background.unwrap_or(BROWSER_PAGE_BG);
    framebuffer::fill_rect(cx, cy, cw, ch, background);

    // Page coordinates map to the screen by adding this origin and
    // subtracting the scroll offset.
    let origin_x = cx + BROWSER_PADDING;
    let origin_y = (cy + BROWSER_PADDING) as isize - *scroll as isize;
    let clip_top = cy as isize;
    let clip_bottom = (cy + ch) as isize;
    let clip_right = cx + cw - BROWSER_GUTTER;

    for command in &page.display.commands {
        match command {
            crate::browser::DisplayCommand::SolidRect { rect, color } => {
                let top = origin_y + rect.y as isize;
                let bottom = top + rect.height.max(1.0) as isize;
                if bottom <= clip_top || top >= clip_bottom {
                    continue;
                }
                // Clip to the canvas rather than letting a tall box paint
                // over the window chrome.
                let visible_top = top.max(clip_top);
                let visible_bottom = bottom.min(clip_bottom);
                let x = origin_x + rect.x.max(0.0) as usize;
                if x >= clip_right {
                    continue;
                }
                let w = (rect.width.max(1.0) as usize).min(clip_right - x);
                framebuffer::fill_rect(
                    x,
                    visible_top as usize,
                    w,
                    (visible_bottom - visible_top) as usize,
                    *color,
                );
            }
            crate::browser::DisplayCommand::Text {
                x, y, width, height, text, color, size, bold, underline, strike,
            } => {
                let top = origin_y + *y as isize;
                let bottom = top + *height as isize;
                if bottom <= clip_top || top >= clip_bottom {
                    continue;
                }
                let sx = origin_x + *x as usize;
                if sx >= clip_right {
                    continue;
                }
                let max_w = clip_right - sx;
                // Rows straddling an edge are drawn partially rather than
                // dropped: page content is not aligned to the scroll step, so
                // dropping them would leave a gap at the top of the view.
                let clip = (clip_top.max(0) as usize, clip_bottom.max(0) as usize);
                framebuffer::draw_text_styled(
                    sx, top, text, *color, background, max_w, *size, *bold, Some(clip),
                );
                let rule = |offset: isize, color| {
                    let ry = top + offset;
                    if ry >= clip_top && ry < clip_bottom {
                        let w = (*width as usize).min(max_w);
                        framebuffer::fill_rect(sx, ry as usize, w, 1, color);
                    }
                };
                if *underline {
                    rule(*height as isize - 2, *color);
                }
                if *strike {
                    rule(*height as isize / 2, *color);
                }
            }
            crate::browser::DisplayCommand::Field(field) => {
                let top = origin_y + field.rect.y as isize;
                let bottom = top + field.rect.height.max(1.0) as isize;
                if bottom <= clip_top || top >= clip_bottom {
                    continue;
                }
                let x = origin_x + field.rect.x.max(0.0) as usize;
                if x >= clip_right {
                    continue;
                }
                let w = (field.rect.width.max(1.0) as usize).min(clip_right - x);
                browser_draw_field(field, &page.forms, x, top, w, (clip_top, clip_bottom));
            }
            crate::browser::DisplayCommand::Image { rect, src } => {
                let top = origin_y + rect.y as isize;
                let bottom = top + rect.height.max(1.0) as isize;
                if bottom <= clip_top || top >= clip_bottom {
                    continue;
                }
                let x = origin_x + rect.x.max(0.0) as usize;
                if x >= clip_right {
                    continue;
                }
                let Some(image) = page.images.get(src) else {
                    continue;
                };
                framebuffer::blit_scaled(
                    x,
                    top,
                    rect.width.max(1.0) as usize,
                    rect.height.max(1.0) as usize,
                    &image.pixels,
                    image.width,
                    image.height,
                    clip_right,
                    Some((clip_top.max(0) as usize, clip_bottom.max(0) as usize)),
                );
            }
        }
    }

    // Scroll position indicator down the right edge.
    let total = (page.height() as usize).max(1);
    let viewport = ch.saturating_sub(2 * BROWSER_PADDING);
    if total > viewport {
        let track_x = cx + cw - 4;
        framebuffer::fill_rect(track_x, cy, 3, ch, theme::FIELD_BG);
        let thumb_h = (ch * viewport / total).max(12);
        let thumb_y = cy + (ch.saturating_sub(thumb_h)) * *scroll
            / total.saturating_sub(viewport).max(1);
        framebuffer::fill_rect(track_x, thumb_y.min(cy + ch - thumb_h), 3, thumb_h, theme::ACCENT);
    }
}

fn paint_clear() {
    with_top(WindowKind::Paint, |win| {
        if let AppState::Paint { canvas, drawing, last, .. } = &mut win.app {
            for px in canvas.iter_mut() {
                *px = CANVAS_BG;
            }
            *drawing = false;
            *last = None;
        }
    });
}

fn paint_set_color(idx: u8) {
    with_top(WindowKind::Paint, |win| {
        let Some(&chosen) = PAINT_PALETTE.get(idx as usize) else { return };
        if let AppState::Paint { color, tool, .. } = &mut win.app {
            *color = chosen;
            // Picking a colour implies you want to paint with it.
            if *tool == PaintTool::Eraser {
                *tool = PaintTool::Brush;
            }
        }
        for wgt in win.widgets.iter_mut() {
            if let Widget::Swatch { selected, action, .. } = wgt {
                *selected = matches!(action, WinAction::PaintColor(i) if *i == idx);
            }
        }
    });
}

fn paint_set_brush(size: u8) {
    with_top(WindowKind::Paint, |win| {
        if let AppState::Paint { brush, .. } = &mut win.app {
            *brush = size as usize;
        }
    });
}

fn paint_set_tool(tool: PaintTool) {
    with_top(WindowKind::Paint, |win| {
        if let AppState::Paint { tool: t, .. } = &mut win.app {
            *t = tool;
        }
    });
}

/// Stamp a filled circle of the current brush into the canvas bitmap.
fn paint_stamp(canvas: &mut [Color], cw: usize, ch: usize,
               x: i32, y: i32, radius: usize, colour: Color) {
    let r = radius as i32;
    let r2 = r * r;
    for dy in -r..=r {
        let py = y + dy;
        if py < 0 || py >= ch as i32 {
            continue;
        }
        for dx in -r..=r {
            let px = x + dx;
            if px < 0 || px >= cw as i32 {
                continue;
            }
            if dx * dx + dy * dy <= r2 {
                canvas[py as usize * cw + px as usize] = colour;
            }
        }
    }
}

/// Stamp along the segment between two samples. The mouse only reports a
/// position every so often, so without this a fast drag leaves a dotted line.
fn paint_stroke(canvas: &mut [Color], cw: usize, ch: usize,
                from: (u16, u16), to: (u16, u16), radius: usize, colour: Color) {
    let (x0, y0) = (from.0 as i32, from.1 as i32);
    let (x1, y1) = (to.0 as i32, to.1 as i32);
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).max(1);
    for s in 0..=steps {
        paint_stamp(canvas, cw, ch, x0 + dx * s / steps, y0 + dy * s / steps, radius, colour);
    }
}

/// Flood fill the contiguous same-coloured region containing `(x, y)`.
///
/// Scanline fill with an explicit stack rather than recursion — the kernel
/// stack is small and a large region would blow it.
fn paint_fill(canvas: &mut [Color], cw: usize, ch: usize, x: usize, y: usize, colour: Color) {
    if x >= cw || y >= ch {
        return;
    }
    let target = canvas[y * cw + x];
    if target == colour {
        return;
    }

    let mut stack = alloc::vec![(x, y)];
    while let Some((sx, sy)) = stack.pop() {
        // Walk left to the start of this run.
        let mut left = sx;
        while left > 0 && canvas[sy * cw + left - 1] == target {
            left -= 1;
        }
        let mut right = sx;
        while right + 1 < cw && canvas[sy * cw + right + 1] == target {
            right += 1;
        }

        for px in left..=right {
            canvas[sy * cw + px] = colour;
        }

        // Seed the rows above and below, one seed per contiguous run so the
        // stack stays proportional to the region's perimeter.
        for (ny, valid) in [(sy.wrapping_sub(1), sy > 0), (sy + 1, sy + 1 < ch)] {
            if !valid {
                continue;
            }
            let mut px = left;
            while px <= right {
                if canvas[ny * cw + px] == target {
                    stack.push((px, ny));
                    while px <= right && canvas[ny * cw + px] == target {
                        px += 1;
                    }
                }
                px += 1;
            }
        }
    }
}

fn snake_restart() {
    with_top(WindowKind::Snake, |win| {
        {
            let AppState::Snake {
                snake, dir, pending_dir, food, game_over, score,
                rng, last_step_ticks, ..
            } = &mut win.app else { return; };
            *snake = vec![(7, 5), (6, 5), (5, 5)];
            *dir = (1, 0);
            *pending_dir = (1, 0);
            *food = (10, 5);
            *game_over = false;
            *score = 0;
            *rng = 0xDEAD_BEEF;
            *last_step_ticks = crate::clock::ticks();
        }
        set_label_text(win, 2, "Score: 0");
    });
}

fn breakout_restart() {
    with_top(WindowKind::Breakout, |win| {
        if let AppState::Breakout(state) = &mut win.app {
            state.restart();
            state.last_step_ticks = crate::clock::ticks();
            let line = state.status_line();
            set_label_text(win, 2, &line);
        }
    });
}

fn racing_restart() {
    with_top(WindowKind::Racing, |win| {
        if let AppState::Racing(state) = &mut win.app {
            state.restart();
            state.last_step_ticks = crate::clock::ticks();
            let line = state.status_line();
            set_label_text(win, 2, &line);
        }
    });
}

fn invaders_restart() {
    with_top(WindowKind::Invaders, |win| {
        if let AppState::Invaders(state) = &mut win.app {
            state.restart();
            state.last_step_ticks = crate::clock::ticks();
            let line = state.status_line();
            set_label_text(win, 2, &line);
        }
    });
}

fn abc_refresh_buttons(win: &mut Window) {
    let AppState::Abc(state) = &win.app else { return; };
    let labels = [
        state.choice_label(0),
        state.choice_label(1),
        state.choice_label(2),
    ];
    let status = state.status_line();
    set_label_text(win, 2, &status);
    for (i, label) in labels.iter().enumerate() {
        if let Some(Widget::Button { label: text, .. }) = win.widgets.get_mut(3 + i) {
            *text = label.clone();
        }
    }
}

fn toggle_cursor_blink() {
    let v = !CURSOR_BLINK.load(Ordering::Relaxed);
    CURSOR_BLINK.store(v, Ordering::Relaxed);
    with_top(WindowKind::Settings, |win| {
        if let Some(Widget::Checkbox { checked, .. }) = win.widgets.get_mut(1) {
            *checked = v;
        }
    });
}

fn toggle_dark_theme() {
    let v = !DARK_THEME.load(Ordering::Relaxed);
    DARK_THEME.store(v, Ordering::Relaxed);
    crate::wallpaper::invalidate();
    with_top(WindowKind::Settings, |win| {
        if let Some(Widget::Checkbox { checked, .. }) = win.widgets.get_mut(2) {
            *checked = v;
        }
    });
}

pub fn launch_builtin(kind: WindowKind) {
    let mut wm = WM.lock();
    match kind {
        WindowKind::Calculator =>
            raise_or_spawn(&mut wm, kind, create_calculator_window),
        WindowKind::FileManager => {
            raise_or_spawn(&mut wm, kind, create_file_manager_window);
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
                refresh_file_manager(&mut wm.windows[idx]);
            }
        }
        WindowKind::Terminal =>
            raise_or_spawn(&mut wm, kind, create_terminal_window),
        WindowKind::Monitor =>
            raise_or_spawn(&mut wm, kind, create_monitor_window),
        WindowKind::Notepad =>
            raise_or_spawn(&mut wm, kind, create_notepad_window),
        WindowKind::CodeEditor =>
            raise_or_spawn(&mut wm, kind, create_code_editor_window),
        WindowKind::Paint =>
            raise_or_spawn(&mut wm, kind, create_paint_window),
        WindowKind::Browser => {
            drop(wm);
            open_browser();
            return;
        }
        WindowKind::Snake => {
            raise_or_spawn(&mut wm, kind, create_snake_window);
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::Snake) {
                if let AppState::Snake { last_step_ticks, .. } = &mut wm.windows[idx].app {
                    *last_step_ticks = crate::clock::ticks();
                }
            }
        }
        WindowKind::Breakout => {
            raise_or_spawn(&mut wm, kind, create_breakout_window);
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::Breakout) {
                if let AppState::Breakout(state) = &mut wm.windows[idx].app {
                    state.last_step_ticks = crate::clock::ticks();
                }
            }
        }
        WindowKind::Abc => {
            raise_or_spawn(&mut wm, kind, create_abc_window);
        }
        WindowKind::Racing => {
            raise_or_spawn(&mut wm, kind, create_racing_window);
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::Racing) {
                if let AppState::Racing(state) = &mut wm.windows[idx].app {
                    state.last_step_ticks = crate::clock::ticks();
                }
            }
        }
        WindowKind::Invaders => {
            raise_or_spawn(&mut wm, kind, create_invaders_window);
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::Invaders) {
                if let AppState::Invaders(state) = &mut wm.windows[idx].app {
                    state.last_step_ticks = crate::clock::ticks();
                }
            }
        }
        WindowKind::ImageView =>
            raise_or_spawn(&mut wm, kind, create_image_view_window),
        WindowKind::Settings =>
            raise_or_spawn(&mut wm, kind, create_settings_window),
        WindowKind::About =>
            raise_or_spawn(&mut wm, kind, create_about_window),
        WindowKind::Installer =>
            raise_or_spawn(&mut wm, kind, create_installer_window),
        WindowKind::Launcher =>
            raise_or_spawn(&mut wm, kind, create_launcher_window),
        WindowKind::FileDialog | WindowKind::Generic | WindowKind::MyComputer => {}
    }
    drop(wm);
    request_redraw();
}

/// Spawn an ELF application. Takes a plain slice so installed packages
/// (whose bytes live on the heap) can be launched exactly like bundled ones.
pub fn launch_elf_app(name: &str, bytes: &[u8]) -> Result<(), &'static str> {
    crate::println!("[launcher] Spawning ELF app: {}", name);
    match crate::process::spawn_elf_bytes(bytes) {
        Ok(pid) => {
            crate::println!("[launcher] spawned pid={}", pid);
            // Don't block! The main loop will call run_scheduler_once.
            Ok(())
        }
        Err(e) => {
            crate::println!("[launcher] spawn failed: {}", e);
            Err(e)
        }
    }
}

pub fn apply_action(action: WinAction) {
    match action {
        WinAction::None => {}
        WinAction::Close => {
            let mut wm = WM.lock();
            let closed = wm.windows.pop();
            wm.dragging = None;
            drop(wm);
            if let Some(pid) = closed.and_then(|w| w.owner_pid) {
                crate::process::terminate(pid);
            }
            request_redraw();
        }
        WinAction::ClearTextbox => {
            let mut wm = WM.lock();
            if let Some(top) = wm.topmost_idx() {
                for widget in wm.windows[top].widgets.iter_mut() {
                    if let Widget::TextBox { text, .. } = widget {
                        text.clear();
                    }
                }
            }
            drop(wm);
            request_redraw();
        }
        WinAction::RefreshFiles => {
            let mut wm = WM.lock();
            if let Some(top) = wm.topmost_idx() {
                let win = &mut wm.windows[top];
                if win.kind == WindowKind::FileManager {
                    fm_close_menu(win);
                    refresh_file_manager(win);
                }
            }
            drop(wm);
            request_redraw();
        }
        WinAction::OpenReadme => {
            // Legacy: show readme/welcome in preview if no selection; prefer FmPreview.
            file_manager_preview_readme();
        }
        WinAction::FmUp => {
            file_manager_go_up();
            request_redraw();
        }
        WinAction::FmNewFolder => {
            file_manager_new_folder();
            request_redraw();
        }
        WinAction::FmNewFile => {
            file_manager_new_file();
            request_redraw();
        }
        WinAction::FmDelete => {
            file_manager_delete();
            request_redraw();
        }
        WinAction::FmOpen => {
            fm_close_top_file_menu();
            file_manager_open_selected();
            request_redraw();
        }
        WinAction::FmSetWallpaper => {
            file_manager_set_wallpaper();
            request_redraw();
        }
        WinAction::ResetWallpaper => {
            crate::wallpaper::clear_picture();
            crate::fs::forget_wallpaper();
            refresh_settings_wallpaper_line();
            request_redraw();
        }
        WinAction::FmPreview => {
            fm_close_top_file_menu();
            file_manager_preview_selection_or_readme();
            request_redraw();
        }
        WinAction::FmViewList => {
            fm_close_top_file_menu();
            let mut wm = WM.lock();
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
                if let AppState::FileManager { view_mode, .. } = &mut wm.windows[idx].app {
                    *view_mode = FmViewMode::List;
                }
                refresh_file_manager(&mut wm.windows[idx]);
            }
            drop(wm);
            request_redraw();
        }
        WinAction::FmViewIcons => {
            fm_close_top_file_menu();
            let mut wm = WM.lock();
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
                if let AppState::FileManager { view_mode, .. } = &mut wm.windows[idx].app {
                    *view_mode = FmViewMode::LargeIcons;
                }
                refresh_file_manager(&mut wm.windows[idx]);
            }
            drop(wm);
            request_redraw();
        }
        WinAction::FileDialogSave => file_dialog_confirm_save(),
        WinAction::FileDialogOpen => file_dialog_confirm_open(),
        WinAction::FileDialogCancel => file_dialog_close(),
        WinAction::FileDialogUp => file_dialog_up(),
        WinAction::TerminalRun => {
            let mut wm = WM.lock();
            if let Some(top) = wm.topmost_idx() {
                let win = &mut wm.windows[top];
                if win.kind == WindowKind::Terminal {
                    let mut cmd = String::new();
                    if let Some(Widget::TextBox { text, .. }) = win.widgets.get(1) {
                        cmd.push_str(text);
                    }
                    let output = run_terminal_command(&cmd);
                    set_label_text(win, 3, &output);
                }
            }
            drop(wm);
            request_redraw();
        }
        WinAction::NotepadSave => notepad_save(),
        WinAction::NotepadLoad => notepad_load(),
        WinAction::NotepadClear => notepad_clear(),
        WinAction::CodeNew => code_editor_new(),
        WinAction::CodeOpen => code_editor_load(),
        WinAction::CodeSave => code_editor_save(),
        WinAction::CodeBuild => { code_editor_build(); }
        WinAction::CodeRun => code_editor_run(),
        WinAction::BrowserGo => browser_go(),
        WinAction::BrowserBack => {
            browser_close_top_menu();
            browser_back();
        }
        WinAction::BrowserHome => browser_navigate(BROWSER_HOME, true),
        WinAction::BrowserImages => browser_images(),
        WinAction::BrowserReload => {
            browser_close_top_menu();
            browser_reload();
        }
        WinAction::BrowserScroll(dir) => browser_scroll(dir),
        WinAction::BrowserScrollTo(offset) => {
            browser_close_top_menu();
            browser_scroll_to(offset);
        }
        WinAction::BrowserOpenImage => browser_open_menu_target(),
        WinAction::BrowserSaveImage => browser_save_image_action(),
        WinAction::BrowserImageWallpaper => browser_wallpaper_action(),
        WinAction::BrowserSubmit => browser_submit_field(),
        WinAction::PaintClear => paint_clear(),
        WinAction::PaintColor(i) => paint_set_color(i),
        WinAction::PaintBrush(n) => paint_set_brush(n),
        WinAction::PaintTool(t) => paint_set_tool(t),
        WinAction::SnakeRestart => snake_restart(),
        WinAction::SnakeDir(dx, dy) => {
            with_top(WindowKind::Snake, |win| {
                if let AppState::Snake { pending_dir, dir, game_over, .. } = &mut win.app {
                    if *game_over {
                        return;
                    }
                    if !(dx + dir.0 == 0 && dy + dir.1 == 0) {
                        *pending_dir = (dx, dy);
                    }
                }
            });
        }
        WinAction::BreakoutRestart => breakout_restart(),
        WinAction::BreakoutLeft => {
            with_top(WindowKind::Breakout, |win| {
                if let AppState::Breakout(state) = &mut win.app {
                    state.nudge_paddle(-1);
                }
            });
        }
        WinAction::BreakoutRight => {
            with_top(WindowKind::Breakout, |win| {
                if let AppState::Breakout(state) = &mut win.app {
                    state.nudge_paddle(1);
                }
            });
        }
        WinAction::BreakoutStop => {}
        WinAction::BreakoutFaster => {
            with_top(WindowKind::Breakout, |win| {
                if let AppState::Breakout(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::BreakoutSlower => {
            with_top(WindowKind::Breakout, |win| {
                if let AppState::Breakout(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::RacingRestart => racing_restart(),
        WinAction::RacingLeft => {
            with_top(WindowKind::Racing, |win| {
                if let AppState::Racing(state) = &mut win.app {
                    state.nudge(-1);
                }
            });
        }
        WinAction::RacingRight => {
            with_top(WindowKind::Racing, |win| {
                if let AppState::Racing(state) = &mut win.app {
                    state.nudge(1);
                }
            });
        }
        WinAction::RacingFaster => {
            with_top(WindowKind::Racing, |win| {
                if let AppState::Racing(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::RacingSlower => {
            with_top(WindowKind::Racing, |win| {
                if let AppState::Racing(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::InvadersRestart => invaders_restart(),
        WinAction::InvadersLeft => {
            with_top(WindowKind::Invaders, |win| {
                if let AppState::Invaders(state) = &mut win.app {
                    state.nudge(-1);
                }
            });
        }
        WinAction::InvadersRight => {
            with_top(WindowKind::Invaders, |win| {
                if let AppState::Invaders(state) = &mut win.app {
                    state.nudge(1);
                }
            });
        }
        WinAction::InvadersFire => {
            with_top(WindowKind::Invaders, |win| {
                if let AppState::Invaders(state) = &mut win.app {
                    state.fire();
                }
            });
        }
        WinAction::InvadersFaster => {
            with_top(WindowKind::Invaders, |win| {
                if let AppState::Invaders(state) = &mut win.app {
                    state.faster();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::InvadersSlower => {
            with_top(WindowKind::Invaders, |win| {
                if let AppState::Invaders(state) = &mut win.app {
                    state.slower();
                    let line = state.status_line();
                    set_label_text(win, 2, &line);
                }
            });
        }
        WinAction::AbcPick(i) => {
            with_top(WindowKind::Abc, |win| {
                if let AppState::Abc(state) = &mut win.app {
                    state.pick(i as usize);
                }
                abc_refresh_buttons(win);
            });
        }
        WinAction::AbcNext => {
            with_top(WindowKind::Abc, |win| {
                if let AppState::Abc(state) = &mut win.app {
                    state.next_round();
                }
                abc_refresh_buttons(win);
            });
        }
        WinAction::CalcDigit(d) => {
            with_top(WindowKind::Calculator, |win| {
                let AppState::Calculator { entry, new_entry, .. } = &mut win.app else { return; };
                if d == 255 {
                    if !*new_entry && !entry.is_empty() {
                        entry.pop();
                        if entry.is_empty() || entry == "-" {
                            entry.clear();
                            entry.push('0');
                            *new_entry = true;
                        }
                    }
                    calc_refresh(win);
                    return;
                }
                let c = (b'0' + d.min(9)) as char;
                if *new_entry {
                    entry.clear();
                    entry.push(c);
                    *new_entry = false;
                } else {
                    if entry == "0" {
                        entry.clear();
                    }
                    if entry.chars().count() < 20 {
                        entry.push(c);
                    }
                }
                calc_refresh(win);
            });
        }
        WinAction::CalcSign => {
            with_top(WindowKind::Calculator, |win| {
                let AppState::Calculator { entry, .. } = &mut win.app else { return; };
                if entry == "0" {
                    return;
                }
                if entry.starts_with('-') {
                    entry.remove(0);
                } else {
                    entry.insert(0, '-');
                }
                calc_refresh(win);
            });
        }
        WinAction::CalcOp(op) => {
            with_top(WindowKind::Calculator, |win| {
                let AppState::Calculator { lhs, op: pending, entry, new_entry } = &mut win.app else { return; };
                let rhs = entry.parse::<i64>().unwrap_or(0);
                if let (Some(a), Some(p)) = (*lhs, *pending) {
                    *lhs = calc_apply_i64(a, rhs, p);
                } else {
                    *lhs = Some(rhs);
                }
                if let Some(v) = *lhs {
                    entry.clear();
                    entry.push_str(&v.to_string());
                } else {
                    entry.clear();
                    entry.push_str("Error");
                    *lhs = None;
                    *pending = None;
                }
                *pending = Some(op);
                *new_entry = true;
                calc_refresh(win);
            });
        }
        WinAction::CalcEq => {
            with_top(WindowKind::Calculator, |win| {
                let AppState::Calculator { lhs, op, entry, new_entry } = &mut win.app else { return; };
                if let (Some(a), Some(p)) = (*lhs, *op) {
                    let rhs = entry.parse::<i64>().unwrap_or(0);
                    if let Some(v) = calc_apply_i64(a, rhs, p) {
                        entry.clear();
                        entry.push_str(&v.to_string());
                    } else {
                        entry.clear();
                        entry.push_str("Error");
                    }
                    *lhs = None;
                    *op = None;
                    *new_entry = true;
                    calc_refresh(win);
                }
            });
        }
        WinAction::CalcClear => {
            with_top(WindowKind::Calculator, |win| {
                let AppState::Calculator { lhs, op, entry, new_entry } = &mut win.app else { return; };
                *lhs = None;
                *op = None;
                entry.clear();
                entry.push('0');
                *new_entry = true;
                calc_refresh(win);
            });
        }
        WinAction::ToggleCursorBlink => toggle_cursor_blink(),
        WinAction::ToggleDarkTheme => toggle_dark_theme(),
        WinAction::ExitGui => exit_gui_mode(),
        WinAction::Shutdown => {
            crate::exit_qemu(crate::QemuExitCode::Success);
        }
        WinAction::InstallerNext => {
            with_top(WindowKind::Installer, |win| {
                let AppState::Installer {
                    step,
                    selected,
                    targets,
                    status,
                    ..
                } = &mut win.app
                else {
                    return;
                };
                match *step {
                    0 => {
                        *step = 1;
                        status.clear();
                        status.push_str("Select a target disk, then click Next.");
                    }
                    1 => {
                        if selected.is_none() || targets.is_empty() {
                            status.clear();
                            status.push_str("Pick a target disk first.");
                        } else {
                            *step = 2;
                            status.clear();
                            status.push_str("Type ERASE to confirm the wipe.");
                        }
                    }
                    _ => {}
                }
                rebuild_installer_widgets(win);
            });
        }
        WinAction::InstallerBack => {
            with_top(WindowKind::Installer, |win| {
                let AppState::Installer { step, status, .. } = &mut win.app else {
                    return;
                };
                if *step > 0 && *step < 3 {
                    *step -= 1;
                    status.clear();
                    status.push_str("Choose carefully — install erases the target disk.");
                    rebuild_installer_widgets(win);
                }
            });
        }
        WinAction::InstallerPick(i) => {
            with_top(WindowKind::Installer, |win| {
                let AppState::Installer {
                    targets,
                    selected,
                    status,
                    ..
                } = &mut win.app
                else {
                    return;
                };
                let idx = i as usize;
                if idx < targets.len() {
                    *selected = Some(idx);
                    status.clear();
                    status.push_str(&alloc::format!("Selected: {}", targets[idx].label()));
                    rebuild_installer_widgets(win);
                }
            });
        }
        WinAction::InstallerStart => {
            // Read confirmation text, then run the copy with the WM lock released
            // so progress redraws can take it.
            let (source, target, confirm_ok) = {
                let mut wm = WM.lock();
                let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::Installer) else {
                    return;
                };
                let win = &mut wm.windows[idx];
                let confirm = win
                    .widgets
                    .iter()
                    .find_map(|w| match w {
                        Widget::TextBox { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                if confirm.trim() != "ERASE" {
                    installer_set_status(win, "Type ERASE (all caps) in the box first.");
                    drop(wm);
                    request_redraw();
                    return;
                }
                let AppState::Installer {
                    source,
                    targets,
                    selected,
                    ..
                } = &win.app
                else {
                    return;
                };
                let Some(ti) = *selected else {
                    installer_set_status(win, "No target selected.");
                    drop(wm);
                    request_redraw();
                    return;
                };
                let Some(target) = targets.get(ti).copied() else {
                    return;
                };
                let source = *source;
                installer_set_status(win, "Installing… do not power off.");
                rebuild_installer_widgets(win);
                // Force step display to stay on confirm until done; status updated.
                (source, target, true)
            };
            let _ = confirm_ok;
            request_redraw();
            crate::framebuffer::present();

            let result = crate::install::install(source, target, Some(installer_progress));
            {
                let mut wm = WM.lock();
                if let Some(win) = wm.windows.iter_mut().find(|w| w.kind == WindowKind::Installer) {
                    if let AppState::Installer { step, status, .. } = &mut win.app {
                        *step = 3;
                        status.clear();
                        match result {
                            Ok(()) => status.push_str(
                                "Install finished. Remove the USB/ISO and reboot from the target disk.",
                            ),
                            Err(e) => status.push_str(&alloc::format!("Install failed: {}", e)),
                        }
                    }
                    rebuild_installer_widgets(win);
                }
            }
            request_redraw();
        }
        WinAction::InstallerReboot => {
            // Same path as the shell `reboot` command: pulse the keyboard
            // controller reset line.
            unsafe {
                core::arch::asm!(
                    "mov al, 0xFE",
                    "out 0x64, al",
                    options(nostack, nomem, preserves_flags),
                );
            }
            crate::exit_qemu(crate::QemuExitCode::Success);
        }
        WinAction::OpenMyComputer => {
            let mut wm = WM.lock();
            raise_or_spawn(&mut wm, WindowKind::MyComputer, create_my_computer_window);
            drop(wm);
            request_redraw();
        }
        WinAction::OpenInstaller => {
            launch_builtin(WindowKind::Installer);
        }
        WinAction::OpenApplications => {
            let mut wm = WM.lock();
            raise_or_spawn(&mut wm, WindowKind::Launcher, create_launcher_window);
            drop(wm);
            request_redraw();
        }
        WinAction::OpenBrowser => open_browser(),
        WinAction::OpenDrive(n) => {
            // Set the File Manager's current path, then open / refocus it
            // and populate the listing.
            let path = match n {
                0 => "/fat",
                1 => "/init",
                2 => "/data",
                3 => crate::fs::DISK_ROOT,
                4 => crate::fs::USB_ROOT,
                _ => "/fat",
            };
            *FM_PATH.lock() = String::from(path);
            let mut wm = WM.lock();
            // Force a fresh window so its title reflects the chosen path.
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
                wm.windows.remove(idx);
            }
            wm.spawn(create_file_manager_window());
            if let Some(idx) = wm.windows.iter().position(|w| w.kind == WindowKind::FileManager) {
                refresh_file_manager(&mut wm.windows[idx]);
            }
            drop(wm);
            request_redraw();
        }
        WinAction::LaunchApp(idx) => {
            if let Err(e) = crate::apps::launch(idx as usize) {
                crate::println!("[launcher] {}", e);
            }
        }
        WinAction::User(action_id) => {
            let wid = {
                let wm = WM.lock();
                let top = wm.topmost_idx();
                if let Some(t) = top {
                    let id = wm.windows[t].id;
                    // Bound the backlog: an app that never calls
                    // `sys_get_event` would otherwise grow this queue until
                    // the allocator aborts the kernel. Dropping the oldest
                    // event is the same policy as a full input ring.
                    let q = &wm.windows[t].user_events;
                    while q.len() >= MAX_PENDING_EVENTS {
                        let _ = q.pop();
                    }
                    q.push(GuiEvent::ButtonClicked { action_id });
                    id
                } else {
                    0u64
                }
            };
            if wid != 0 {
                crate::process::wake_on_window(wid);
            }
        }
    }
}

// ── Userspace GUI syscall surface ──────────────────────────────────────────
//
// Everything below is reachable from a ring-3 ELF, so each entry point has to
// assume hostile input. Two rules apply throughout:
//
//   1. Ownership — a process may only touch windows it created. Window ids
//      are sequential and therefore trivially guessable, so without this a
//      process could enumerate ids and drive (or read events from) any other
//      app's UI, built-ins included.
//   2. Quotas — every allocation a syscall can trigger is bounded. The kernel
//      is built with `panic = abort`, so an unbounded `push`/`push_str` is a
//      remote kill switch, not just a leak.

/// Per-process window budget.
const MAX_WINDOWS_PER_PROCESS: usize = 8;
/// Widgets a single window may hold.
const MAX_WIDGETS_PER_WINDOW: usize = 128;
/// Cap on any userspace-supplied string (title, label, footer).
const MAX_USER_TEXT_LEN: usize = 256;
/// Largest window a process may ask for, in pixels.
const MAX_USER_WINDOW_DIM: usize = 4096;
/// Undelivered events retained per window before we start dropping them.
const MAX_PENDING_EVENTS: usize = 256;

/// PID of the process currently executing a syscall, if any.
fn calling_pid() -> Option<u64> {
    crate::process::CURRENT_PROCESS.lock().map(|p| p.pid)
}

/// Truncate userspace text on a char boundary so a huge string cannot be
/// used to exhaust the heap.
fn clamp_user_text(s: &str) -> &str {
    if s.len() <= MAX_USER_TEXT_LEN {
        return s;
    }
    let mut end = MAX_USER_TEXT_LEN;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Look up a window the caller is allowed to touch.
///
/// Returns `None` both when the id does not exist and when it belongs to
/// somebody else, so a caller cannot probe which ids are live.
fn owned_window_index(windows: &[Window], id: u64, pid: Option<u64>) -> Option<usize> {
    let idx = windows.iter().position(|w| w.id == id)?;
    match (windows[idx].owner_pid, pid) {
        (Some(owner), Some(caller)) if owner == caller => Some(idx),
        _ => None,
    }
}

pub fn sys_create_window(title: &str, w: usize, h: usize) -> u64 {
    let Some(pid) = calling_pid() else {
        return u64::MAX;
    };
    let title = clamp_user_text(title);
    // Clamp rather than reject: apps ask for silly sizes by accident, and a
    // window wider than the screen breaks hit-testing (`w as i32` truncates).
    let w = w.clamp(64, MAX_USER_WINDOW_DIM);
    let h = h.clamp(48, MAX_USER_WINDOW_DIM);

    let mut wm = WM.lock();
    let owned = wm.windows.iter().filter(|w| w.owner_pid == Some(pid)).count();
    if owned >= MAX_WINDOWS_PER_PROCESS {
        return u64::MAX;
    }
    let n = wm.windows.len() as i32;
    // Position it somewhat randomly or staggered.
    let x = 100 + (n * 20) % 400;
    let y = 100 + (n * 20) % 300;
    let mut win = Window::new(x, y, w, h, title);
    win.owner_pid = Some(pid);
    wm.spawn(win);
    let id = wm.windows.last().map(|w| w.id).unwrap_or(0);
    drop(wm);
    request_redraw();
    id
}

pub fn sys_add_button(win_handle: u64, x: usize, y: usize, w: usize, h: usize, text: &str, action_id: u64) -> u64 {
    let pid = calling_pid();
    let text = clamp_user_text(text);
    let w = w.min(MAX_USER_WINDOW_DIM);
    let h = h.min(MAX_USER_WINDOW_DIM);
    let mut wm = WM.lock();
    if let Some(idx) = owned_window_index(&wm.windows, win_handle, pid) {
        let win = &mut wm.windows[idx];
        if win.widgets.len() >= MAX_WIDGETS_PER_WINDOW {
            return u64::MAX;
        }
        let handle = win.widgets.len() as u64;
        win.widgets.push(Widget::button(x, y, w, h, text, WinAction::User(action_id)));
        drop(wm);
        request_redraw();
        return handle;
    }
    drop(wm);
    u64::MAX
}

pub fn sys_add_label(win_handle: u64, x: usize, y: usize, text: &str) -> u64 {
    let pid = calling_pid();
    let text = clamp_user_text(text);
    let mut wm = WM.lock();
    if let Some(idx) = owned_window_index(&wm.windows, win_handle, pid) {
        let win = &mut wm.windows[idx];
        if win.widgets.len() >= MAX_WIDGETS_PER_WINDOW {
            return u64::MAX;
        }
        let handle = win.widgets.len() as u64;
        win.widgets.push(Widget::label(x, y, text));
        drop(wm);
        request_redraw();
        return handle;
    }
    drop(wm);
    u64::MAX
}

pub fn sys_get_event(win_handle: u64, user_rip: u64, user_rsp: u64) -> u64 {
    let _ = (user_rip, user_rsp);
    let pid = calling_pid();
    {
        let wm = WM.lock();
        if let Some(idx) = owned_window_index(&wm.windows, win_handle, pid) {
            if let Some(event) = wm.windows[idx].user_events.pop() {
                match event {
                    GuiEvent::ButtonClicked { action_id } => {
                        return (action_id << 8) | 1;
                    }
                    GuiEvent::None => {}
                }
            }
        } else {
            return u64::MAX;
        }
    }
    // No event: non-blocking poll. Userspace loops already call `yield_now`.
    0
}

pub fn sys_set_footer(win_handle: u64, text: &str) -> u64 {
    let pid = calling_pid();
    let text = clamp_user_text(text);
    let mut wm = WM.lock();
    if let Some(idx) = owned_window_index(&wm.windows, win_handle, pid) {
        wm.windows[idx].footer = Some(String::from(text));
        drop(wm);
        request_redraw();
        return 0;
    }
    drop(wm);
    u64::MAX
}

pub fn sys_update_widget(win_handle: u64, widget_handle: u64, text: &str) -> u64 {
    let pid = calling_pid();
    let text = clamp_user_text(text);
    let mut wm = WM.lock();
    if let Some(widx) = owned_window_index(&wm.windows, win_handle, pid) {
        if let Some(wgt) = wm.windows[widx].widgets.get_mut(widget_handle as usize) {
            match wgt {
                Widget::Label { text: t, .. } => {
                    t.clear();
                    t.push_str(text);
                }
                Widget::Button { label: l, .. } => {
                    l.clear();
                    l.push_str(text);
                }
                Widget::TextBox { text: t, .. } => {
                    t.clear();
                    t.push_str(text);
                }
                Widget::TextArea { text: t, .. } => {
                    t.clear();
                    t.push_str(text);
                }
                _ => {}
            }
            drop(wm);
            request_redraw();
            return 0;
        }
    }
    drop(wm);
    u64::MAX
}

/// Close every window belonging to `pid`, without touching the scheduler.
/// Called during process teardown.
pub fn close_windows_for_pid(pid: u64) {
    let mut wm = WM.lock();
    let before = wm.windows.len();
    wm.windows.retain(|w| w.owner_pid != Some(pid));
    if wm.windows.len() != before {
        // A removed window shifts every later index, so a drag in flight
        // would otherwise keep moving whatever window slid into its slot.
        wm.dragging = None;
        drop(wm);
        request_redraw();
    }
}

// Keep the send/box scaffolding from earlier phases so Box<dyn Send> stays valid.
const _: fn() = || { let _: Box<dyn Send> = Box::new(0u8); };
