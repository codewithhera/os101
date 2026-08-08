//! A TLS 1.3 client.
//!
//! Enough of RFC 8446 to fetch a page from a modern web server, which by 2026
//! means every interesting server there is. Without this, OS101's browser
//! could only reach the shrinking set of sites still answering on port 80, or
//! go through a plaintext gateway that reads everything on the way past.
//!
//! # What is implemented
//!
//! One cipher suite, `TLS_CHACHA20_POLY1305_SHA256`, over one key exchange
//! group, X25519. That is deliberately the narrowest configuration that real
//! servers accept: TLS 1.3 removed the negotiation sprawl that made earlier
//! versions so much work, and a client that offers exactly one of everything
//! still interoperates, because every TLS 1.3 server must support this suite's
//! neighbours and nearly all support this one.
//!
//! The handshake is the ordinary 1-RTT one: ClientHello, ServerHello, then the
//! server's encrypted flight, then our Finished. The server's Finished is
//! verified, which proves whoever we are talking to derived the same
//! handshake secret from the same transcript — so nobody rewrote the
//! handshake in flight.
//!
//! # What is not
//!
//! **Certificates are not verified.** The server's certificate chain is
//! parsed only far enough to be skipped. OS101 has no root store, no RSA or
//! ECDSA verification, and no clock accurate enough to check an expiry date.
//!
//! The practical consequence, stated plainly: this protects against a passive
//! eavesdropper, and does not protect against an active
//! machine-in-the-middle. Someone who can only watch the traffic learns
//! nothing. Someone who can *redirect* it — the operator of the network you
//! are on — can present any certificate they like and we will accept it. That
//! is strictly better than the plaintext gateway this replaces, and strictly
//! worse than a real browser. Do not type a password into OS101.
//!
//! Also absent, none of which a fetch-a-page client needs: session resumption
//! and 0-RTT, client certificates, renegotiation, post-handshake key updates
//! (a `KeyUpdate` is reported as an error rather than mishandled), and
//! HelloRetryRequest — if a server will not use X25519 we say so and give up.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::ip::Ipv4Addr;
use super::{tcp, TICKS_PER_SEC};
use crate::crypto::{chacha20poly1305 as aead, hkdf, hmac, random, sha256, x25519};

const CT_CHANGE_CIPHER_SPEC: u8 = 20;
const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
const CT_APPLICATION_DATA: u8 = 23;

const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_NEW_SESSION_TICKET: u8 = 4;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;
const HS_KEY_UPDATE: u8 = 24;

const TLS_CHACHA20_POLY1305_SHA256: u16 = 0x1303;
const GROUP_X25519: u16 = 0x001D;
const VERSION_TLS12: u16 = 0x0303;
const VERSION_TLS13: u16 = 0x0304;

/// A record's payload may be 2^14 bytes plus AEAD expansion. Anything larger
/// is a protocol violation, and refusing it bounds what one record can make us
/// allocate.
const MAX_RECORD_PAYLOAD: usize = 16384 + 256;
/// The most plaintext we put in one outgoing record, per RFC 8446.
const MAX_FRAGMENT: usize = 16384;

/// A ServerHello carrying this in its random field is really a
/// HelloRetryRequest — TLS 1.3 disguises it as a ServerHello so that middle
/// boxes cannot tell the difference. RFC 8446 §4.1.3.
const HELLO_RETRY_REQUEST_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// How long a handshake may take end to end.
const HANDSHAKE_TIMEOUT: u64 = 20 * TICKS_PER_SEC;

// ---------------------------------------------------------------------------
// Key schedule
// ---------------------------------------------------------------------------

