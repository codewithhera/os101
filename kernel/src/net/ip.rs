//! IPv4 addressing, header handling and dispatch.

use alloc::vec::Vec;
use core::fmt;

use super::{arp, icmp, tcp, udp};

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

pub const IPV4_HEADER_LEN: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const UNSPECIFIED: Ipv4Addr = Ipv4Addr([0, 0, 0, 0]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);

    pub const fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Ipv4Addr([a, b, c, d])
    }

    pub fn from_slice(s: &[u8]) -> Option<Self> {
        Some(Ipv4Addr(s.get(..4)?.try_into().ok()?))
    }

    pub fn is_unspecified(&self) -> bool {
        self.0 == [0, 0, 0, 0]
    }

    pub fn octets(&self) -> [u8; 4] {
        self.0
    }

    /// Parse dotted-quad notation. Returns `None` for anything else, which
    /// is how callers tell an address from a hostname.
    pub fn parse(s: &str) -> Option<Self> {
        let mut octets = [0u8; 4];
        let mut count = 0;
        for part in s.split('.') {
            if count == 4 || part.is_empty() || part.len() > 3 {
                return None;
            }
            octets[count] = part.parse::<u8>().ok()?;
            count += 1;
        }
        if count == 4 { Some(Ipv4Addr(octets)) } else { None }
    }

    /// Whether `other` is reachable directly rather than via the router.
    pub fn same_subnet(&self, other: Ipv4Addr, netmask: Ipv4Addr) -> bool {
        (0..4).all(|i| self.0[i] & netmask.0[i] == other.0[i] & netmask.0[i])
    }
}

impl fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

/// The ones-complement sum used by IP, ICMP, UDP and TCP.
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        sum += u16::from_be_bytes([c[0], c[1]]) as u32;
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += (last as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Checksum over a TCP/UDP pseudo-header plus the segment itself.
pub fn transport_checksum(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, segment: &[u8]) -> u16 {
    let mut buf = Vec::with_capacity(12 + segment.len());
    buf.extend_from_slice(&src.0);
    buf.extend_from_slice(&dst.0);
    buf.push(0);
    buf.push(proto);
    buf.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    buf.extend_from_slice(segment);
    checksum(&buf)
}

/// Build an IPv4 packet around `payload`.
pub fn build(src: Ipv4Addr, dst: Ipv4Addr, proto: u8, payload: &[u8], ident: u16) -> Vec<u8> {
    let total_len = (IPV4_HEADER_LEN + payload.len()) as u16;
    let mut pkt = Vec::with_capacity(total_len as usize);

    pkt.push(0x45); // IPv4, 5 dwords of header
    pkt.push(0); // DSCP/ECN
    pkt.extend_from_slice(&total_len.to_be_bytes());
    pkt.extend_from_slice(&ident.to_be_bytes());
    pkt.extend_from_slice(&0x4000u16.to_be_bytes()); // don't fragment
    pkt.push(64); // TTL
    pkt.push(proto);
    pkt.extend_from_slice(&[0, 0]); // checksum, filled in below
    pkt.extend_from_slice(&src.0);
    pkt.extend_from_slice(&dst.0);

    let sum = checksum(&pkt[..IPV4_HEADER_LEN]);
    pkt[10..12].copy_from_slice(&sum.to_be_bytes());

    pkt.extend_from_slice(payload);
    pkt
}

/// Send an IPv4 packet, resolving the next hop's MAC address first.
pub fn send(dst: Ipv4Addr, proto: u8, payload: &[u8]) -> Result<(), &'static str> {
    let (configured, _mac, src, netmask, gateway, _dns) = super::config();
    if !configured && !dst.is_unspecified() && proto != PROTO_UDP {
        return Err("interface has no address (run `net up`)");
    }

    // Off-subnet traffic goes to the router.
    let next_hop = if dst == Ipv4Addr::BROADCAST || dst.same_subnet(src, netmask) {
        dst
    } else {
        if gateway.is_unspecified() {
            return Err("no default gateway");
        }
        gateway
    };

    let dst_mac = if next_hop == Ipv4Addr::BROADCAST {
        super::BROADCAST_MAC
    } else {
        arp::resolve(next_hop).ok_or("ARP resolution failed")?
    };

    static IDENT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(1);
    let ident = IDENT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let packet = build(src, dst, proto, payload, ident);
    super::send_frame(dst_mac, super::ETHERTYPE_IPV4, &packet)
}

/// Handle a received IPv4 packet.
pub fn handle(packet: &[u8], src_mac: [u8; 6]) {
    if packet.len() < IPV4_HEADER_LEN {
        return;
    }
    let version_ihl = packet[0];
    if version_ihl >> 4 != 4 {
        return;
    }
    let header_len = ((version_ihl & 0x0F) as usize) * 4;
    if header_len < IPV4_HEADER_LEN || packet.len() < header_len {
        return;
    }

    let total_len = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    if total_len < header_len || total_len > packet.len() {
        // Frames are padded to 60 bytes, so a short total_len is normal;
        // anything longer than the frame is malformed.
        if total_len > packet.len() {
            return;
        }
    }

    // Fragmented packets are not reassembled. Dropping them is safer than
    // handing a partial datagram up the stack.
    let flags_frag = u16::from_be_bytes([packet[6], packet[7]]);
    if flags_frag & 0x2000 != 0 || flags_frag & 0x1FFF != 0 {
        return;
    }

    let proto = packet[9];
    let Some(src) = Ipv4Addr::from_slice(&packet[12..16]) else { return };
    let Some(dst) = Ipv4Addr::from_slice(&packet[16..20]) else { return };

    let local = super::local_ip();
    if !local.is_unspecified() && dst != local && dst != Ipv4Addr::BROADCAST {
        return;
    }

    // Learning the sender's MAC here saves an ARP round trip on the reply.
    arp::learn(src, src_mac);

    let payload = &packet[header_len..total_len.max(header_len)];
    match proto {
        PROTO_ICMP => icmp::handle(src, payload),
        PROTO_UDP => udp::handle(src, payload),
        PROTO_TCP => tcp::handle(src, payload),
        _ => {}
    }
}
