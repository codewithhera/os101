//! A small IPv4 network stack.
//!
//! Layering, bottom to top: [`e1000`] drives the card, this module owns the
//! Ethernet layer and the interface's addresses, and [`arp`], [`ip`],
//! [`icmp`], [`udp`], [`dhcp`], [`dns`], [`tcp`] and [`http`] sit on top.
//!
//! The whole stack is **polled**. [`poll`] pulls whatever the card has
//! received and runs it through the protocol handlers; the shell's main loop
//! calls it every iteration, and any operation that has to wait for a reply
//! calls [`wait_until`], which polls while it waits. Nothing here runs in
//! interrupt context, so handlers are free to allocate.

pub mod arp;
pub mod dhcp;
pub mod dns;
pub mod e1000;
pub mod http;
pub mod icmp;
pub mod ip;
pub mod selftest;
pub mod tcp;
pub mod tls;
pub mod udp;

use alloc::vec::Vec;
use spin::Mutex;

use e1000::E1000;
use ip::Ipv4Addr;

pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETH_HEADER_LEN: usize = 14;
pub const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

/// The PIT runs at about 18.2 Hz, which is the clock every timeout here is
/// expressed against.
pub const TICKS_PER_SEC: u64 = 18;

static NIC: Mutex<Option<E1000>> = Mutex::new(None);
static IFACE: Mutex<Interface> = Mutex::new(Interface::new());

/// Addresses and state for the single network interface.
pub struct Interface {
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub dns: Ipv4Addr,
    pub configured: bool,
}

impl Interface {
    const fn new() -> Self {
        Self {
            mac: [0; 6],
            ip: Ipv4Addr::UNSPECIFIED,
            netmask: Ipv4Addr::UNSPECIFIED,
            gateway: Ipv4Addr::UNSPECIFIED,
            dns: Ipv4Addr::UNSPECIFIED,
            configured: false,
        }
    }
}

/// Snapshot of the interface configuration.
pub fn config() -> (bool, [u8; 6], Ipv4Addr, Ipv4Addr, Ipv4Addr, Ipv4Addr) {
    let i = IFACE.lock();
    (i.configured, i.mac, i.ip, i.netmask, i.gateway, i.dns)
}

pub fn local_ip() -> Ipv4Addr {
    IFACE.lock().ip
}

pub fn is_up() -> bool {
    NIC.lock().is_some()
}

pub fn is_configured() -> bool {
    IFACE.lock().configured
}

pub fn set_config(ip: Ipv4Addr, netmask: Ipv4Addr, gateway: Ipv4Addr, dns: Ipv4Addr) {
    let mut i = IFACE.lock();
    i.ip = ip;
    i.netmask = netmask;
    i.gateway = gateway;
    i.dns = dns;
    i.configured = !ip.is_unspecified();
}

/// Bring up the card, if one is present.
///
/// A missing NIC is not an error: the OS boots fine without networking, and
/// the shell reports the situation when a network command is used.
pub fn init() -> Result<(), &'static str> {
    let nic = E1000::probe()?;
    let mac = nic.mac();
    *NIC.lock() = Some(nic);
    IFACE.lock().mac = mac;
    Ok(())
}

pub fn mac() -> [u8; 6] {
    IFACE.lock().mac
}

pub fn link_up() -> bool {
    NIC.lock().as_ref().map(|n| n.link_up()).unwrap_or(false)
}

/// (received, transmitted, dropped) frame counts.
pub fn stats() -> (u64, u64, u64) {
    NIC.lock()
        .as_ref()
        .map(|n| (n.rx_packets, n.tx_packets, n.rx_dropped))
        .unwrap_or((0, 0, 0))
}

/// Wrap a payload in an Ethernet header and hand it to the card.
pub fn send_frame(dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Result<(), &'static str> {
    let src = mac();
    let mut frame = Vec::with_capacity(ETH_HEADER_LEN + payload.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&src);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);

    // The wire has a 60-byte minimum (before the CRC the card appends).
    while frame.len() < 60 {
        frame.push(0);
    }

    let mut guard = NIC.lock();
    let nic = guard.as_mut().ok_or("no network interface")?;
    nic.send(&frame)
}

/// Drain the receive ring and dispatch everything in it.
pub fn poll() {
    let frames = {
        let mut guard = NIC.lock();
        match guard.as_mut() {
            Some(nic) => nic.receive(),
            None => return,
        }
    };
    // The NIC lock is released before dispatch: handlers reply, and
    // replying needs to take it again.
    for frame in frames {
        handle_frame(&frame);
    }
}

fn handle_frame(frame: &[u8]) {
    if frame.len() < ETH_HEADER_LEN {
        return;
    }
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let payload = &frame[ETH_HEADER_LEN..];
    let src_mac: [u8; 6] = frame[6..12].try_into().unwrap_or([0; 6]);

    match ethertype {
        ETHERTYPE_ARP => arp::handle(payload),
        ETHERTYPE_IPV4 => ip::handle(payload, src_mac),
        _ => {}
    }
}

/// Poll until `cond` holds or the deadline passes. Returns whether it held.
///
/// This is how every request/response exchange in the stack waits: there are
/// no threads to block, so the caller spins the receive path itself.
pub fn wait_until(mut cond: impl FnMut() -> bool, timeout_ticks: u64) -> bool {
    // `clock`, not `interrupts`: waits run inside actions dispatched from the
    // main loop, which processes events with interrupts disabled, so the
    // interrupt-driven tick count stands still for the whole wait and a
    // timeout written against it would never expire.
    let start = crate::clock::ticks();
    loop {
        poll();
        if cond() {
            return true;
        }
        if crate::clock::ticks().wrapping_sub(start) >= timeout_ticks {
            return cond();
        }
        core::hint::spin_loop();
    }
}

/// Format a MAC address as `aa:bb:cc:dd:ee:ff`.
pub fn format_mac(mac: [u8; 6]) -> alloc::string::String {
    alloc::format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Bring the interface up end to end: probe the card, then DHCP.
pub fn autoconfigure() -> Result<(), &'static str> {
    if !is_up() {
        init()?;
    }
    dhcp::configure()
}
