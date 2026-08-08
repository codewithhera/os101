#![no_std]
#![no_main]

use os101_user::{exit, write, yield_now};

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let _ = write(b"hello from sample_app\n");
    yield_now();
    exit(0)
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    exit(1)
}
