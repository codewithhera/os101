//! Checks for the TLS client, run at every boot.
//!
//! The key schedule is checked against the published handshake trace in
//! RFC 8448 §3, which is the point of this file. A cryptographic protocol
//! that is merely self-consistent is worthless — it will fail against the
//! first real server and give no clue why — so every secret below is compared
//! against a value someone else computed and published, not one this code
//! produced. The trace uses AES-128-GCM rather than our ChaCha20-Poly1305, but
//! the key schedule is identical up to the length of the key it expands, so
//! all of it applies.
//!
//! The parsing checks feed it the actual bytes from that trace, plus the
//! malformed inputs that a hostile or broken server could send.

use alloc::vec::Vec;

use super::*;
use crate::selftest::Report;

/// Decode a hex literal at boot. Only ever called on the constants below, so
/// a malformed one is a bug here rather than something to report.
fn hex(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let high = (bytes[i] as char).to_digit(16).unwrap_or(0) as u8;
        let low = (bytes[i + 1] as char).to_digit(16).unwrap_or(0) as u8;
        out.push((high << 4) | low);
        i += 2;
    }
    out
}

fn hex32(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&hex(text));
    out
}

// The RFC 8448 §3 "Simple 1-RTT Handshake" trace.
const SHARED_SECRET: &str = "8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d";
/// Hash of ClientHello..ServerHello.
const TRANSCRIPT_HELLO: &str = "860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8";
/// Hash of ClientHello..the server's Finished.
const TRANSCRIPT_FINISHED: &str = "9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13";
const HANDSHAKE_SECRET: &str = "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac";
const CLIENT_HANDSHAKE: &str = "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21";
const SERVER_HANDSHAKE: &str = "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38";
const CLIENT_APPLICATION: &str = "9e40646ce79a7f9dc05af8889bce6552875afa0b06df0087f792ebb7c17504a5";
const SERVER_APPLICATION: &str = "a11af9f05531f856ad47116b45a950328204b4f44bfb6b3a4b4f1f3fcb631643";
const SERVER_HANDSHAKE_IV: &str = "5d313eb2671276ee13000b30";
const CLIENT_HANDSHAKE_IV: &str = "5bd3c71b836e0b76bb73265f";
/// AES-128, hence sixteen bytes rather than the thirty-two ChaCha20 uses.
const SERVER_HANDSHAKE_KEY: &str = "3fce516009c21727d0f2e4e86ee403bc";
const CLIENT_FINISHED_KEY: &str = "b80ad01015fb2f0bd65ff7d4da5d6bf83f84821d1f87fdc7d3c75b5a7b42d9c4";
const CLIENT_VERIFY_DATA: &str = "a8ec436d677634ae525ac1fcebe11a039ec17694fac6e98527b642f2edd5ce61";
const SERVER_FINISHED_KEY: &str = "008d3b66f816ea559f96b537e885c31fc068bf492c652f01f288a1d8cdc19fc8";

/// The ServerHello from the same trace, handshake header and all.
const SERVER_HELLO: &str = "\
0200005603\
03a6af06a4121860dc5e6e60249cd34c95930c8ac5cb1434dac155772ed3e26928\
00130100002e00330024001d0020\
c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f\
002b00020304";
const SERVER_KEY_SHARE: &str = "c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f";
/// Where the cipher suite sits in that message's body: two bytes of version,
/// thirty-two of random, then the (here empty) session id.
const SUITE_OFFSET: usize = 2 + 32 + 1;

