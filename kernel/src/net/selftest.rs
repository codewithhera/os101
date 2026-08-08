//! Boot-time checks for the parts of the network stack that are pure
//! functions.
//!
//! It covers the code most likely to break silently — checksums, address and
//! URL parsing, chunked decoding and sequence-number comparison — none of
//! which need a network to be present.

use alloc::vec::Vec;

use crate::selftest::Report;

use super::ip::{self, Ipv4Addr};
use super::{dns, http, tcp};

pub fn run() -> Report {
    let mut r = Report::new();

    checksums(&mut r);
    addresses(&mut r);
    urls(&mut r);
    chunked(&mut r);
    dns_encoding(&mut r);
    sequence_numbers(&mut r);

    r
}

fn checksums(r: &mut Report) {
    // A real IPv4 header with its checksum field zeroed. The correct answer,
    // 0xB861, is the worked example from RFC 1071.
    let header: [u8; 20] = [
        0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11,
        0x00, 0x00, 0xC0, 0xA8, 0x00, 0x01, 0xC0, 0xA8, 0x00, 0xC7,
    ];
    r.check("ipv4 checksum", ip::checksum(&header) == 0xB861);

    // Verifying a header that already carries its checksum must give zero.
    let mut verified = header;
    verified[10] = 0xB8;
    verified[11] = 0x61;
    r.check("checksum verifies to zero", ip::checksum(&verified) == 0);

    // An odd-length buffer exercises the trailing-byte path.
    r.check("odd length checksum", ip::checksum(&[0x00, 0x01, 0xF2]) != 0);
}

fn addresses(r: &mut Report) {
    r.check("parse dotted quad", Ipv4Addr::parse("192.168.1.1") == Some(Ipv4Addr::new(192, 168, 1, 1)));
    r.check("parse zero address", Ipv4Addr::parse("0.0.0.0") == Some(Ipv4Addr::UNSPECIFIED));
    r.check("reject short address", Ipv4Addr::parse("10.0.2").is_none());
    r.check("reject long address", Ipv4Addr::parse("10.0.2.15.1").is_none());
    r.check("reject out-of-range octet", Ipv4Addr::parse("10.0.2.256").is_none());
    r.check("reject hostname as address", Ipv4Addr::parse("example.com").is_none());

    let mask = Ipv4Addr::new(255, 255, 255, 0);
    r.check(
        "same subnet",
        Ipv4Addr::new(10, 0, 2, 15).same_subnet(Ipv4Addr::new(10, 0, 2, 2), mask),
    );
    r.check(
        "different subnet",
        !Ipv4Addr::new(10, 0, 2, 15).same_subnet(Ipv4Addr::new(10, 0, 3, 2), mask),
    );
}

fn urls(r: &mut Report) {
    match http::Url::parse("http://example.com/a/b?c=d") {
        Ok(u) => {
            r.check("url host", u.host == "example.com");
            r.check("url default port", u.port == 80);
            r.check("url path", u.path == "/a/b?c=d");
        }
        Err(_) => r.check("parse absolute url", false),
    }

    match http::Url::parse("example.com") {
        Ok(u) => {
            r.check("bare host", u.host == "example.com");
            r.check("bare host default path", u.path == "/");
        }
        Err(_) => r.check("parse bare host", false),
    }

    match http::Url::parse("http://example.com:8080/x") {
        Ok(u) => r.check("explicit port", u.port == 8080 && u.path == "/x"),
        Err(_) => r.check("parse explicit port", false),
    }

    match http::Url::parse("https://example.com/x") {
        Ok(u) => {
            r.check("https is secure", u.secure);
            r.check("https default port", u.port == 443);
            r.check("https path", u.path == "/x");
        }
        Err(_) => r.check("parse https url", false),
    }

    // A bare hostname means HTTPS now — typing `example.com` in the address
    // bar should reach the site the way any other browser would.
    match http::Url::parse("example.com") {
        Ok(u) => r.check("a bare host is secure", u.secure && u.port == 443),
        Err(_) => r.check("parse bare host as https", false),
    }
    match http::Url::parse("http://example.com") {
        Ok(u) => r.check("http stays plain", !u.secure && u.port == 80),
        Err(_) => r.check("parse plain http", false),
    }

    match http::Url::parse("https://example.com:8443/y") {
        Ok(u) => r.check("https explicit port", u.secure && u.port == 8443),
        Err(_) => r.check("parse https explicit port", false),
    }

    // Round-tripping has to keep the scheme, or a redirect would silently
    // downgrade the connection.
    match http::Url::parse("https://example.com/a") {
        Ok(u) => r.check("https round trips", u.to_absolute() == "https://example.com/a"),
        Err(_) => r.check("https round trips", false),
    }
    match http::Url::parse("http://example.com:8080/a") {
        Ok(u) => r.check("a port round trips", u.to_absolute() == "http://example.com:8080/a"),
        Err(_) => r.check("a port round trips", false),
    }

    r.check("reject empty url", http::Url::parse("").is_err());
}

fn chunked(r: &mut Report) {
    let body = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
    r.check("chunked decode", http::decode_chunked(body) == b"hello world".to_vec());

    // Chunk extensions after a semicolon must be ignored.
    let with_ext = b"3;name=value\r\nabc\r\n0\r\n\r\n";
    r.check("chunk extensions", http::decode_chunked(with_ext) == b"abc".to_vec());

    r.check("empty chunked body", http::decode_chunked(b"0\r\n\r\n").is_empty());
}

fn dns_encoding(r: &mut Report) {
    let mut out = Vec::new();
    let ok = dns::encode_name("www.example.com", &mut out).is_ok();
    r.check(
        "dns name encoding",
        ok && out == b"\x03www\x07example\x03com\x00".to_vec(),
    );

    // A label over 63 bytes is not representable.
    let long = "a".repeat(64);
    let mut out2 = Vec::new();
    r.check("reject long dns label", dns::encode_name(&long, &mut out2).is_err());
}

fn sequence_numbers(r: &mut Report) {
    r.check("seq ordering", tcp::seq_le(100, 200));
    r.check("seq equal", tcp::seq_le(100, 100));
    r.check("seq reverse", !tcp::seq_le(200, 100));
    // The comparison has to survive the counter wrapping past 2^32.
    r.check("seq wraparound", tcp::seq_le(0xFFFF_FF00, 0x0000_0100));
    r.check("seq wraparound reverse", !tcp::seq_le(0x0000_0100, 0xFFFF_FF00));
}