/// The `HkdfLabel` structure of RFC 8446 §7.1, and the expansion it names.
///
/// Every secret in TLS 1.3 comes from this one function, distinguished only by
/// the label — which is what stops a key derived for one purpose being valid
/// for another.
fn expand_label(secret: &[u8; 32], label: &str, context: &[u8], out: &mut [u8]) {
    let mut info = Vec::with_capacity(2 + 1 + 6 + label.len() + 1 + context.len());
    info.extend_from_slice(&(out.len() as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label.as_bytes());
    info.push(context.len() as u8);
    info.extend_from_slice(context);

    // The only way this fails is a request longer than 255 hash lengths, and
    // every call here asks for 32 bytes or fewer.
    let _ = hkdf::expand(secret, &info, out);
}

/// `Derive-Secret(secret, label, messages)` — an expansion whose context is
/// the hash of the handshake so far, binding the new secret to the transcript.
fn derive_secret(secret: &[u8; 32], label: &str, transcript: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    expand_label(secret, label, transcript, &mut out);
    out
}

/// The traffic keys for one direction, and the record counter they are used
/// with.
struct Keys {
    key: [u8; 32],
    iv: [u8; 12],
    /// Records sent or received under these keys. Never reset except when the
    /// keys change, because the nonce is derived from it and a repeat would
    /// destroy the AEAD's security outright.
    seq: u64,
}

impl Keys {
    /// Derive the write keys a traffic secret implies.
    fn from_secret(secret: &[u8; 32]) -> Keys {
        let mut key = [0u8; 32];
        let mut iv = [0u8; 12];
        expand_label(secret, "key", &[], &mut key);
        expand_label(secret, "iv", &[], &mut iv);
        Keys { key, iv, seq: 0 }
    }

    /// RFC 8446 §5.3: the sequence number, left-padded to the IV's length, is
    /// XORed into the IV. No sequence number is ever reused with one key.
    fn nonce(&self) -> [u8; 12] {
        let mut nonce = self.iv;
        let counter = self.seq.to_be_bytes();
        for i in 0..8 {
            nonce[4 + i] ^= counter[i];
        }
        nonce
    }
}

/// Everything derived from the shared secret, in the order RFC 8446 §7.1
/// derives it.
struct Schedule {
    handshake_secret: [u8; 32],
    client_handshake: [u8; 32],
    server_handshake: [u8; 32],
}

impl Schedule {
    fn new(shared: &[u8; 32], transcript: &[u8; 32]) -> Schedule {
        let zeros = [0u8; 32];
        let early = hkdf::extract(&[], &zeros);
        let derived = derive_secret(&early, "derived", &sha256::digest(&[]));
        let handshake_secret = hkdf::extract(&derived, shared);

        Schedule {
            client_handshake: derive_secret(&handshake_secret, "c hs traffic", transcript),
            server_handshake: derive_secret(&handshake_secret, "s hs traffic", transcript),
            handshake_secret,
        }
    }

    /// The application traffic secrets, which are bound to the transcript
    /// through the *server's* Finished — not the client's.
    fn application(&self, transcript: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
        let zeros = [0u8; 32];
        let derived = derive_secret(&self.handshake_secret, "derived", &sha256::digest(&[]));
        let master = hkdf::extract(&derived, &zeros);
        (
            derive_secret(&master, "c ap traffic", transcript),
            derive_secret(&master, "s ap traffic", transcript),
        )
    }
}

/// The `verify_data` a Finished message must carry, per RFC 8446 §4.4.4.
fn finished_mac(traffic_secret: &[u8; 32], transcript: &[u8; 32]) -> [u8; 32] {
    let mut finished_key = [0u8; 32];
    expand_label(traffic_secret, "finished", &[], &mut finished_key);
    hmac::hmac_sha256(&finished_key, transcript)
}

// ---------------------------------------------------------------------------
// Reading records off the wire
// ---------------------------------------------------------------------------

/// Buffers raw TCP bytes and hands back whole TLS records.
///
/// Record boundaries have nothing to do with TCP segment boundaries: one
/// record can arrive in six segments, and six records can arrive in one.
struct Reader {
    raw: Vec<u8>,
    /// Set once TCP itself has ended, so a caller waiting for more data can be
    /// told there will not be any rather than waiting out the deadline.
    ended: bool,
}

impl Reader {
    fn new() -> Reader {
        Reader { raw: Vec::new(), ended: false }
    }

    /// Wait until at least `want` bytes are buffered.
    fn fill(&mut self, want: usize, deadline: u64) -> Result<(), String> {
        while self.raw.len() < want {
            if self.ended {
                return Err("the connection ended mid-record".to_string());
            }
            if crate::clock::ticks() >= deadline {
                return Err("timed out waiting for the server".to_string());
            }
            match tcp::recv_some(TICKS_PER_SEC) {
                Ok(chunk) => self.raw.extend_from_slice(&chunk),
                Err(_) => {
                    self.ended = true;
                }
            }
        }
        Ok(())
    }

}

// ---------------------------------------------------------------------------
// Writing records
// ---------------------------------------------------------------------------

/// Wrap plaintext in an encrypted record and send it.
///
/// TLS 1.3 hides the real content type inside the encrypted payload and labels
/// every record `application_data` on the outside, so an observer cannot tell
/// handshake traffic from data.
fn send_encrypted(keys: &mut Keys, content_type: u8, plaintext: &[u8]) -> Result<(), String> {
    let mut inner = Vec::with_capacity(plaintext.len() + 1 + 16);
    inner.extend_from_slice(plaintext);
    inner.push(content_type);

    let length = (inner.len() + 16) as u16;
    let header = [
        CT_APPLICATION_DATA,
        (VERSION_TLS12 >> 8) as u8,
        VERSION_TLS12 as u8,
        (length >> 8) as u8,
        length as u8,
    ];

    aead::seal(&keys.key, &keys.nonce(), &header, &mut inner);
    keys.seq += 1;

    let mut record = Vec::with_capacity(5 + inner.len());
    record.extend_from_slice(&header);
    record.extend_from_slice(&inner);
    tcp::send(&record).map_err(|e| e.to_string())
}

/// Send a record with no encryption, for the ClientHello and the compatibility
/// change_cipher_spec.
fn send_plain(content_type: u8, version: u16, payload: &[u8]) -> Result<(), String> {
    let mut record = Vec::with_capacity(5 + payload.len());
    record.push(content_type);
    record.extend_from_slice(&version.to_be_bytes());
    record.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    record.extend_from_slice(payload);
    tcp::send(&record).map_err(|e| e.to_string())
}

/// Undo [`send_encrypted`]: authenticate, decrypt, and recover the real
/// content type from the end of the plaintext.
fn open_record(keys: &mut Keys, header: &[u8; 5], payload: Vec<u8>) -> Result<(u8, Vec<u8>), String> {
    let mut buffer = payload;
    aead::open(&keys.key, &keys.nonce(), header, &mut buffer)
        .map_err(|_| "could not decrypt a record from the server".to_string())?;
    keys.seq += 1;

    // The content type is the last non-zero byte; anything after it is
    // padding, which exists to disguise message lengths.
    while let Some(&0) = buffer.last() {
        buffer.pop();
    }
    let content_type = buffer.pop().ok_or_else(|| "an empty record arrived".to_string())?;
    Ok((content_type, buffer))
}

// ---------------------------------------------------------------------------
// Building the ClientHello
// ---------------------------------------------------------------------------

/// Append a length-prefixed block, with the length written afterwards once its
/// size is known.
///
/// TLS is built almost entirely from nested length-prefixed vectors, and
/// writing the lengths by hand is where hand-rolled encoders go wrong. This
/// records where the length belongs, runs the body, then backfills.
fn with_length<F: FnOnce(&mut Vec<u8>)>(out: &mut Vec<u8>, width: usize, body: F) {
    let position = out.len();
    for _ in 0..width {
        out.push(0);
    }
    let start = out.len();
    body(out);
    let length = out.len() - start;
    for i in 0..width {
        out[position + i] = (length >> (8 * (width - 1 - i))) as u8;
    }
}

fn extension(out: &mut Vec<u8>, kind: u16, body: impl FnOnce(&mut Vec<u8>)) {
    out.extend_from_slice(&kind.to_be_bytes());
    with_length(out, 2, body);
}

fn client_hello(hostname: &str, public_key: &[u8; 32], random: &[u8; 32], session_id: &[u8; 32]) -> Vec<u8> {
    let mut body = Vec::new();
    // A TLS 1.3 ClientHello still claims to be TLS 1.2 here and puts the real
    // version in an extension, so that servers and middleboxes written before
    // 1.3 existed do not choke on it.
    body.extend_from_slice(&VERSION_TLS12.to_be_bytes());
    body.extend_from_slice(random);

    // A non-empty session id puts the handshake in "compatibility mode",
    // where it looks enough like a resumed TLS 1.2 handshake to pass through
    // middleboxes that would otherwise drop it.
    with_length(&mut body, 1, |out| out.extend_from_slice(session_id));

    with_length(&mut body, 2, |out| {
        out.extend_from_slice(&TLS_CHACHA20_POLY1305_SHA256.to_be_bytes())
    });
    with_length(&mut body, 1, |out| out.push(0)); // no compression

    with_length(&mut body, 2, |out| {
        // server_name: which site we want, since one address serves many.
        extension(out, 0, |ext| {
            with_length(ext, 2, |list| {
                list.push(0); // host_name
                with_length(list, 2, |name| name.extend_from_slice(hostname.as_bytes()));
            })
        });

        // supported_versions: the extension that actually selects TLS 1.3.
        extension(out, 43, |ext| {
            with_length(ext, 1, |list| list.extend_from_slice(&VERSION_TLS13.to_be_bytes()))
        });

        // supported_groups
        extension(out, 10, |ext| {
            with_length(ext, 2, |list| list.extend_from_slice(&GROUP_X25519.to_be_bytes()))
        });

        // signature_algorithms. We never check a signature, but the extension
        // is mandatory for a certificate-authenticated handshake and a server
        // will abort without it.
        extension(out, 13, |ext| {
            with_length(ext, 2, |list| {
                for scheme in [0x0403u16, 0x0804, 0x0401, 0x0503, 0x0805, 0x0501] {
                    list.extend_from_slice(&scheme.to_be_bytes());
                }
            })
        });

        // key_share: our X25519 public key, offered up front so the server can
        // reply with everything encrypted and the handshake costs one round
        // trip instead of two.
        extension(out, 51, |ext| {
            with_length(ext, 2, |shares| {
                shares.extend_from_slice(&GROUP_X25519.to_be_bytes());
                with_length(shares, 2, |key| key.extend_from_slice(public_key));
            })
        });

        // application_layer_protocol_negotiation. Without this a modern server
        // may well choose HTTP/2, which this OS cannot speak.
        extension(out, 16, |ext| {
            with_length(ext, 2, |list| {
                with_length(list, 1, |name| name.extend_from_slice(b"http/1.1"))
            })
        });
    });

    let mut message = Vec::with_capacity(body.len() + 4);
    message.push(HS_CLIENT_HELLO);
    with_length(&mut message, 3, |out| out.extend_from_slice(&body));
    message
}

// ---------------------------------------------------------------------------
// Parsing what comes back
// ---------------------------------------------------------------------------

/// A cursor that cannot read off the end.
///
/// Everything parsed here came from the network, so every read is checked and
/// returns an error rather than panicking. A kernel that panics on a malformed
/// packet is a kernel anyone can halt.
struct Cursor<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Cursor<'a> {
        Cursor { data, at: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.at.checked_add(n).map_or(true, |end| end > self.data.len()) {
            return Err("a message from the server was truncated".to_string());
        }
        let slice = &self.data[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// A vector prefixed with an `n`-byte length.
    fn vector(&mut self, width: usize) -> Result<&'a [u8], String> {
        let mut length = 0usize;
        for _ in 0..width {
            length = (length << 8) | self.u8()? as usize;
        }
        self.take(length)
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.at
    }
}

/// What a ServerHello tells us that we need.
struct ServerHello {
    key_share: [u8; 32],
}

fn parse_server_hello(body: &[u8]) -> Result<ServerHello, String> {
    let mut cursor = Cursor::new(body);
    cursor.u16()?; // legacy_version
    let random = cursor.take(32)?;
    if random == HELLO_RETRY_REQUEST_RANDOM {
        return Err(
            "the server asked for a different key exchange group; OS101 only offers X25519"
                .to_string(),
        );
    }
    cursor.vector(1)?; // echoed session id

    let suite = cursor.u16()?;
    if suite != TLS_CHACHA20_POLY1305_SHA256 {
        return Err(alloc::format!(
            "the server chose cipher suite {suite:#06x}, which OS101 does not implement"
        ));
    }
    cursor.u8()?; // compression

    let extensions = cursor.vector(2)?;
    let mut ext = Cursor::new(extensions);
    let mut key_share = None;
    let mut version_ok = false;

    while ext.remaining() > 0 {
        let kind = ext.u16()?;
        let body = ext.vector(2)?;
        match kind {
            43 => {
                let mut inner = Cursor::new(body);
                version_ok = inner.u16()? == VERSION_TLS13;
            }
            51 => {
                let mut inner = Cursor::new(body);
                let group = inner.u16()?;
                let key = inner.vector(2)?;
                if group != GROUP_X25519 {
                    return Err("the server replied with the wrong key exchange group".to_string());
                }
                if key.len() != 32 {
                    return Err("the server's key share is the wrong size".to_string());
                }
                let mut bytes = [0u8; 32];
                bytes.copy_from_slice(key);
                key_share = Some(bytes);
            }
            _ => {}
        }
    }

    if !version_ok {
        return Err("the server does not speak TLS 1.3, which is all OS101 supports".to_string());
    }
    match key_share {
        Some(key_share) => Ok(ServerHello { key_share }),
        None => Err("the server did not send a key share".to_string()),
    }
}

/// Turn an alert record into something worth reading.
fn describe_alert(payload: &[u8]) -> String {
    if payload.len() < 2 {
        return "the server sent a malformed alert".to_string();
    }
    let description = match payload[1] {
        0 => "close notify",
        40 => "handshake failure",
        42 => "bad certificate",
        47 => "illegal parameter",
        48 => "unknown certificate authority",
        50 => "decode error",
        51 => "decrypt error",
        70 => "protocol version not supported",
        71 => "insufficient security",
        80 => "internal error",
        109 => "missing extension",
        112 => "unrecognised name (the server does not serve this host)",
        120 => "no application protocol in common",
        other => return alloc::format!("the server sent alert {other}"),
    };
    alloc::format!("the server rejected the connection: {description}")
}

// ---------------------------------------------------------------------------
// The handshake
// ---------------------------------------------------------------------------

/// An established TLS connection.
pub struct Stream {
    reader: Reader,
    send_keys: Keys,
    recv_keys: Keys,
    /// Decrypted application bytes not yet handed to the caller.
    pending: Vec<u8>,
    /// Set by a close_notify, which is TLS's clean end-of-stream.
    finished: bool,
}

/// Accumulates the handshake messages that every secret is bound to.
///
/// The hash covers the handshake messages only — not the record framing they
/// arrived in — which is why messages are fed here individually rather than
/// records being hashed as they are read.
struct Transcript {
    hash: sha256::Sha256,
}

impl Transcript {
    fn new() -> Transcript {
        Transcript { hash: sha256::Sha256::new() }
    }

    fn add(&mut self, message: &[u8]) {
        self.hash.update(message);
    }

    fn digest(&self) -> [u8; 32] {
        self.hash.finish()
    }
}

/// Open a TLS connection to an already-resolved address.
///
/// `hostname` is sent in SNI and is what the server uses to pick which site to
/// serve, so it must be the name from the URL and not a stringified address.
pub fn connect(address: Ipv4Addr, port: u16, hostname: &str) -> Result<Stream, String> {
    tcp::connect(address, port).map_err(|e| e.to_string())?;
    match handshake(hostname) {
        Ok(stream) => Ok(stream),
        Err(error) => {
            tcp::close();
            Err(error)
        }
    }
}

fn handshake(hostname: &str) -> Result<Stream, String> {
    let deadline = crate::clock::ticks() + HANDSHAKE_TIMEOUT;

    let private_key = random::bytes32();
    let public_key = x25519::public_key(&private_key);
    let client_random = random::bytes32();
    let session_id = random::bytes32();

    let mut transcript = Transcript::new();
    let hello = client_hello(hostname, &public_key, &client_random, &session_id);
    transcript.add(&hello);
    // The first record claims TLS 1.0 for the benefit of the oldest middle
    // boxes, as RFC 8446 §5.1 permits.
    send_plain(CT_HANDSHAKE, 0x0301, &hello)?;

    let mut reader = Reader::new();

    // ServerHello arrives unencrypted — it carries the key share that makes
    // encryption possible.
    let server_hello = loop {
        let (content_type, _header, payload) = next_record(&mut reader, deadline)?;
        match content_type {
            CT_CHANGE_CIPHER_SPEC => continue,
            CT_ALERT => return Err(describe_alert(&payload)),
            CT_HANDSHAKE => {
                let mut cursor = Cursor::new(&payload);
                let kind = cursor.u8()?;
                let body = cursor.vector(3)?;
                if kind != HS_SERVER_HELLO {
                    return Err("the server replied with something other than a ServerHello".to_string());
                }
                transcript.add(&payload);
                break parse_server_hello(body)?;
            }
            other => {
                return Err(alloc::format!("unexpected record type {other} before the handshake finished"))
            }
        }
    };

    let shared = x25519::scalar_mult(&private_key, &server_hello.key_share);
    // RFC 7748 §6.1 requires rejecting an all-zero result: it means the server
    // sent a low-order point, and every session would share the same secret.
    if shared == [0u8; 32] {
        return Err("the server sent a degenerate key share".to_string());
    }

    let schedule = Schedule::new(&shared, &transcript.digest());
    let mut client_keys = Keys::from_secret(&schedule.client_handshake);
    let mut server_keys = Keys::from_secret(&schedule.server_handshake);

    // Everything from here is encrypted. Read the server's flight:
    // EncryptedExtensions, its certificate, the signature over the
    // transcript, and Finished.
    let mut pending_handshake: Vec<u8> = Vec::new();
    let mut transcript_before_finished = [0u8; 32];
    let mut server_verify: Option<Vec<u8>> = None;

    while server_verify.is_none() {
        let (content_type, header, payload) = next_record(&mut reader, deadline)?;
        match content_type {
            CT_CHANGE_CIPHER_SPEC => continue,
            CT_ALERT => return Err(describe_alert(&payload)),
            CT_APPLICATION_DATA => {
                let (inner_type, plaintext) = open_record(&mut server_keys, &header, payload)?;
                match inner_type {
                    CT_ALERT => return Err(describe_alert(&plaintext)),
                    CT_HANDSHAKE => pending_handshake.extend_from_slice(&plaintext),
                    other => {
                        return Err(alloc::format!(
                            "the server sent record type {other} during the handshake"
                        ))
                    }
                }
            }
            other => return Err(alloc::format!("unexpected record type {other}")),
        }

        // One record can hold several messages, and one message can span
        // records, so drain whatever is complete.
        while let Some(message) = take_handshake_message(&mut pending_handshake)? {
            let kind = message[0];
            match kind {
                HS_ENCRYPTED_EXTENSIONS | HS_CERTIFICATE | HS_CERTIFICATE_VERIFY => {
                    // The certificate is skipped rather than checked; see this
                    // module's documentation for exactly what that costs.
                    transcript.add(&message);
                }
                HS_CERTIFICATE_REQUEST => {
                    return Err("the server asked for a client certificate, which OS101 has none of".to_string())
                }
                HS_FINISHED => {
                    // The MAC covers everything up to but not including this
                    // message, so the digest has to be taken before adding it.
                    transcript_before_finished = transcript.digest();
                    server_verify = Some(message[4..].to_vec());
                    transcript.add(&message);
                    break;
                }
                other => {
                    return Err(alloc::format!(
                        "unexpected handshake message {other} from the server"
                    ))
                }
            }
        }
    }

    let expected = finished_mac(&schedule.server_handshake, &transcript_before_finished);
    let received = server_verify.unwrap_or_default();
    if received.len() != expected.len() || !constant_time_eq(&received, &expected) {
        return Err("the server's Finished message did not verify".to_string());
    }

    // Application keys are bound to the transcript through the server's
    // Finished — our own Finished comes after and does not affect them.
    let after_server_finished = transcript.digest();
    let (client_application, server_application) = schedule.application(&after_server_finished);

    // A change_cipher_spec here means nothing in TLS 1.3; it exists so the
    // exchange keeps looking like TLS 1.2 to anything watching.
    send_plain(CT_CHANGE_CIPHER_SPEC, VERSION_TLS12, &[1])?;

    let verify_data = finished_mac(&schedule.client_handshake, &after_server_finished);
    let mut finished = Vec::with_capacity(36);
    finished.push(HS_FINISHED);
    with_length(&mut finished, 3, |out| out.extend_from_slice(&verify_data));
    send_encrypted(&mut client_keys, CT_HANDSHAKE, &finished)?;

    Ok(Stream {
        reader,
        send_keys: Keys::from_secret(&client_application),
        recv_keys: Keys::from_secret(&server_application),
        pending: Vec::new(),
        finished: false,
    })
}

/// Read one record, keeping its header for use as AEAD additional data.
fn next_record(reader: &mut Reader, deadline: u64) -> Result<(u8, [u8; 5], Vec<u8>), String> {
    reader.fill(5, deadline)?;
    let content_type = reader.raw[0];
    let length = u16::from_be_bytes([reader.raw[3], reader.raw[4]]) as usize;
    if length > MAX_RECORD_PAYLOAD {
        return Err(alloc::format!("the server sent an oversized record ({length} bytes)"));
    }
    reader.fill(5 + length, deadline)?;

    let mut header = [0u8; 5];
    header.copy_from_slice(&reader.raw[..5]);
    let payload = reader.raw[5..5 + length].to_vec();
    reader.raw.drain(..5 + length);
    Ok((content_type, header, payload))
}

/// Pull one complete handshake message off the front of the buffer.
fn take_handshake_message(buffer: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let length = ((buffer[1] as usize) << 16) | ((buffer[2] as usize) << 8) | buffer[3] as usize;
    if length > MAX_BODY_PER_MESSAGE {
        return Err("a handshake message from the server was absurdly large".to_string());
    }
    if buffer.len() < 4 + length {
        return Ok(None);
    }
    let message = buffer[..4 + length].to_vec();
    buffer.drain(..4 + length);
    Ok(Some(message))
}

/// A certificate chain is the largest handshake message in practice, and a few
/// hundred kilobytes is far past anything legitimate.
const MAX_BODY_PER_MESSAGE: usize = 256 * 1024;

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for i in 0..a.len() {
        difference |= a[i] ^ b[i];
    }
    difference == 0
}

