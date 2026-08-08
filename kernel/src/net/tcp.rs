//! A minimal TCP client.
//!
//! Enough of the protocol to open a connection, send a request and read the
//! response — which is what an HTTP client needs. Deliberately not
//! implemented: retransmission of data (only the SYN is retried), congestion
//! control, out-of-order reassembly, and simultaneous connections. Segments
//! that arrive out of order are dropped and the peer retransmits them, which
//! is correct if inefficient.
//!
//! One connection exists at a time, held in [`CONN`].

use alloc::vec::Vec;
use spin::Mutex;

use super::ip::{self, Ipv4Addr};
use super::TICKS_PER_SEC;

pub const FIN: u8 = 1 << 0;
pub const SYN: u8 = 1 << 1;
pub const RST: u8 = 1 << 2;
pub const PSH: u8 = 1 << 3;
pub const ACK: u8 = 1 << 4;

const HEADER_LEN: usize = 20;
/// Advertised receive window, and the cap on how much we will buffer.
const WINDOW: u16 = 32768;
/// Has to stay above `http::MAX_BODY` plus the headers, or a large image
/// would be truncated before the HTTP layer ever saw all of it.
const MAX_RX_BYTES: usize = 1152 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    SynSent,
    Established,
    /// We have sent a FIN and are waiting for the peer to finish.
    FinWait,
}

pub struct Connection {
    pub state: State,
    pub local_port: u16,
    pub remote: Ipv4Addr,
    pub remote_port: u16,
    /// Next sequence number we will send.
    snd_nxt: u32,
    /// Oldest sequence number the peer has not acknowledged.
    snd_una: u32,
    /// Next sequence number we expect to receive.
    rcv_nxt: u32,
    rx: Vec<u8>,
    /// Set once the peer sends FIN.
    pub remote_closed: bool,
    /// Set if the peer sends RST.
    pub reset: bool,
}

static CONN: Mutex<Option<Connection>> = Mutex::new(None);

/// True when `a` is at or before `b` in sequence space, accounting for wrap.
pub(crate) fn seq_le(a: u32, b: u32) -> bool {
    (b.wrapping_sub(a) as i32) >= 0
}

fn build_segment(
    src_port: u16, dst_port: u16, seq: u32, ack: u32, flags: u8, payload: &[u8],
    src: Ipv4Addr, dst: Ipv4Addr,
) -> Vec<u8> {
    let mut seg = Vec::with_capacity(HEADER_LEN + payload.len());
    seg.extend_from_slice(&src_port.to_be_bytes());
    seg.extend_from_slice(&dst_port.to_be_bytes());
    seg.extend_from_slice(&seq.to_be_bytes());
    seg.extend_from_slice(&ack.to_be_bytes());
    seg.push(5 << 4); // 5 dwords of header, no options
    seg.push(flags);
    seg.extend_from_slice(&WINDOW.to_be_bytes());
    seg.extend_from_slice(&[0, 0]); // checksum placeholder
    seg.extend_from_slice(&[0, 0]); // urgent pointer
    seg.extend_from_slice(payload);

    let sum = ip::transport_checksum(src, dst, ip::PROTO_TCP, &seg);
    seg[16..18].copy_from_slice(&sum.to_be_bytes());
    seg
}

/// Send one segment on the current connection's four-tuple.
fn emit(conn: &Connection, seq: u32, ack: u32, flags: u8, payload: &[u8]) -> Result<(), &'static str> {
    let src = super::local_ip();
    let seg = build_segment(
        conn.local_port, conn.remote_port, seq, ack, flags, payload, src, conn.remote,
    );
    ip::send(conn.remote, ip::PROTO_TCP, &seg)
}

