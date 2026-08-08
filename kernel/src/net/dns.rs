//! A DNS resolver: A records over UDP, with a small cache.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use spin::Mutex;

use super::ip::Ipv4Addr;
use super::{udp, TICKS_PER_SEC};

const DNS_PORT: u16 = 53;
const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const CLASS_IN: u16 = 1;

const MAX_CACHE: usize = 32;

static CACHE: Mutex<Vec<(String, Ipv4Addr)>> = Mutex::new(Vec::new());

pub fn cached() -> Vec<(String, Ipv4Addr)> {
    CACHE.lock().clone()
}

pub fn clear_cache() {
    CACHE.lock().clear();
}

fn remember(name: &str, addr: Ipv4Addr) {
    let mut cache = CACHE.lock();
    if cache.iter().any(|(n, _)| n == name) {
        return;
    }
    if cache.len() >= MAX_CACHE {
        cache.remove(0);
    }
    cache.push((name.to_string(), addr));
}

/// Encode a hostname as a sequence of length-prefixed labels.
pub(crate) fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), &'static str> {
    for label in name.split('.') {
        if label.is_empty() {
            continue;
        }
        if label.len() > 63 {
            return Err("hostname label too long");
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(())
}

/// Skip over a name in the answer section, which may be compressed.
///
/// Returns the offset just past the name as it appears at `pos`.
fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)?;
        if len & 0xC0 == 0xC0 {
            // A pointer is two bytes and always ends the name.
            return Some(pos + 2);
        }
        if len == 0 {
            return Some(pos + 1);
        }
        pos += 1 + len as usize;
        if pos > msg.len() {
            return None;
        }
    }
}

fn build_query(id: u16, name: &str) -> Result<Vec<u8>, &'static str> {
    let mut q = Vec::with_capacity(64);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // standard query, recursion desired
    q.extend_from_slice(&1u16.to_be_bytes()); // one question
    q.extend_from_slice(&0u16.to_be_bytes()); // no answers
    q.extend_from_slice(&0u16.to_be_bytes()); // no authority records
    q.extend_from_slice(&0u16.to_be_bytes()); // no additional records
    encode_name(name, &mut q)?;
    q.extend_from_slice(&TYPE_A.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(q)
}

/// Pull the first A record out of a response.
fn parse_response(msg: &[u8], id: u16) -> Option<Ipv4Addr> {
    if msg.len() < 12 {
        return None;
    }
    if u16::from_be_bytes([msg[0], msg[1]]) != id {
        return None;
    }
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return None; // not a response, or the server reported an error
    }

    let qd = u16::from_be_bytes([msg[4], msg[5]]) as usize;
    let an = u16::from_be_bytes([msg[6], msg[7]]) as usize;

    let mut pos = 12;
    for _ in 0..qd {
        pos = skip_name(msg, pos)?;
        pos += 4; // question type and class
    }

    for _ in 0..an {
        pos = skip_name(msg, pos)?;
        if pos + 10 > msg.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([msg[pos], msg[pos + 1]]);
        let rdlen = u16::from_be_bytes([msg[pos + 8], msg[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > msg.len() {
            return None;
        }
        if rtype == TYPE_A && rdlen == 4 {
            return Ipv4Addr::from_slice(&msg[pos..pos + 4]);
        }
        // A CNAME chain is followed implicitly: the server normally includes
        // the target's A record later in the same answer section.
        let _ = TYPE_CNAME;
        pos += rdlen;
    }
    None
}

/// Resolve a hostname, or pass a literal address straight through.
pub fn resolve(name: &str) -> Result<Ipv4Addr, &'static str> {
    if let Some(addr) = Ipv4Addr::parse(name) {
        return Ok(addr);
    }
    if let Some((_, addr)) = CACHE.lock().iter().find(|(n, _)| n == name) {
        return Ok(*addr);
    }

    let (configured, mac, _ip, _mask, _gw, server) = super::config();
    if !configured {
        return Err("the network is not configured (run `net up`)");
    }
    if server.is_unspecified() {
        return Err("no DNS server is configured");
    }

    // Source port is arbitrary but should vary per query.
    let id = (u16::from_be_bytes([mac[4], mac[5]]))
        ^ (crate::clock::ticks() as u16).rotate_left(7);
    let src_port = 40000 + (id % 20000);

    let query = build_query(id, name)?;
    udp::bind(src_port);

    let mut answer = None;
    'attempts: for _ in 0..3 {
        if udp::send(server, src_port, DNS_PORT, &query).is_err() {
            break;
        }
        let deadline = crate::clock::ticks() + 2 * TICKS_PER_SEC;
        while crate::clock::ticks() < deadline {
            super::poll();
            while let Some(dg) = udp::recv(src_port) {
                // Only trust an answer that came back from the resolver we
                // asked, on the port we asked it on. Together with the
                // random query ID this is what makes off-path spoofing hard.
                if dg.src != server || dg.src_port != DNS_PORT {
                    continue;
                }
                if let Some(addr) = parse_response(&dg.data, id) {
                    answer = Some(addr);
                    break 'attempts;
                }
            }
            core::hint::spin_loop();
        }
    }

    udp::unbind(src_port);

    match answer {
        Some(addr) => {
            remember(name, addr);
            Ok(addr)
        }
        None => Err("could not resolve the hostname"),
    }
}
