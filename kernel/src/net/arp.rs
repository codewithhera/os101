//! Address Resolution Protocol — IPv4 address to MAC address.

use alloc::vec::Vec;
use spin::Mutex;

use super::ip::Ipv4Addr;
use super::{BROADCAST_MAC, ETHERTYPE_ARP, ETHERTYPE_IPV4, TICKS_PER_SEC};

const HTYPE_ETHERNET: u16 = 1;
const OP_REQUEST: u16 = 1;
const OP_REPLY: u16 = 2;

/// Cap on cache size. A hobby network never has many peers, and an
/// unbounded cache is a way for a hostile LAN to exhaust the heap.
const MAX_ENTRIES: usize = 32;

static CACHE: Mutex<Vec<(Ipv4Addr, [u8; 6])>> = Mutex::new(Vec::new());

/// Record a mapping, replacing any previous entry for that address.
pub fn learn(ip: Ipv4Addr, mac: [u8; 6]) {
    if ip.is_unspecified() || mac == BROADCAST_MAC || mac == [0; 6] {
        return;
    }
    let mut cache = CACHE.lock();
    if let Some(entry) = cache.iter_mut().find(|(cached, _)| *cached == ip) {
        entry.1 = mac;
        return;
    }
    if cache.len() >= MAX_ENTRIES {
        cache.remove(0);
    }
    cache.push((ip, mac));
}

pub fn lookup(ip: Ipv4Addr) -> Option<[u8; 6]> {
    CACHE.lock().iter().find(|(cached, _)| *cached == ip).map(|(_, mac)| *mac)
}

pub fn entries() -> Vec<(Ipv4Addr, [u8; 6])> {
    CACHE.lock().clone()
}

/// Look up `ip`, asking on the wire if it is not already known.
pub fn resolve(ip: Ipv4Addr) -> Option<[u8; 6]> {
    if let Some(mac) = lookup(ip) {
        return Some(mac);
    }

    // Three tries: the first request can easily be lost while the peer is
    // still bringing its own link up.
    for _ in 0..3 {
        if request(ip).is_err() {
            return None;
        }
        if super::wait_until(|| lookup(ip).is_some(), TICKS_PER_SEC) {
            return lookup(ip);
        }
    }
    None
}

/// Broadcast a "who has this address" query.
pub fn request(target: Ipv4Addr) -> Result<(), &'static str> {
    let (_, mac, src_ip, _, _, _) = super::config();
    let packet = build(OP_REQUEST, mac, src_ip, [0; 6], target);
    super::send_frame(BROADCAST_MAC, ETHERTYPE_ARP, &packet)
}

fn build(
    op: u16,
    sender_mac: [u8; 6],
    sender_ip: Ipv4Addr,
    target_mac: [u8; 6],
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(28);
    p.extend_from_slice(&HTYPE_ETHERNET.to_be_bytes());
    p.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    p.push(6); // hardware address length
    p.push(4); // protocol address length
    p.extend_from_slice(&op.to_be_bytes());
    p.extend_from_slice(&sender_mac);
    p.extend_from_slice(&sender_ip.octets());
    p.extend_from_slice(&target_mac);
    p.extend_from_slice(&target_ip.octets());
    p
}

/// Process a received ARP packet: learn from it, and answer queries for us.
pub fn handle(packet: &[u8]) {
    if packet.len() < 28 {
        return;
    }
    let htype = u16::from_be_bytes([packet[0], packet[1]]);
    let ptype = u16::from_be_bytes([packet[2], packet[3]]);
    if htype != HTYPE_ETHERNET || ptype != ETHERTYPE_IPV4 || packet[4] != 6 || packet[5] != 4 {
        return;
    }

    let op = u16::from_be_bytes([packet[6], packet[7]]);
    let sender_mac: [u8; 6] = match packet[8..14].try_into() {
        Ok(m) => m,
        Err(_) => return,
    };
    let Some(sender_ip) = Ipv4Addr::from_slice(&packet[14..18]) else { return };
    let Some(target_ip) = Ipv4Addr::from_slice(&packet[24..28]) else { return };

    learn(sender_ip, sender_mac);

    let (_, mac, local_ip, _, _, _) = super::config();
    if op == OP_REQUEST && !local_ip.is_unspecified() && target_ip == local_ip {
        let reply = build(OP_REPLY, mac, local_ip, sender_mac, sender_ip);
        let _ = super::send_frame(sender_mac, ETHERTYPE_ARP, &reply);
    }
}