/// Open a connection. Any previous one is abandoned.
pub fn connect(remote: Ipv4Addr, remote_port: u16) -> Result<(), &'static str> {
    if !super::is_configured() {
        return Err("the network is not configured (run `net up`)");
    }

    // Ephemeral port, varied per connection so a quick reconnect to the same
    // server is not confused with the previous one. This has to come from
    // `clock`: back-to-back connections run inside one main-loop pass, with
    // interrupts off, so the interrupt-driven count would hand every one of
    // them the same port and the same initial sequence number.
    let ticks = crate::clock::ticks();
    let local_port = 49152 + (ticks as u16 % 16000);
    let isn = (ticks as u32).wrapping_mul(2654435761);

    {
        let mut guard = CONN.lock();
        *guard = Some(Connection {
            state: State::SynSent,
            local_port,
            remote,
            remote_port,
            snd_nxt: isn.wrapping_add(1),
            snd_una: isn,
            rcv_nxt: 0,
            rx: Vec::new(),
            remote_closed: false,
            reset: false,
        });
    }

    // Retry the SYN: the first one is often lost while ARP resolves.
    for _ in 0..4 {
        {
            let guard = CONN.lock();
            let Some(conn) = guard.as_ref() else { return Err("connection went away") };
            if conn.state != State::SynSent {
                break;
            }
            let seq = conn.snd_una;
            let c = Connection {
                state: conn.state, local_port: conn.local_port, remote: conn.remote,
                remote_port: conn.remote_port, snd_nxt: conn.snd_nxt, snd_una: conn.snd_una,
                rcv_nxt: conn.rcv_nxt, rx: Vec::new(), remote_closed: false, reset: false,
            };
            drop(guard);
            emit(&c, seq, 0, SYN, &[])?;
        }

        let connected = super::wait_until(
            || {
                let g = CONN.lock();
                match g.as_ref() {
                    Some(c) => c.state == State::Established || c.reset,
                    None => true,
                }
            },
            2 * TICKS_PER_SEC,
        );

        if connected {
            let g = CONN.lock();
            return match g.as_ref() {
                Some(c) if c.reset => Err("the connection was refused"),
                Some(c) if c.state == State::Established => Ok(()),
                _ => Err("the connection failed"),
            };
        }
    }

    *CONN.lock() = None;
    Err("the connection timed out")
}

/// Send `data` and wait for it to be acknowledged.
///
/// Stop-and-wait: fine for an HTTP request, which is one small segment.
pub fn send(data: &[u8]) -> Result<(), &'static str> {
    // Keep each segment inside a comfortable MSS for Ethernet.
    const MSS: usize = 1400;

    for chunk in data.chunks(MSS) {
        let (snapshot, seq, ack) = {
            let guard = CONN.lock();
            let conn = guard.as_ref().ok_or("not connected")?;
            if conn.state != State::Established {
                return Err("not connected");
            }
            (
                Connection {
                    state: conn.state, local_port: conn.local_port, remote: conn.remote,
                    remote_port: conn.remote_port, snd_nxt: conn.snd_nxt, snd_una: conn.snd_una,
                    rcv_nxt: conn.rcv_nxt, rx: Vec::new(), remote_closed: false, reset: false,
                },
                conn.snd_nxt,
                conn.rcv_nxt,
            )
        };

        emit(&snapshot, seq, ack, PSH | ACK, chunk)?;

        let expected = seq.wrapping_add(chunk.len() as u32);
        {
            let mut guard = CONN.lock();
            if let Some(conn) = guard.as_mut() {
                conn.snd_nxt = expected;
            }
        }

        let acked = super::wait_until(
            || {
                let g = CONN.lock();
                match g.as_ref() {
                    Some(c) => seq_le(expected, c.snd_una) || c.reset || c.remote_closed,
                    None => true,
                }
            },
            5 * TICKS_PER_SEC,
        );
        if !acked {
            return Err("the peer did not acknowledge the request");
        }
    }
    Ok(())
}

/// Read until the peer closes, no data arrives for a while, or the cap is hit.
pub fn recv_to_end(timeout_ticks: u64) -> Result<Vec<u8>, &'static str> {
    let start = crate::clock::ticks();
    let mut last_len = 0usize;
    let mut last_change = start;

    loop {
        super::poll();

        let (len, done, reset) = {
            let g = CONN.lock();
            match g.as_ref() {
                Some(c) => (c.rx.len(), c.remote_closed, c.reset),
                None => (0, true, false),
            }
        };

        if len != last_len {
            last_len = len;
            last_change = crate::clock::ticks();
        }
        if done || reset {
            break;
        }

        let now = crate::clock::ticks();
        if now.wrapping_sub(start) >= timeout_ticks {
            break;
        }
        // Give up early once the stream has been quiet for a while but only
        // after something has actually arrived.
        if last_len > 0 && now.wrapping_sub(last_change) >= 3 * TICKS_PER_SEC {
            break;
        }
        core::hint::spin_loop();
    }

    let mut guard = CONN.lock();
    match guard.as_mut() {
        Some(conn) => {
            if conn.rx.is_empty() && conn.reset {
                return Err("the connection was reset");
            }
            Ok(core::mem::take(&mut conn.rx))
        }
        None => Err("the connection was closed"),
    }
}