fn key_schedule(report: &mut Report) {
    let shared = hex32(SHARED_SECRET);
    let hello_hash = hex32(TRANSCRIPT_HELLO);
    let schedule = Schedule::new(&shared, &hello_hash);

    report.check("rfc 8448 handshake secret", schedule.handshake_secret == hex32(HANDSHAKE_SECRET));
    report.check("rfc 8448 client handshake secret", schedule.client_handshake == hex32(CLIENT_HANDSHAKE));
    report.check("rfc 8448 server handshake secret", schedule.server_handshake == hex32(SERVER_HANDSHAKE));

    let finished_hash = hex32(TRANSCRIPT_FINISHED);
    let (client_app, server_app) = schedule.application(&finished_hash);
    report.check("rfc 8448 client application secret", client_app == hex32(CLIENT_APPLICATION));
    report.check("rfc 8448 server application secret", server_app == hex32(SERVER_APPLICATION));

    // The traffic keys the secrets expand into. A sixteen-byte key because the
    // trace negotiated AES-128; ours would be thirty-two, but the expansion
    // being checked is the same function.
    let mut key = [0u8; 16];
    expand_label(&schedule.server_handshake, "key", &[], &mut key);
    report.check("rfc 8448 server handshake key", key[..] == hex(SERVER_HANDSHAKE_KEY)[..]);

    let server_keys = Keys::from_secret(&schedule.server_handshake);
    report.check("rfc 8448 server handshake iv", server_keys.iv[..] == hex(SERVER_HANDSHAKE_IV)[..]);
    let client_keys = Keys::from_secret(&schedule.client_handshake);
    report.check("rfc 8448 client handshake iv", client_keys.iv[..] == hex(CLIENT_HANDSHAKE_IV)[..]);

    // Finished, end to end: the key it derives and the MAC it produces.
    let mut finished_key = [0u8; 32];
    expand_label(&schedule.client_handshake, "finished", &[], &mut finished_key);
    report.check("rfc 8448 client finished key", finished_key == hex32(CLIENT_FINISHED_KEY));
    expand_label(&schedule.server_handshake, "finished", &[], &mut finished_key);
    report.check("rfc 8448 server finished key", finished_key == hex32(SERVER_FINISHED_KEY));

    let verify = finished_mac(&schedule.client_handshake, &finished_hash);
    report.check("rfc 8448 client verify data", verify == hex32(CLIENT_VERIFY_DATA));

    // A different transcript must produce a different MAC, which is the whole
    // reason Finished exists.
    let mut tampered = finished_hash;
    tampered[0] ^= 1;
    report.check(
        "a changed transcript changes verify data",
        finished_mac(&schedule.client_handshake, &tampered) != verify,
    );

    // Labels must be distinguishable, or a key derived for one purpose would
    // be valid for another.
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    expand_label(&schedule.handshake_secret, "key", &[], &mut a);
    expand_label(&schedule.handshake_secret, "iv", &[], &mut b);
    report.check("different labels give different keys", a != b);

    let mut short = [0u8; 12];
    let mut long = [0u8; 32];
    expand_label(&schedule.handshake_secret, "key", &[], &mut short);
    expand_label(&schedule.handshake_secret, "key", &[], &mut long);
    // The requested length is part of the label structure, so a short
    // expansion is not a prefix of a long one.
    report.check("length is bound into the expansion", short[..] != long[..12]);
}

fn nonces(report: &mut Report) {
    let mut keys = Keys { key: [0u8; 32], iv: [0x11; 12], seq: 0 };
    report.check("the first nonce is the iv", keys.nonce() == [0x11; 12]);

    keys.seq = 1;
    let mut expected = [0x11u8; 12];
    expected[11] ^= 1;
    report.check("the counter xors into the low bytes", keys.nonce() == expected);

    keys.seq = 0x0102_0304_0506_0708;
    let nonce = keys.nonce();
    report.check("the counter is big endian", nonce[4] == 0x11 ^ 0x01 && nonce[11] == 0x11 ^ 0x08);
    report.check("the iv prefix is untouched", nonce[..4] == [0x11; 4]);

    // Two sequence numbers must never produce the same nonce: reuse would
    // destroy the AEAD outright.
    keys.seq = 7;
    let seven = keys.nonce();
    keys.seq = 8;
    report.check("consecutive records differ", seven != keys.nonce());
}

