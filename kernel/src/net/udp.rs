//! UDP datagrams and a small receive queue.
//!
//! There are no sockets in the POSIX sense. A caller binds a port, sends,
//! then polls [`recv`] until its reply turns up — which is all DHCP and DNS
//! need.

use alloc::vec::Vec;
use spin::Mutex;

use super::ip::{self, Ipv4Addr};

pub const UDP_HEADER_LEN: usize = 8;

/// Bounded so a flood of unwanted datagrams cannot exhaust the heap.
const MAX_QUEUED: usize = 16;

pub struct Datagram {
    pub src: Ipv4Addr,
    pub src_port: u16,
    pub dst_port: u16,
    pub data: Vec<u8>,
}

static BOUND: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static QUEUE: Mutex<Vec<Datagram>> = Mutex::new(Vec::new());

/// Start accepting datagrams for `port`.
pub fn bind(port: u16) {
    let mut bound = BOUND.lock();
    if !bound.contains(&port) {
        bound.push(port);
    }
}

pub fn unbind(port: u16) {
    BOUND.lock().retain(|p| *p != port);
    QUEUE.lock().retain(|d| d.dst_port != port);
}

/// Take the oldest queued datagram for `port`, if any.
pub fn recv(port: u16) -> Option<Datagram> {
    let mut queue = QUEUE.lock();
    let idx = queue.iter().position(|d| d.dst_port == port)?;
    Some(queue.remove(idx))
}

pub fn send(dst: Ipv4Addr, src_port: u16, dst_port: u16, data: &[u8]) -> Result<(), &'static str> {
    let src = super::local_ip();

    let mut segment = Vec::with_capacity(UDP_HEADER_LEN + data.len());
    segment.extend_from_slice(&src_port.to_be_bytes());
    segment.extend_from_slice(&dst_port.to_be_bytes());
    segment.extend_from_slice(&((UDP_HEADER_LEN + data.len()) as u16).to_be_bytes());
    segment.extend_from_slice(&[0, 0]); // checksum placeholder
    segment.extend_from_slice(data);

    // UDP checksums are optional over IPv4; a zero means "not computed", and
    // the all-ones case has to be encoded as 0xFFFF instead.
    let sum = ip::transport_checksum(src, dst, ip::PROTO_UDP, &segment);
    let sum = if sum == 0 { 0xFFFF } else { sum };
    segment[6..8].copy_from_slice(&sum.to_be_bytes());

    ip::send(dst, ip::PROTO_UDP, &segment)
}

pub fn handle(src: Ipv4Addr, segment: &[u8]) {
    if segment.len() < UDP_HEADER_LEN {
        return;
    }
    let src_port = u16::from_be_bytes([segment[0], segment[1]]);
    let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
    let length = u16::from_be_bytes([segment[4], segment[5]]) as usize;

    if length < UDP_HEADER_LEN || length > segment.len() {
        return;
    }
    if !BOUND.lock().contains(&dst_port) {
        return;
    }

    let mut queue = QUEUE.lock();
    if queue.len() >= MAX_QUEUED {
        queue.remove(0);
    }
    queue.push(Datagram {
        src,
        src_port,
        dst_port,
        data: segment[UDP_HEADER_LEN..length].to_vec(),
    });
}