/// Take whatever has arrived, waiting up to `timeout_ticks` for the first byte.
///
/// [`recv_to_end`] cannot serve TLS: a record has to be read, decrypted and
/// acted on before the next one is asked for, and the peer will not close the
/// connection to tell us a record has ended. This returns as soon as anything
/// is available and leaves the connection open.
///
/// An empty result means the timeout passed with nothing to read, which is not
/// an error — a caller waiting for a specific number of bytes simply asks
/// again. The peer closing *is* reported, so a caller cannot loop forever on a
/// connection that has gone away.
pub fn recv_some(timeout_ticks: u64) -> Result<Vec<u8>, &'static str> {
    let start = crate::clock::ticks();
    loop {
        super::poll();

        {
            let mut guard = CONN.lock();
            let Some(conn) = guard.as_mut() else {
                return Err("the connection was closed");
            };
            if !conn.rx.is_empty() {
                return Ok(core::mem::take(&mut conn.rx));
            }
            if conn.reset {
                return Err("the connection was reset");
            }
            if conn.remote_closed {
                return Err("the peer closed the connection");
            }
        }

        if crate::clock::ticks().wrapping_sub(start) >= timeout_ticks {
            return Ok(Vec::new());
        }
        core::hint::spin_loop();
    }
}

/// Send a FIN and forget the connection.
pub fn close() {
    let snapshot = {
        let mut guard = CONN.lock();
        match guard.as_mut() {
            Some(conn) if conn.state == State::Established => {
                conn.state = State::FinWait;
                Some((
                    Connection {
                        state: conn.state, local_port: conn.local_port, remote: conn.remote,
                        remote_port: conn.remote_port, snd_nxt: conn.snd_nxt,
                        snd_una: conn.snd_una, rcv_nxt: conn.rcv_nxt, rx: Vec::new(),
                        remote_closed: false, reset: false,
                    },
                    conn.snd_nxt,
                    conn.rcv_nxt,
                ))
            }
            _ => None,
        }
    };

    if let Some((conn, seq, ack)) = snapshot {
        let _ = emit(&conn, seq, ack, FIN | ACK, &[]);
    }
    *CONN.lock() = None;
}

/// Process an inbound TCP segment.
pub fn handle(src: Ipv4Addr, segment: &[u8]) {
    if segment.len() < HEADER_LEN {
        return;
    }
    let src_port = u16::from_be_bytes([segment[0], segment[1]]);
    let dst_port = u16::from_be_bytes([segment[2], segment[3]]);
    let seq = u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]);
    let ack_no = u32::from_be_bytes([segment[8], segment[9], segment[10], segment[11]]);
    let data_offset = ((segment[12] >> 4) as usize) * 4;
    let flags = segment[13];

    if data_offset < HEADER_LEN || data_offset > segment.len() {
        return;
    }
    let payload = &segment[data_offset..];

    // Everything that needs sending is collected here, then emitted after
    // the connection lock is released — `ip::send` must not be called with
    // it held.
    let mut reply: Option<(Connection, u32, u32, u8)> = None;

    {
        let mut guard = CONN.lock();
        let Some(conn) = guard.as_mut() else { return };
        if conn.remote != src || conn.remote_port != src_port || conn.local_port != dst_port {
            return;
        }

        if flags & RST != 0 {
            conn.reset = true;
            conn.state = State::Closed;
            return;
        }

        if flags & ACK != 0 && seq_le(conn.snd_una, ack_no) {
            conn.snd_una = ack_no;
        }

        match conn.state {
            State::SynSent => {
                if flags & SYN != 0 && flags & ACK != 0 {
                    conn.rcv_nxt = seq.wrapping_add(1);
                    conn.state = State::Established;
                    reply = Some((snapshot(conn), conn.snd_nxt, conn.rcv_nxt, ACK));
                }
            }
            State::Established | State::FinWait => {
                let mut should_ack = false;

                if !payload.is_empty() {
                    if seq == conn.rcv_nxt {
                        let room = MAX_RX_BYTES.saturating_sub(conn.rx.len());
                        let take = payload.len().min(room);
                        conn.rx.extend_from_slice(&payload[..take]);
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(take as u32);
                    }
                    // Out-of-order data is dropped, but still acknowledged at
                    // the last in-order point so the peer retransmits.
                    should_ack = true;
                }

                if flags & FIN != 0 && seq.wrapping_add(payload.len() as u32) == conn.rcv_nxt {
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.remote_closed = true;
                    should_ack = true;
                }

                if should_ack {
                    reply = Some((snapshot(conn), conn.snd_nxt, conn.rcv_nxt, ACK));
                }
            }
            State::Closed => {}
        }
    }

    if let Some((conn, seq, ack, flags)) = reply {
        let _ = emit(&conn, seq, ack, flags, &[]);
    }
}

/// Copy the addressing fields needed to emit a segment, without the buffers.
fn snapshot(conn: &Connection) -> Connection {
    Connection {
        state: conn.state,
        local_port: conn.local_port,
        remote: conn.remote,
        remote_port: conn.remote_port,
        snd_nxt: conn.snd_nxt,
        snd_una: conn.snd_una,
        rcv_nxt: conn.rcv_nxt,
        rx: Vec::new(),
        remote_closed: false,
        reset: false,
    }
}