fn record_layer(report: &mut Report) {
    let key = [0x2Au8; 32];
    let iv = [0x3Bu8; 12];

    // Build a record the way `send_encrypted` does, then read it back the way
    // `open_record` does.
    let plaintext = b"hello from a hobby operating system".to_vec();
    let mut inner = plaintext.clone();
    inner.push(CT_APPLICATION_DATA);
    let length = (inner.len() + 16) as u16;
    let header = [
        CT_APPLICATION_DATA,
        0x03,
        0x03,
        (length >> 8) as u8,
        length as u8,
    ];
    let mut sealed = inner.clone();
    crate::crypto::chacha20poly1305::seal(&key, &iv, &header, &mut sealed);

    let mut keys = Keys { key, iv, seq: 0 };
    match open_record(&mut keys, &header, sealed.clone()) {
        Ok((content_type, body)) => {
            report.check("a record round trips", body == plaintext);
            report.check("the inner content type survives", content_type == CT_APPLICATION_DATA);
            report.check("opening advances the counter", keys.seq == 1);
        }
        Err(_) => {
            report.check("a record round trips", false);
            report.check("the inner content type survives", false);
            report.check("opening advances the counter", false);
        }
    }

    // Padding is zero bytes after the content type, and must be stripped
    // without eating the type itself.
    let mut padded = plaintext.clone();
    padded.push(CT_HANDSHAKE);
    padded.extend_from_slice(&[0, 0, 0, 0]);
    let length = (padded.len() + 16) as u16;
    let header = [CT_APPLICATION_DATA, 0x03, 0x03, (length >> 8) as u8, length as u8];
    let mut sealed_padded = padded;
    crate::crypto::chacha20poly1305::seal(&key, &iv, &header, &mut sealed_padded);
    let mut keys = Keys { key, iv, seq: 0 };
    match open_record(&mut keys, &header, sealed_padded) {
        Ok((content_type, body)) => {
            report.check("padding is stripped", body == plaintext);
            report.check("the type behind padding is found", content_type == CT_HANDSHAKE);
        }
        Err(_) => {
            report.check("padding is stripped", false);
            report.check("the type behind padding is found", false);
        }
    }

    // A record must not open under the wrong sequence number, or a replayed or
    // reordered record would be accepted.
    let mut wrong = Keys { key, iv, seq: 1 };
    report.check(
        "a record will not open out of order",
        open_record(&mut wrong, &header, sealed.clone()).is_err(),
    );

    let mut tampered = sealed.clone();
    tampered[0] ^= 1;
    let mut keys = Keys { key, iv, seq: 0 };
    report.check(
        "a tampered record is rejected",
        open_record(&mut keys, &header, tampered).is_err(),
    );

    let mut wrong_header = header;
    wrong_header[4] ^= 1;
    let mut keys = Keys { key, iv, seq: 0 };
    report.check(
        "the header is authenticated",
        open_record(&mut keys, &wrong_header, sealed).is_err(),
    );

    let mut keys = Keys { key, iv, seq: 0 };
    report.check(
        "an empty record is rejected",
        open_record(&mut keys, &header, Vec::new()).is_err(),
    );
}

fn hello(report: &mut Report) {
    let public_key = [0x77u8; 32];
    let random = [0x11u8; 32];
    let session = [0x22u8; 32];
    let message = client_hello("www.google.com", &public_key, &random, &session);

    report.check("a client hello is a handshake message", message[0] == HS_CLIENT_HELLO);

    // Every nested length must add up, which is what `with_length` exists to
    // guarantee. Walking the structure back out is the only way to be sure.
    let mut cursor = Cursor::new(&message);
    let ok = (|| -> Result<bool, alloc::string::String> {
        cursor.u8()?;
        let body = cursor.vector(3)?;
        let mut inner = Cursor::new(body);
        let version = inner.u16()?;
        let hello_random = inner.take(32)?;
        let session_id = inner.vector(1)?;
        let suites = inner.vector(2)?;
        let compression = inner.vector(1)?;
        let extensions = inner.vector(2)?;
        Ok(version == VERSION_TLS12
            && hello_random == random
            && session_id == session
            && suites == TLS_CHACHA20_POLY1305_SHA256.to_be_bytes()
            && compression == [0]
            && !extensions.is_empty()
            && inner.remaining() == 0)
    })()
    .unwrap_or(false);
    report.check("a client hello parses back", ok);
    report.check("the hello consumes the whole message", cursor.remaining() == 0);

    // The extensions a server will refuse us without.
    let mut kinds = Vec::new();
    let mut cursor = Cursor::new(&message);
    let _ = (|| -> Result<(), alloc::string::String> {
        cursor.u8()?;
        let body = cursor.vector(3)?;
        let mut inner = Cursor::new(body);
        inner.u16()?;
        inner.take(32)?;
        inner.vector(1)?;
        inner.vector(2)?;
        inner.vector(1)?;
        let extensions = inner.vector(2)?;
        let mut ext = Cursor::new(extensions);
        while ext.remaining() > 0 {
            kinds.push(ext.u16()?);
            ext.vector(2)?;
        }
        Ok(())
    })();

    report.check("the hello names the server", kinds.contains(&0));
    report.check("the hello asks for tls 1.3", kinds.contains(&43));
    report.check("the hello offers a group", kinds.contains(&10));
    report.check("the hello lists signature algorithms", kinds.contains(&13));
    report.check("the hello carries a key share", kinds.contains(&51));
    report.check("the hello asks for http/1.1", kinds.contains(&16));

    // The hostname has to reach the wire verbatim, or the server serves the
    // wrong site — or refuses.
    let hostname = b"www.google.com";
    let found = message.windows(hostname.len()).any(|w| w == hostname);
    report.check("the hostname is in the hello", found);
    let found_key = message.windows(32).any(|w| w == public_key);
    report.check("the public key is in the hello", found_key);

    // An empty hostname must not produce a malformed message.
    let empty = client_hello("", &public_key, &random, &session);
    report.check("an empty hostname still builds", empty.len() > 40);
}

