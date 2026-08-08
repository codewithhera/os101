//! Serial port (COM1) driver.
//!
//! Mirrors kernel output to the host terminal via UART 16550. QEMU's
//! `-serial stdio` flag connects this to the terminal that launched it,
//! giving us printf-debugging from day one.

use core::fmt::{self, Write};
use spin::Mutex;
use uart_16550::SerialPort;

/// COM1 at I/O port 0x3F8 — the standard first serial port on x86.
///
/// SAFETY: 0x3F8 is the well-known, fixed I/O address for COM1.
pub static SERIAL1: Mutex<SerialPort> = Mutex::new(unsafe { SerialPort::new(0x3F8) });

/// Initialise the serial port. Must be called before any serial output.
pub fn init() {
    SERIAL1.lock().init();
}

/// Write raw bytes to COM1.
///
/// The `write!` path wants a `fmt::Arguments`, which the C side of the QuickJS
/// shim has no way to build — it arrives with a pointer and a length. Bytes go
/// out untranslated because the UART deals in bytes and the only caller is a
/// printf whose output is already ASCII.
pub fn write(bytes: &[u8]) {
    let mut port = SERIAL1.lock();
    for &byte in bytes {
        port.send(byte);
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    SERIAL1
        .lock()
        .write_fmt(args)
        .expect("serial print failed");
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}
