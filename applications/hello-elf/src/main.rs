#![no_std]
#![no_main]

use linked_list_allocator::LockedHeap;
use os101_user::{exit, gui_add_label, gui_create_window, yield_now};

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    static mut HEAP: [u8; 16 * 1024] = [0; 16 * 1024];
    unsafe {
        let ptr = core::ptr::addr_of_mut!(HEAP) as *mut u8;
        ALLOCATOR.lock().init(ptr, 16 * 1024);
    }

    let win = gui_create_window("Hello ELF", 300, 140);
    gui_add_label(win, 16, 24, "Hello from userspace ELF!");
    gui_add_label(win, 16, 52, "This app is running correctly.");

    loop {
        yield_now();
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    exit(1)
}
