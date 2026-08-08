//! Embedded image icons for applications, desktop items, files, and folders.
//!
//! Icons are real PNG assets under `kernel/assets/icons/`. They are decoded
//! once on first use and then scaled by the framebuffer renderer. Keeping the
//! lookup API here means the launcher, desktop, titlebars, file manager, and
//! taskbar all use the same artwork.

use alloc::boxed::Box;
use spin::Mutex;

use crate::color::Color;
use crate::framebuffer;
use crate::image::{self, Image};
use crate::window::WindowKind;

/// Logical titlebar/list icon size. Larger surfaces scale the same source
/// image up through [`draw_scaled`].
pub const ICON_SIZE: usize = 16;

pub struct Icon {
    bytes: &'static [u8],
    decoded: Mutex<Option<&'static Image>>,
}

impl Icon {
    const fn new(bytes: &'static [u8]) -> Self {
        Self {
            bytes,
            decoded: Mutex::new(None),
        }
    }

    fn image(&self) -> Option<&'static Image> {
        let mut slot = self.decoded.lock();
        if slot.is_none() {
            *slot = image::decode(self.bytes).map(|img| &*Box::leak(Box::new(img)));
        }
        *slot
    }
}

/// Draw an image icon at the standard 16×16 size.
///
/// The palette arguments remain in the public API so existing UI call sites
/// do not need special cases. Image icons already contain their final colours.
pub fn draw(
    x: usize,
    y: usize,
    icon: &Icon,
    _outline: impl Into<Color>,
    _fill: impl Into<Color>,
    _accent: impl Into<Color>,
) {
    draw_scaled(x, y, ICON_SIZE, icon, Color::BLACK, Color::BLACK, Color::BLACK);
}

/// Draw an icon image scaled to a square `size`.
pub fn draw_scaled(
    x: usize,
    y: usize,
    size: usize,
    icon: &Icon,
    _outline: impl Into<Color>,
    _fill: impl Into<Color>,
    _accent: impl Into<Color>,
) {
    let Some(img) = icon.image() else {
        return;
    };
    framebuffer::blit_scaled(
        x,
        y as isize,
        size,
        size,
        &img.pixels,
        img.width,
        img.height,
        x.saturating_add(size),
        None,
    );
}

pub fn for_window_kind(kind: WindowKind) -> &'static Icon {
    match kind {
        WindowKind::Calculator => &CALCULATOR,
        WindowKind::Launcher => &APPS,
        WindowKind::FileManager | WindowKind::FileDialog => &FOLDER,
        WindowKind::Notepad => &NOTEPAD,
        WindowKind::Terminal => &TERMINAL,
        WindowKind::Monitor => &MONITOR,
        WindowKind::Paint => &PAINT,
        WindowKind::Snake => &SNAKE,
        WindowKind::Breakout => &BREAKOUT,
        WindowKind::Abc => &ABC,
        WindowKind::Racing => &RACING,
        WindowKind::Invaders => &INVADERS,
        WindowKind::ImageView => &PICTURE,
        WindowKind::Settings => &SETTINGS,
        WindowKind::About => &ABOUT,
        WindowKind::MyComputer => &COMPUTER,
        WindowKind::Browser => &GLOBE,
        WindowKind::CodeEditor => &C_APP,
        WindowKind::Installer => &DRIVE,
        WindowKind::Generic => &APP,
    }
}

/// Badge/accent colours are still useful around image icons.
pub fn accent_for_window_kind(kind: WindowKind) -> Color {
    use crate::theme;
    match kind {
        WindowKind::Calculator => theme::ACCENT,
        WindowKind::Launcher => Color::hex(0xA855F7),
        WindowKind::FileManager | WindowKind::FileDialog => Color::hex(0xF59E0B),
        WindowKind::Notepad => Color::hex(0x38BDF8),
        WindowKind::Terminal => theme::SUCCESS,
        WindowKind::Monitor => Color::hex(0x14B8A6),
        WindowKind::Paint => Color::hex(0xEC4899),
        WindowKind::Snake => Color::hex(0x22C55E),
        WindowKind::Breakout => Color::hex(0xF59E0B),
        WindowKind::Abc => Color::hex(0xA855F7),
        WindowKind::Racing => Color::hex(0x22D3EE),
        WindowKind::Invaders => Color::hex(0xF472B6),
        WindowKind::ImageView => Color::hex(0xF97316),
        WindowKind::Settings => Color::hex(0x94A3B8),
        WindowKind::About => Color::hex(0x60A5FA),
        WindowKind::MyComputer => Color::hex(0x38BDF8),
        WindowKind::Browser => Color::hex(0x0EA5E9),
        WindowKind::CodeEditor => Color::hex(0x34D399),
        WindowKind::Installer => Color::hex(0xF97316),
        WindowKind::Generic => theme::ACCENT,
    }
}

