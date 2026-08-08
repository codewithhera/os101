//! HMAC-SHA-256, as specified in RFC 2104.
//!
//! A keyed hash, built by hashing the message twice under two different
//! transforms of the key. TLS 1.3 uses it for everything secret: the whole key
//! schedule is [`super::hkdf`], which is nothing but HMAC applied in a
//! particular order, and the Finished messages are an HMAC over the
//! transcript.

use super::sha256::{self, Sha256, BLOCK_LEN, DIGEST_LEN};

/// The inner and outer key transforms (RFC 2104 §2).
const INNER_PAD: u8 = 0x36;
const OUTER_PAD: u8 = 0x5c;

/// HMAC-SHA-256 of `message` under `key`.
///
/// Any key length works. A key longer than a block is replaced by its digest
/// and a shorter one is zero-padded, both as RFC 2104 §2 requires, so an empty
/// key is a block of zeros rather than an error.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; DIGEST_LEN] {
    hmac_sha256_parts(key, &[message])
}

/// HMAC-SHA-256 over the concatenation of `parts`, without joining them first.
///
/// HKDF-Expand's message is `T(n-1) || info || counter`, and `info` is however
/// long the caller's label and context turn out to be. Feeding the pieces to
/// the hash in turn keeps that off the heap and out of a fixed buffer that
/// could be too small.
pub fn hmac_sha256_parts(key: &[u8], parts: &[&[u8]]) -> [u8; DIGEST_LEN] {
    let mut padded_key = [0u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        padded_key[..DIGEST_LEN].copy_from_slice(&sha256::digest(key));
    } else {
        padded_key[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    inner.update(&xor_key(&padded_key, INNER_PAD));
    for part in parts {
        inner.update(part);
    }
    let inner = inner.finish();

    let mut outer = Sha256::new();
    outer.update(&xor_key(&padded_key, OUTER_PAD));
    outer.update(&inner);
    outer.finish()
}

fn xor_key(padded_key: &[u8; BLOCK_LEN], pad: u8) -> [u8; BLOCK_LEN] {
    let mut out = [0u8; BLOCK_LEN];
    for (out, byte) in out.iter_mut().zip(padded_key.iter()) {
        *out = byte ^ pad;
    }
    out
}

/// RFC 4231 §4's seven test cases, which between them cover a short key, a
/// key of exactly the digest length, a 131-byte key that has to be hashed
/// first, and data longer than a block.
///
/// The three key lengths either side of the block size, and the empty key,
/// have no RFC vector; those digests were generated with OpenSSL (via Python's
/// `hmac`) rather than with this code. They are here because the boundary
/// between padding a key and hashing it is at exactly 64 bytes.
pub fn selftest() -> crate::selftest::Report {
    /// The data from RFC 4231 test case 6, reused for the key-length
    /// boundaries below.
    const LARGER_KEY_DATA: &[u8] = b"Test Using Larger Than Block-Size Key - Hash Key First";

    const CASE_1: [u8; DIGEST_LEN] = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53,
        0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
        0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
        0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
    ];
    const CASE_2: [u8; DIGEST_LEN] = [
        0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e,
        0x6a, 0x04, 0x24, 0x26, 0x08, 0x95, 0x75, 0xc7,
        0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83,
        0x9d, 0xec, 0x58, 0xb9, 0x64, 0xec, 0x38, 0x43,
    ];
    const CASE_3: [u8; DIGEST_LEN] = [
        0x77, 0x3e, 0xa9, 0x1e, 0x36, 0x80, 0x0e, 0x46,
        0x85, 0x4d, 0xb8, 0xeb, 0xd0, 0x91, 0x81, 0xa7,
        0x29, 0x59, 0x09, 0x8b, 0x3e, 0xf8, 0xc1, 0x22,
        0xd9, 0x63, 0x55, 0x14, 0xce, 0xd5, 0x65, 0xfe,
    ];
    const CASE_4: [u8; DIGEST_LEN] = [
        0x82, 0x55, 0x8a, 0x38, 0x9a, 0x44, 0x3c, 0x0e,
        0xa4, 0xcc, 0x81, 0x98, 0x99, 0xf2, 0x08, 0x3a,
        0x85, 0xf0, 0xfa, 0xa3, 0xe5, 0x78, 0xf8, 0x07,
        0x7a, 0x2e, 0x3f, 0xf4, 0x67, 0x29, 0x66, 0x5b,
    ];
    const CASE_5: [u8; DIGEST_LEN] = [
        0xa3, 0xb6, 0x16, 0x74, 0x73, 0x10, 0x0e, 0xe0,
        0x6e, 0x0c, 0x79, 0x6c, 0x29, 0x55, 0x55, 0x2b,
        0xfa, 0x6f, 0x7c, 0x0a, 0x6a, 0x8a, 0xef, 0x8b,
        0x93, 0xf8, 0x60, 0xaa, 0xb0, 0xcd, 0x20, 0xc5,
    ];
    const CASE_6: [u8; DIGEST_LEN] = [
        0x60, 0xe4, 0x31, 0x59, 0x1e, 0xe0, 0xb6, 0x7f,
        0x0d, 0x8a, 0x26, 0xaa, 0xcb, 0xf5, 0xb7, 0x7f,
        0x8e, 0x0b, 0xc6, 0x21, 0x37, 0x28, 0xc5, 0x14,
        0x05, 0x46, 0x04, 0x0f, 0x0e, 0xe3, 0x7f, 0x54,
    ];
    const CASE_7: [u8; DIGEST_LEN] = [
        0x9b, 0x09, 0xff, 0xa7, 0x1b, 0x94, 0x2f, 0xcb,
        0x27, 0x63, 0x5f, 0xbc, 0xd5, 0xb0, 0xe9, 0x44,
        0xbf, 0xdc, 0x63, 0x64, 0x4f, 0x07, 0x13, 0x93,
        0x8a, 0x7f, 0x51, 0x53, 0x5c, 0x3a, 0x35, 0xe2,
    ];

    // Generated, as noted above.
    const EMPTY_KEY_AND_DATA: [u8; DIGEST_LEN] = [
        0xb6, 0x13, 0x67, 0x9a, 0x08, 0x14, 0xd9, 0xec,
        0x77, 0x2f, 0x95, 0xd7, 0x78, 0xc3, 0x5f, 0xc5,
        0xff, 0x16, 0x97, 0xc4, 0x93, 0x71, 0x56, 0x53,
        0xc6, 0xc7, 0x12, 0x14, 0x42, 0x92, 0xc5, 0xad,
    ];
    const KEY_OF_63: [u8; DIGEST_LEN] = [
        0xb9, 0xf6, 0xd3, 0x20, 0x56, 0x03, 0xdd, 0xdc,
        0xfb, 0x87, 0x1b, 0x81, 0x32, 0xf6, 0x98, 0xf2,
        0xfc, 0x3b, 0x0c, 0xf9, 0x76, 0x5f, 0xed, 0x01,
        0x13, 0x23, 0x1d, 0x1e, 0x67, 0x39, 0xd7, 0x28,
    ];
    const KEY_OF_64: [u8; DIGEST_LEN] = [
        0x84, 0x33, 0x2a, 0x75, 0x80, 0xed, 0x3c, 0xf7,
        0x5d, 0xe8, 0x3c, 0x64, 0x4c, 0x8d, 0x2c, 0x1c,
        0x26, 0x2a, 0xd9, 0x0e, 0x01, 0x90, 0xe5, 0xc5,
        0xae, 0x4b, 0x82, 0xb2, 0x10, 0x2e, 0x8e, 0x75,
    ];
    const KEY_OF_65: [u8; DIGEST_LEN] = [
        0xc6, 0x29, 0x55, 0xa9, 0x69, 0x44, 0xff, 0x68,
        0xde, 0xab, 0xbc, 0x0e, 0xab, 0x61, 0x92, 0x06,
        0x5c, 0x1c, 0x55, 0xbb, 0x8d, 0xde, 0xe1, 0x61,
        0x51, 0xed, 0x53, 0x37, 0xf9, 0x11, 0xea, 0xb9,
    ];

    let mut report = crate::selftest::Report::new();

    report.check("rfc 4231 case 1", hmac_sha256(&[0x0b; 20], b"Hi There") == CASE_1);
    report.check(
        "rfc 4231 case 2, a four-byte key",
        hmac_sha256(b"Jefe", b"what do ya want for nothing?") == CASE_2,
    );
    report.check("rfc 4231 case 3", hmac_sha256(&[0xaa; 20], &[0xdd; 50]) == CASE_3);

    let mut counting_key = [0u8; 25];
    for (i, byte) in counting_key.iter_mut().enumerate() {
        *byte = (i + 1) as u8;
    }
    report.check("rfc 4231 case 4", hmac_sha256(&counting_key, &[0xcd; 50]) == CASE_4);

    report.check(
        "rfc 4231 case 5",
        hmac_sha256(&[0x0c; 20], b"Test With Truncation") == CASE_5,
    );
    report.check(
        "rfc 4231 case 6, a key longer than a block",
        hmac_sha256(&[0xaa; 131], LARGER_KEY_DATA) == CASE_6,
    );
    report.check(
        "rfc 4231 case 7, long key and long data",
        hmac_sha256(
            &[0xaa; 131],
            b"This is a test using a larger than block-size key and a larger \
              than block-size data. The key needs to be hashed before being \
              used by the HMAC algorithm.",
        ) == CASE_7,
    );

    report.check("an empty key", hmac_sha256(&[], &[]) == EMPTY_KEY_AND_DATA);
    report.check("a 63-byte key", hmac_sha256(&[0xaa; 63], LARGER_KEY_DATA) == KEY_OF_63);
    report.check(
        "a key of exactly one block",
        hmac_sha256(&[0xaa; 64], LARGER_KEY_DATA) == KEY_OF_64,
    );
    report.check(
        "a key one byte over a block",
        hmac_sha256(&[0xaa; 65], LARGER_KEY_DATA) == KEY_OF_65,
    );

    // HKDF feeds its message in pieces, so a split message — including an
    // empty piece — has to hash the same as the whole.
    const SPLIT: [&[u8]; 3] = [b"Hi", &[], b" There"];
    report.check(
        "a message split into parts",
        hmac_sha256_parts(&[0x0b; 20], &SPLIT) == CASE_1,
    );

    report
}