// ---------------------------------------------------------------------------
// Using the connection
// ---------------------------------------------------------------------------

impl Stream {
    /// Encrypt and send application data, fragmenting if it is large.
    pub fn send(&mut self, data: &[u8]) -> Result<(), String> {
        for chunk in data.chunks(MAX_FRAGMENT) {
            send_encrypted(&mut self.send_keys, CT_APPLICATION_DATA, chunk)?;
        }
        Ok(())
    }

    /// Read until the server closes the stream, or nothing arrives for a
    /// while, mirroring [`tcp::recv_to_end`] so the HTTP client can treat a
    /// TLS connection like a plain one.
    pub fn recv_to_end(&mut self, timeout_ticks: u64) -> Result<Vec<u8>, String> {
        let deadline = crate::clock::ticks() + timeout_ticks;
        let mut out = core::mem::take(&mut self.pending);

        while !self.finished {
            let (content_type, header, payload) = match next_record(&mut self.reader, deadline) {
                Ok(record) => record,
                // Servers do not always send close_notify before closing the
                // socket. Data already read is still good.
                Err(_) => break,
            };

            match content_type {
                CT_CHANGE_CIPHER_SPEC => continue,
                CT_ALERT => return Err(describe_alert(&payload)),
                CT_APPLICATION_DATA => {
                    let (inner_type, plaintext) = open_record(&mut self.recv_keys, &header, payload)?;
                    match inner_type {
                        CT_APPLICATION_DATA => out.extend_from_slice(&plaintext),
                        CT_ALERT => {
                            // close_notify is the ordinary end of a stream,
                            // not a failure.
                            if plaintext.len() >= 2 && plaintext[1] == 0 {
                                self.finished = true;
                                break;
                            }
                            return Err(describe_alert(&plaintext));
                        }
                        CT_HANDSHAKE => self.post_handshake(&plaintext)?,
                        other => {
                            return Err(alloc::format!("the server sent record type {other}"))
                        }
                    }
                }
                other => return Err(alloc::format!("unexpected record type {other}")),
            }

            if out.len() >= super::http::MAX_BODY + MAX_FRAGMENT {
                break;
            }
        }

        Ok(out)
    }

    /// Messages that may arrive after the handshake has completed.
    fn post_handshake(&mut self, plaintext: &[u8]) -> Result<(), String> {
        let mut buffer = plaintext.to_vec();
        while let Some(message) = take_handshake_message(&mut buffer)? {
            match message[0] {
                // Session tickets are for resumption, which OS101 does not do.
                // Servers send them unprompted, so they must be ignored rather
                // than treated as an error.
                HS_NEW_SESSION_TICKET => {}
                HS_KEY_UPDATE => {
                    return Err("the server asked to rekey, which OS101 does not support".to_string())
                }
                other => {
                    return Err(alloc::format!("unexpected message {other} after the handshake"))
                }
            }
        }
        Ok(())
    }

    /// Tell the server we are done, then drop the TCP connection.
    pub fn close(&mut self) {
        // A close_notify is a courtesy; if it cannot be sent the connection is
        // already gone, and there is nothing useful to do about it.
        let _ = send_encrypted(&mut self.send_keys, CT_ALERT, &[1, 0]);
        tcp::close();
    }
}

pub mod selftest;
