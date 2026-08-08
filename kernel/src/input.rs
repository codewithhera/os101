//! Unified Input Core with Lock-Free Concurrency.
//!
//! Uses `crossbeam_queue::SegQueue` to ensure that interrupt handlers
//! can push events without deadlocking against the main loop.

use crossbeam_queue::SegQueue;
use pc_keyboard::DecodedKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Key(DecodedKey),
    MouseButton { left: bool, right: bool, double_clicked: bool },
    /// Wheel movement in detents; negative is up, away from the user.
    MouseWheel { delta: i8 },
}

lazy_static::lazy_static! {
    /// Lock-free FIFO queue for input events.
    static ref EVENT_QUEUE: SegQueue<InputEvent> = SegQueue::new();
}

/// Push a new event into the unified queue. Lock-free and interrupt-safe.
pub fn push(event: InputEvent) {
    EVENT_QUEUE.push(event);
}

/// Pop the next event from the queue. Returns None if empty.
pub fn pop() -> Option<InputEvent> {
    EVENT_QUEUE.pop()
}

/// Explicitly initialize the input core. Must be called before interrupts are enabled.
pub fn init() {
    lazy_static::initialize(&EVENT_QUEUE);
}
