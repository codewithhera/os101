#![no_std]

pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_YIELD: u64 = 3;
pub const SYS_SBRK: u64 = 4;
pub const SYS_TIME_MS: u64 = 5;
pub const SYS_GUI_CREATE_WINDOW: u64 = 10;
pub const SYS_GUI_ADD_BUTTON: u64 = 11;
pub const SYS_GUI_ADD_LABEL: u64 = 12;
pub const SYS_GUI_GET_EVENT: u64 = 13;
pub const SYS_GUI_UPDATE_WIDGET: u64 = 14;
pub const SYS_GUI_SET_FOOTER: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiEvent {
    None,
    ButtonClicked { action_id: u64 },
}

#[inline]
pub fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline]
pub fn syscall6(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            in("r10") a4,
            in("r8") a5,
            in("r9") a6,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline]
pub fn write(buf: &[u8]) -> u64 {
    syscall3(SYS_WRITE, buf.as_ptr() as u64, buf.len() as u64, 0)
}

#[inline]
pub fn yield_now() {
    let _ = syscall3(SYS_YIELD, 0, 0, 0);
}

/// Move the heap break by `increment` bytes and return where it was, or
/// `None` if the heap cannot grow that far.
///
/// This is what an allocator is built on. A Rust application usually has no
/// need for it — `linked_list_allocator` over a static array is simpler — but
/// it is the same call the C library's `malloc` makes, so both languages end
/// up with a heap that grows on demand instead of one fixed at build time.
#[inline]
pub fn sbrk(increment: i64) -> Option<*mut u8> {
    let previous = syscall3(SYS_SBRK, increment as u64, 0, 0);
    if previous == u64::MAX {
        return None;
    }
    Some(previous as *mut u8)
}

/// Milliseconds since the Unix epoch, from the machine's real-time clock.
#[inline]
pub fn time_millis() -> i64 {
    syscall3(SYS_TIME_MS, 0, 0, 0) as i64
}

#[inline]
pub fn exit(code: u64) -> ! {
    let _ = syscall3(SYS_EXIT, code, 0, 0);
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack, preserves_flags)); }
    }
}

pub fn gui_create_window(title: &str, w: usize, h: usize) -> u64 {
    let packed_size = ((w as u64) << 32) | (h as u64);
    syscall3(SYS_GUI_CREATE_WINDOW, title.as_ptr() as u64, title.len() as u64, packed_size)
}

pub fn gui_add_button(win_handle: u64, x: usize, y: usize, w: usize, h: usize, text: &str, action_id: u64) -> u64 {
    let packed_pos = ((x as u64) << 32) | (y as u64);
    let packed_size = ((w as u64) << 32) | (h as u64);
    syscall6(SYS_GUI_ADD_BUTTON, win_handle, packed_pos, packed_size, text.as_ptr() as u64, text.len() as u64, action_id)
}

pub fn gui_add_label(win_handle: u64, x: usize, y: usize, text: &str) -> u64 {
    let packed_pos = ((x as u64) << 32) | (y as u64);
    syscall6(SYS_GUI_ADD_LABEL, win_handle, packed_pos, text.as_ptr() as u64, text.len() as u64, 0, 0)
}

pub fn gui_get_event(win_handle: u64) -> GuiEvent {
    let res = syscall3(SYS_GUI_GET_EVENT, win_handle, 0, 0);
    if res == 0 {
        return GuiEvent::None;
    }
    let event_type = res & 0xFF;
    let payload = res >> 8;
    match event_type {
        1 => GuiEvent::ButtonClicked { action_id: payload },
        _ => GuiEvent::None,
    }
}

pub fn gui_update_widget(win_handle: u64, widget_handle: u64, text: &str) -> u64 {
    syscall6(SYS_GUI_UPDATE_WIDGET, win_handle, widget_handle, text.as_ptr() as u64, text.len() as u64, 0, 0)
}

/// Set the footer/status-bar text on the given window. Drawn in the
/// reserved FOOTER_H band along the bottom of the content area.
pub fn gui_set_footer(win_handle: u64, text: &str) -> u64 {
    syscall3(SYS_GUI_SET_FOOTER, win_handle, text.as_ptr() as u64, text.len() as u64)
}
