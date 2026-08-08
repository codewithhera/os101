//! DHCP client — the four-way DISCOVER / OFFER / REQUEST / ACK exchange.
//!
//! Enough to get an address, netmask, router and resolver from a server.
//! There is no lease renewal: leases outlast any session this OS is likely
//! to have, and a timer-driven renewal would need machinery the kernel does
//! not have yet.

use alloc::vec::Vec;

use super::ip::Ipv4Addr;
use super::{udp, TICKS_PER_SEC};

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;

const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;
const MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];

const MSG_DISCOVER: u8 = 1;
const MSG_OFFER: u8 = 2;
const MSG_REQUEST: u8 = 3;
const MSG_ACK: u8 = 5;
const MSG_NAK: u8 = 6;

const OPT_SUBNET_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_MSG_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAM_LIST: u8 = 55;
const OPT_END: u8 = 255;

struct Lease {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    dns: Ipv4Addr,
    server: Ipv4Addr,
}

/// Build a DHCP message. `xid` ties a reply back to our request.
fn build(msg_type: u8, xid: u32, mac: [u8; 6], requested: Option<Ipv4Addr>,
         server: Option<Ipv4Addr>) -> Vec<u8> {
    let mut p = Vec::with_capacity(300);
    p.push(OP_REQUEST);
    p.push(1); // Ethernet
    p.push(6); // MAC length
    p.push(0); // hops
    p.extend_from_slice(&xid.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes()); // seconds elapsed
    // Ask for broadcast replies: we have no address yet, so a unicast reply
    // would be addressed to something we cannot receive.
    p.extend_from_slice(&0x8000u16.to_be_bytes());
    p.extend_from_slice(&[0; 4]); // ciaddr
    p.extend_from_slice(&[0; 4]); // yiaddr
    p.extend_from_slice(&[0; 4]); // siaddr
    p.extend_from_slice(&[0; 4]); // giaddr
    p.extend_from_slice(&mac);
    p.extend_from_slice(&[0; 10]); // chaddr padding
    p.extend_from_slice(&[0; 64]); // sname
    p.extend_from_slice(&[0; 128]); // file
    p.extend_from_slice(&MAGIC_COOKIE);

    p.push(OPT_MSG_TYPE);
    p.push(1);
    p.push(msg_type);

    if let Some(ip) = requested {
        p.push(OPT_REQUESTED_IP);
        p.push(4);
        p.extend_from_slice(&ip.octets());
    }
    if let Some(ip) = server {
        p.push(OPT_SERVER_ID);
        p.push(4);
        p.extend_from_slice(&ip.octets());
    }

    p.push(OPT_PARAM_LIST);
    p.push(3);
    p.push(OPT_SUBNET_MASK);
    p.push(OPT_ROUTER);
    p.push(OPT_DNS);

    p.push(OPT_END);
    // Some servers ignore very short messages; pad to the classic minimum.
    while p.len() < 300 {
        p.push(0);
    }
    p
}

/// Pull the fields we care about out of a reply.
fn parse(msg: &[u8], xid: u32, mac: [u8; 6]) -> Option<(u8, Lease)> {
    if msg.len() < 240 || msg[0] != OP_REPLY {
        return None;
    }
    if u32::from_be_bytes(msg[4..8].try_into().ok()?) != xid {
        return None;
    }
    if msg[28..34] != mac {
        return None;
    }
    if msg[236..240] != MAGIC_COOKIE {
        return None;
    }

    let mut msg_type = 0u8;
    let mut lease = Lease {
        ip: Ipv4Addr::from_slice(&msg[16..20])?,
        netmask: Ipv4Addr::new(255, 255, 255, 0),
        gateway: Ipv4Addr::UNSPECIFIED,
        dns: Ipv4Addr::UNSPECIFIED,
        server: Ipv4Addr::UNSPECIFIED,
    };

    let mut i = 240;
    while i < msg.len() {
        let code = msg[i];
        if code == OPT_END {
            break;
        }
        if code == 0 {
            i += 1; // pad
            continue;
        }
        if i + 1 >= msg.len() {
            break;
        }
        let len = msg[i + 1] as usize;
        let value = msg.get(i + 2..i + 2 + len)?;
        match code {
            OPT_MSG_TYPE if len >= 1 => msg_type = value[0],
            OPT_SUBNET_MASK if len >= 4 => lease.netmask = Ipv4Addr::from_slice(value)?,
            OPT_ROUTER if len >= 4 => lease.gateway = Ipv4Addr::from_slice(value)?,
            OPT_DNS if len >= 4 => lease.dns = Ipv4Addr::from_slice(value)?,
            OPT_SERVER_ID if len >= 4 => lease.server = Ipv4Addr::from_slice(value)?,
            _ => {}
        }
        i += 2 + len;
    }

    Some((msg_type, lease))
}

/// Run the exchange and apply the result to the interface.
pub fn configure() -> Result<(), &'static str> {
    let mac = super::mac();
    // No clock to seed from, so mix the MAC with the tick counter.
    let xid = u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]])
        ^ (crate::clock::ticks() as u32).rotate_left(11);

    udp::bind(CLIENT_PORT);
    // Sending starts from 0.0.0.0, which `ip::send` allows for UDP.
    super::set_config(Ipv4Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED,
                      Ipv4Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED);

    let result = (|| {
        let offer = exchange(MSG_DISCOVER, MSG_OFFER, xid, mac, None, None)?;
        let ack = exchange(
            MSG_REQUEST,
            MSG_ACK,
            xid,
            mac,
            Some(offer.ip),
            if offer.server.is_unspecified() { None } else { Some(offer.server) },
        )?;
        Ok(ack)
    })();

    udp::unbind(CLIENT_PORT);

    let lease = result?;
    super::set_config(lease.ip, lease.netmask, lease.gateway, lease.dns);
    Ok(())
}

/// Send one message and wait for the expected reply, retrying a few times.
fn exchange(send_type: u8, expect: u8, xid: u32, mac: [u8; 6],
            requested: Option<Ipv4Addr>, server: Option<Ipv4Addr>) -> Result<Lease, &'static str> {
    let msg = build(send_type, xid, mac, requested, server);

    for _ in 0..4 {
        udp::send(Ipv4Addr::BROADCAST, CLIENT_PORT, SERVER_PORT, &msg)
            .map_err(|_| "could not send the DHCP request")?;

        let deadline = crate::clock::ticks() + 2 * TICKS_PER_SEC;
        while crate::clock::ticks() < deadline {
            super::poll();
            while let Some(dg) = udp::recv(CLIENT_PORT) {
                if let Some((kind, lease)) = parse(&dg.data, xid, mac) {
                    if kind == expect {
                        return Ok(lease);
                    }
                    if kind == MSG_NAK {
                        return Err("the DHCP server refused the request");
                    }
                }
            }
            core::hint::spin_loop();
        }
    }

    Err(if send_type == MSG_DISCOVER {
        "no DHCP server answered"
    } else {
        "the DHCP server did not acknowledge the lease"
    })
}
