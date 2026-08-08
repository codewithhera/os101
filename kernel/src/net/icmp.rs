//! ICMP — enough for `ping` to work in both directions.

use alloc::vec::Vec;
use spin::Mutex;

use super::ip::{self, Ipv4Addr};
use super::TICKS_PER_SEC;

const ECHO_REPLY: u8 = 0;
const ECHO_REQUEST: u8 = 8;

/// Replies we have seen, as (identifier, sequence, source).
static REPLIES: Mutex<Vec<(u16, u16, Ipv4Addr)>> = Mutex::new(Vec::new());

fn build_echo(kind: u8, ident: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(8 + payload.len());
    msg.push(kind);
    msg.push(0); // code
    msg.extend_from_slice(&[0, 0]); // checksum placeholder
    msg.extend_from_slice(&ident.to_be_bytes());
    msg.extend_from_slice(&seq.to_be_bytes());
    msg.extend_from_slice(payload);

    let sum = ip::checksum(&msg);
    msg[2..4].copy_from_slice(&sum.to_be_bytes());
    msg
}

/// Send one echo request and wait for its reply.
///
/// Returns the round-trip time in PIT ticks, or `None` on timeout.
pub fn ping(dst: Ipv4Addr, seq: u16, timeout_ticks: u64) -> Option<u64> {
    const IDENT: u16 = 0x0501;
    let payload = b"os101 ping payload.....";

    REPLIES.lock().retain(|(i, s, _)| !(*i == IDENT && *s == seq));

    let msg = build_echo(ECHO_REQUEST, IDENT, seq, payload);
    let start = crate::clock::ticks();
    ip::send(dst, ip::PROTO_ICMP, &msg).ok()?;

    let arrived = super::wait_until(
        || {
            REPLIES
                .lock()
                .iter()
                .any(|(i, s, from)| *i == IDENT && *s == seq && *from == dst)
        },
        timeout_ticks,
    );

    if !arrived {
        return None;
    }
    REPLIES.lock().retain(|(i, s, _)| !(*i == IDENT && *s == seq));
    Some(crate::clock::ticks().wrapping_sub(start))
}

pub fn handle(src: Ipv4Addr, msg: &[u8]) {
    if msg.len() < 8 {
        return;
    }
    let kind = msg[0];
    let ident = u16::from_be_bytes([msg[4], msg[5]]);
    let seq = u16::from_be_bytes([msg[6], msg[7]]);

    match kind {
        ECHO_REQUEST => {
            // Answer with the same payload, as the protocol requires.
            let reply = build_echo(ECHO_REPLY, ident, seq, &msg[8..]);
            let _ = ip::send(src, ip::PROTO_ICMP, &reply);
        }
        ECHO_REPLY => {
            let mut replies = REPLIES.lock();
            if replies.len() < 16 {
                replies.push((ident, seq, src));
            }
        }
        _ => {}
    }
}

/// Default ping timeout.
pub fn default_timeout() -> u64 {
    2 * TICKS_PER_SEC
}