fn server_hello(report: &mut Report) {
    let bytes = hex(SERVER_HELLO);
    let body = &bytes[4..];

    // The trace negotiated AES-128-GCM, which OS101 does not implement, so the
    // real message must be turned away rather than half-understood. Accepting
    // a suite we cannot decrypt would fail later and much more confusingly.
    report.check("an unusable cipher suite is refused", parse_server_hello(body).is_err());

    // The same message with our suite substituted is one we should understand
    // completely — the layout is a real server's, only the choice differs.
    let mut ours = body.to_vec();
    ours[SUITE_OFFSET..SUITE_OFFSET + 2]
        .copy_from_slice(&TLS_CHACHA20_POLY1305_SHA256.to_be_bytes());
    match parse_server_hello(&ours) {
        Ok(parsed) => report.check(
            "rfc 8448 server hello parses",
            parsed.key_share[..] == hex(SERVER_KEY_SHARE)[..],
        ),
        Err(_) => report.check("rfc 8448 server hello parses", false),
    }

    // HelloRetryRequest wears a ServerHello's clothes; the random field is the
    // only thing that distinguishes it.
    let mut retry = ours.clone();
    retry[2..34].copy_from_slice(&HELLO_RETRY_REQUEST_RANDOM);
    report.check("a hello retry request is recognised", parse_server_hello(&retry).is_err());

    // A server that agrees to a version we did not offer, or forgets the key
    // share, leaves us with no way to continue.
    let mut no_version = ours.clone();
    let version_at = no_version.len() - 2;
    no_version[version_at] = 0x03;
    no_version[version_at + 1] = 0x03;
    report.check("a tls 1.2 fallback is refused", parse_server_hello(&no_version).is_err());

    let without_key_share = {
        let mut trimmed = ours[..SUITE_OFFSET + 3].to_vec();
        // An extensions block holding only supported_versions.
        trimmed.extend_from_slice(&[0x00, 0x06, 0x00, 0x2B, 0x00, 0x02, 0x03, 0x04]);
        trimmed
    };
    report.check(
        "a missing key share is refused",
        parse_server_hello(&without_key_share).is_err(),
    );

    // Truncation at every length must produce an error, never a panic. A
    // server that hangs up mid-message is ordinary, and a kernel that faults
    // on it is not.
    let mut survived = true;
    for cut in 0..ours.len() {
        if parse_server_hello(&ours[..cut]).is_ok() {
            survived = false;
        }
    }
    report.check("a truncated server hello is refused", survived);

    report.check("an empty server hello is refused", parse_server_hello(&[]).is_err());
}

