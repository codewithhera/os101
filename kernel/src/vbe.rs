//! Bochs VBE mode setting — a bigger screen than the bootloader hands us.
//!
//! The bootloader's BIOS stage picks the VESA mode, and it hardcodes a
//! 1280x720 ceiling, skipping every larger mode the card advertises. That is
//! well below what the emulated (and any real) adapter can do.
//!
//! QEMU's VGA, Bochs' and VirtualBox's all expose the same small register
//! window — an index port and a data port — through which the OS can ask for
//! an arbitrary resolution and a linear framebuffer. Setting the mode here,
//! before [`crate::framebuffer::init`] sizes its buffers, means the rest of the
//! kernel never has to know the display was renegotiated.
//!
//! Cards without this interface (most real hardware) are left alone, and the
//! caller falls back to whatever the bootloader set up.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};

use crate::memory;
use crate::pci;

const INDEX_PORT: u16 = 0x01CE;
const DATA_PORT: u16 = 0x01CF;

/// Register indices, written to [`INDEX_PORT`] to select what [`DATA_PORT`]
/// reads and writes.
mod index {
    pub const ID: u16 = 0;
    pub const XRES: u16 = 1;
    pub const YRES: u16 = 2;
    pub const BPP: u16 = 3;
    pub const ENABLE: u16 = 4;
    pub const BANK: u16 = 5;
    pub const VIRT_WIDTH: u16 = 6;
    pub const VIRT_HEIGHT: u16 = 7;
    pub const X_OFFSET: u16 = 8;
    pub const Y_OFFSET: u16 = 9;
}

/// Bits for the `ENABLE` register.
mod enable {
    pub const DISABLED: u16 = 0x00;
    pub const ENABLED: u16 = 0x01;
    /// Makes the next three reads of `XRES`/`YRES`/`BPP` report the card's
    /// maximums instead of the current mode.
    pub const GETCAPS: u16 = 0x02;
    pub const LFB: u16 = 0x40;
}

/// Interface revisions, oldest to newest. Anything outside this range means
/// there is no Bochs VBE here and the ports belong to something else.
const ID_MIN: u16 = 0xB0C0;
const ID_MAX: u16 = 0xB0C5;

/// 32 bits per pixel keeps every pixel 4-byte aligned, which both the blitter
/// and the card prefer over packed 24-bit.
const BPP: u16 = 32;

/// PCI class code for a display controller.
const CLASS_DISPLAY: u8 = 0x03;

unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value,
                     options(nomem, nostack, preserves_flags));
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", in("dx") port, out("ax") value,
                     options(nomem, nostack, preserves_flags));
    value
}

fn read_reg(reg: u16) -> u16 {
    unsafe {
        outw(INDEX_PORT, reg);
        inw(DATA_PORT)
    }
}

fn write_reg(reg: u16, value: u16) {
    unsafe {
        outw(INDEX_PORT, reg);
        outw(DATA_PORT, value);
    }
}

/// The resolution and colour depth currently programmed into the card.
#[derive(Clone, Copy)]
struct Mode {
    width: u16,
    height: u16,
    bpp: u16,
}

fn current_mode() -> Mode {
    Mode {
        width: read_reg(index::XRES),
        height: read_reg(index::YRES),
        bpp: read_reg(index::BPP),
    }
}

/// Program a mode and report whether the card accepted it.
///
/// The card silently refuses anything that does not fit in its video memory,
/// leaving `ENABLE` clear, so the readback is the only reliable answer.
fn apply(mode: Mode) -> bool {
    write_reg(index::ENABLE, enable::DISABLED);
    write_reg(index::XRES, mode.width);
    write_reg(index::YRES, mode.height);
    write_reg(index::BPP, mode.bpp);
    write_reg(index::VIRT_WIDTH, mode.width);
    write_reg(index::VIRT_HEIGHT, mode.height);
    write_reg(index::BANK, 0);
    write_reg(index::X_OFFSET, 0);
    write_reg(index::Y_OFFSET, 0);
    write_reg(index::ENABLE, enable::ENABLED | enable::LFB);

    read_reg(index::ENABLE) & enable::ENABLED != 0
        && read_reg(index::XRES) == mode.width
        && read_reg(index::YRES) == mode.height
}