pub fn accent_for_icon(icon: &Icon) -> Color {
    let table: [(&Icon, WindowKind); 19] = [
        (&CALCULATOR, WindowKind::Calculator),
        (&APPS, WindowKind::Launcher),
        (&FOLDER, WindowKind::FileManager),
        (&NOTEPAD, WindowKind::Notepad),
        (&TERMINAL, WindowKind::Terminal),
        (&MONITOR, WindowKind::Monitor),
        (&PAINT, WindowKind::Paint),
        (&SNAKE, WindowKind::Snake),
        (&BREAKOUT, WindowKind::Breakout),
        (&ABC, WindowKind::Abc),
        (&RACING, WindowKind::Racing),
        (&INVADERS, WindowKind::Invaders),
        (&PICTURE, WindowKind::ImageView),
        (&SETTINGS, WindowKind::Settings),
        (&ABOUT, WindowKind::About),
        (&COMPUTER, WindowKind::MyComputer),
        (&GLOBE, WindowKind::Browser),
        (&C_APP, WindowKind::CodeEditor),
        (&DRIVE, WindowKind::Installer),
    ];
    for (candidate, kind) in table {
        if core::ptr::eq(candidate, icon) {
            return accent_for_window_kind(kind);
        }
    }
    accent_for_window_kind(WindowKind::Generic)
}

/// Icon looked up by launcher display name.
pub fn for_app_name(name: &str) -> &'static Icon {
    match name {
        "Calculator" => &CALCULATOR,
        "Hello C" => &C_APP,
        "Hello C++" => &CPP_APP,
        "Hello ELF" => &BINARY_APP,
        "WinGUI" => &GUI_APP,
        "About" => &ABOUT,
        "File Manager" => &FOLDER,
        "Image Viewer" => &PICTURE,
        "Monitor" => &MONITOR,
        "Notepad" => &NOTEPAD,
        "Paint" => &PAINT,
        "Settings" => &SETTINGS,
        "Snake" => &SNAKE,
        "Breakout" => &BREAKOUT,
        "ABC Fun" => &ABC,
        "Race Cars" => &RACING,
        "Space Invaders" => &INVADERS,
        "Terminal" => &TERMINAL,
        "Code Editor" => &C_APP,
        "My Computer" => &COMPUTER,
        "Web Browser" => &GLOBE,
        "Install OS101" => &DRIVE,
        _ => &APP,
    }
}

macro_rules! icon {
    ($name:ident, $file:literal) => {
        pub static $name: Icon =
            Icon::new(include_bytes!(concat!("../assets/icons/", $file)));
    };
}

icon!(APP, "app.png");
icon!(APPS, "apps.png");
icon!(CALCULATOR, "calculator.png");
icon!(FOLDER, "folder.png");
icon!(NOTEPAD, "notepad.png");
icon!(TERMINAL, "terminal.png");
icon!(MONITOR, "monitor.png");
icon!(GLOBE, "globe.png");
icon!(PAINT, "paint.png");
icon!(SNAKE, "snake.png");
icon!(BREAKOUT, "breakout.png");
icon!(RACING, "racing.png");
icon!(INVADERS, "invaders.png");
icon!(ABC, "abc.png");
icon!(PICTURE, "picture.png");
icon!(SETTINGS, "settings.png");
icon!(ABOUT, "about.png");
icon!(COMPUTER, "computer.png");
icon!(FILE, "file.png");
icon!(DRIVE, "drive.png");
icon!(POWER, "power.png");
icon!(C_APP, "c_app.png");
icon!(CPP_APP, "cpp_app.png");
icon!(BINARY_APP, "binary_app.png");
icon!(GUI_APP, "gui_app.png");

/// Decode every shipped icon once so a missing/corrupt asset is visible in
/// the boot self-test instead of showing up later as an empty launcher tile.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();
    let icons: [(&str, &Icon); 25] = [
        ("app", &APP),
        ("apps", &APPS),
        ("calculator", &CALCULATOR),
        ("folder", &FOLDER),
        ("notepad", &NOTEPAD),
        ("terminal", &TERMINAL),
        ("monitor", &MONITOR),
        ("globe", &GLOBE),
        ("paint", &PAINT),
        ("snake", &SNAKE),
        ("breakout", &BREAKOUT),
        ("racing", &RACING),
        ("invaders", &INVADERS),
        ("abc", &ABC),
        ("picture", &PICTURE),
        ("settings", &SETTINGS),
        ("about", &ABOUT),
        ("computer", &COMPUTER),
        ("file", &FILE),
        ("drive", &DRIVE),
        ("power", &POWER),
        ("c app", &C_APP),
        ("c++ app", &CPP_APP),
        ("binary app", &BINARY_APP),
        ("gui app", &GUI_APP),
    ];
    for (name, icon) in icons {
        let valid = icon
            .image()
            .is_some_and(|img| img.width == 96 && img.height == 96);
        report.check(name, valid);
    }
    report
}