fn framing(report: &mut Report) {
    // Two messages in one buffer, the second incomplete.
    let mut buffer = Vec::new();
    buffer.extend_from_slice(&[HS_ENCRYPTED_EXTENSIONS, 0, 0, 2, 0xAA, 0xBB]);
    buffer.extend_from_slice(&[HS_FINISHED, 0, 0, 4, 0x01]);

    match take_handshake_message(&mut buffer) {
        Ok(Some(message)) => {
            report.check("a whole message is taken", message == [HS_ENCRYPTED_EXTENSIONS, 0, 0, 2, 0xAA, 0xBB]);
            report.check("the rest stays buffered", buffer.len() == 5);
        }
        _ => {
            report.check("a whole message is taken", false);
            report.check("the rest stays buffered", false);
        }
    }
    report.check(
        "a partial message waits",
        matches!(take_handshake_message(&mut buffer), Ok(None)),
    );

    // Completing it must then yield the message.
    buffer.extend_from_slice(&[0x02, 0x03, 0x04]);
    report.check(
        "a completed message is taken",
        matches!(take_handshake_message(&mut buffer), Ok(Some(m)) if m.len() == 8),
    );
    report.check("the buffer empties", buffer.is_empty());

    let mut header_only = alloc::vec![HS_FINISHED, 0, 0];
    report.check(
        "a partial header waits",
        matches!(take_handshake_message(&mut header_only), Ok(None)),
    );

    // A length field a server could use to make us allocate gigabytes.
    let mut absurd = alloc::vec![HS_CERTIFICATE, 0xFF, 0xFF, 0xFF];
    report.check(
        "an absurd length is refused",
        take_handshake_message(&mut absurd).is_err(),
    );
}

fn cursor(report: &mut Report) {
    let data = [1u8, 2, 3, 4];
    let mut c = Cursor::new(&data);
    report.check("a cursor reads a byte", c.u8() == Ok(1));
    report.check("a cursor reads a short", c.u16() == Ok(0x0203));
    report.check("a cursor tracks what is left", c.remaining() == 1);
    report.check("a cursor stops at the end", c.take(2).is_err());

    let mut empty = Cursor::new(&[]);
    report.check("an empty cursor yields nothing", empty.u8().is_err());

    // A length prefix longer than the data must fail rather than slice out of
    // bounds.
    let lying = [0x00u8, 0x10, 0x01];
    let mut c = Cursor::new(&lying);
    report.check("an overlong vector is refused", c.vector(2).is_err());

    let vector = [0x00u8, 0x02, 0xAA, 0xBB, 0xCC];
    let mut c = Cursor::new(&vector);
    report.check("a vector reads its length", c.vector(2) == Ok(&[0xAA, 0xBB][..]));
    report.check("a vector leaves the tail", c.remaining() == 1);
}

fn helpers(report: &mut Report) {
    // `with_length` backfills a length it does not know in advance, which is
    // the one thing every message here depends on.
    let mut out = Vec::new();
    with_length(&mut out, 2, |body| body.extend_from_slice(&[1, 2, 3]));
    report.check("a two byte length is written", out == [0, 3, 1, 2, 3]);

    let mut out = Vec::new();
    with_length(&mut out, 3, |body| {
        with_length(body, 1, |inner| inner.extend_from_slice(&[9, 9]));
    });
    report.check("nested lengths nest", out == [0, 0, 3, 2, 9, 9]);

    let mut out = Vec::new();
    with_length(&mut out, 1, |_| {});
    report.check("an empty block writes zero", out == [0]);

    report.check("equal slices compare equal", constant_time_eq(&[1, 2, 3], &[1, 2, 3]));
    report.check("different slices compare unequal", !constant_time_eq(&[1, 2, 3], &[1, 2, 4]));
    report.check("different lengths compare unequal", !constant_time_eq(&[1, 2], &[1, 2, 3]));
    report.check("empty slices compare equal", constant_time_eq(&[], &[]));

    // Alerts are what a server sends when it refuses us, so they have to turn
    // into something a person can act on rather than a number.
    report.check("a known alert is named", describe_alert(&[2, 112]).contains("does not serve"));
    report.check("an unknown alert still reports", describe_alert(&[2, 200]).contains("200"));
    report.check("a short alert does not panic", !describe_alert(&[]).is_empty());
}

pub fn run() -> Report {
    let mut report = Report::new();
    key_schedule(&mut report);
    nonces(&mut report);
    record_layer(&mut report);
    hello(&mut report);
    server_hello(&mut report);
    framing(&mut report);
    cursor(&mut report);
    helpers(&mut report);
    report
}