/// Ask the card for its maximum resolution and depth.
///
/// Reading the capabilities means going through `DISABLED`, so the current
/// mode is lost; the caller is responsible for programming a mode afterwards.
fn capabilities() -> Mode {
    write_reg(index::ENABLE, enable::GETCAPS);
    let caps = current_mode();
    write_reg(index::ENABLE, enable::DISABLED);
    caps
}

/// Physical base of the linear framebuffer, taken from the display
/// controller's first memory BAR.
fn linear_framebuffer_base() -> Option<u64> {
    let device = pci::scan()
        .into_iter()
        .find(|d| d.class == CLASS_DISPLAY)?;
    match device.bar(0) {
        Some(pci::Bar::Memory(base)) if base != 0 => {
            device.enable(pci::command::MEMORY_SPACE);
            Some(base)
        }
        _ => None,
    }
}

/// Switch the display to `width` x `height` and return the framebuffer to draw
/// into, or `None` if this machine has no Bochs VBE or cannot manage that mode.
///
/// On failure the card is left in the mode the bootloader set up, so the
/// caller can fall back to the framebuffer it was handed without the screen
/// going dark in between.
///
/// # Safety
///
/// Must be called before [`crate::framebuffer::init`] and only once, while the
/// bootloader's framebuffer is still unused: it invalidates the geometry that
/// framebuffer was described with.
pub unsafe fn set_mode(width: u16, height: u16) -> Option<(&'static mut [u8], FrameBufferInfo)> {
    let id = read_reg(index::ID);
    if !(ID_MIN..=ID_MAX).contains(&id) {
        crate::serial::_print(format_args!(
            "VBE: no Bochs VBE interface (id {:#06x}), keeping the bootloader's mode\n",
            id
        ));
        return None;
    }

    let base = linear_framebuffer_base()?;
    let previous = current_mode();

    let caps = capabilities();
    if width > caps.width || height > caps.height || BPP > caps.bpp {
        crate::serial::_print(format_args!(
            "VBE: {}x{}x{} exceeds the card's {}x{}x{}, keeping {}x{}\n",
            width, height, BPP, caps.width, caps.height, caps.bpp, previous.width, previous.height
        ));
        apply(previous);
        return None;
    }

    let wanted = Mode { width, height, bpp: BPP };
    if !apply(wanted) {
        crate::serial::_print(format_args!(
            "VBE: card rejected {}x{}x{}, falling back to {}x{}\n",
            width, height, BPP, previous.width, previous.height
        ));
        apply(previous);
        return None;
    }

    let bytes_per_pixel = (BPP / 8) as usize;
    let info = FrameBufferInfo {
        byte_len: width as usize * height as usize * bytes_per_pixel,
        width: width as usize,
        height: height as usize,
        // The card packs 32-bit pixels little-endian, so the low byte — the
        // first one in memory — is blue. The top byte is padding.
        pixel_format: PixelFormat::Bgr,
        bytes_per_pixel,
        stride: width as usize,
    };

    // Every physical address is already mapped at a fixed offset, the same
    // window the NIC reaches its registers through, so the BAR needs no
    // mapping of its own.
    let virt = memory::physical_memory_offset() + base;
    let buffer = unsafe { core::slice::from_raw_parts_mut(virt as *mut u8, info.byte_len) };

    crate::serial::_print(format_args!(
        "VBE: display set to {}x{}x{} (lfb {:#x}, was {}x{})\n",
        width, height, BPP, base, previous.width, previous.height
    ));

    Some((buffer, info))
}
