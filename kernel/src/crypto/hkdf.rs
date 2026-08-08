//! HKDF-SHA-256, as specified in RFC 5869.
//!
//! Extract-then-expand: condense whatever entropy the input has into one
//! uniform pseudorandom key, then stretch that into as many labelled outputs
//! as are needed. TLS 1.3's key schedule is built entirely from the two, so
//! every secret in a connection — handshake traffic keys, application traffic
//! keys, the Finished keys — comes out of here.

use super::hmac;
use super::sha256::{self, DIGEST_LEN};

/// The most one expansion can produce. The counter appended to each block is a
/// single byte, which caps the output at 255 blocks (RFC 5869 §2.3).
const MAX_OUTPUT: usize = 255 * DIGEST_LEN;

/// HKDF-Extract: a pseudorandom key from input keying material.
///
/// An empty `salt` is the same thing as RFC 5869's "if not provided, it is set
/// to a string of HashLen zeros", and needs no special case: HMAC pads a key
/// shorter than a block with zeros regardless. TLS 1.3 relies on that, since
/// its early stages extract with a salt that is not there yet.
pub fn extract(salt: &[u8], ikm: &[u8]) -> [u8; DIGEST_LEN] {
    hmac::hmac_sha256(salt, ikm)
}

/// HKDF-Expand: fill `out` with key material derived from `prk` and `info`.
///
/// Fails only when `out` is longer than 255 digests, which no TLS derivation
/// comes near.
pub fn expand(prk: &[u8; DIGEST_LEN], info: &[u8], out: &mut [u8]) -> Result<(), &'static str> {
    if out.len() > MAX_OUTPUT {
        return Err("hkdf: expand asked for more than 255 blocks");
    }

    // T(1) is HMAC(prk, info | 1) and T(n) is HMAC(prk, T(n-1) | info | n), so
    // the rounds differ only in whether there is a previous block to feed
    // back. The first round has none, which is a feedback length of zero.
    let mut previous = [0u8; DIGEST_LEN];
    let mut feedback_len = 0;
    let mut counter: u8 = 1;
    let mut written = 0;
    while written < out.len() {
        let block = hmac::hmac_sha256_parts(prk, &[&previous[..feedback_len], info, &[counter]]);

        let remaining = out.len() - written;
        let take = if remaining < DIGEST_LEN { remaining } else { DIGEST_LEN };
        out[written..written + take].copy_from_slice(&block[..take]);
        written += take;
        previous = block;
        feedback_len = DIGEST_LEN;

        // 255 is the last counter value a legal `out` can reach, so the wrap
        // can only happen on the iteration that also ends the loop.
        counter = counter.wrapping_add(1);
    }
    Ok(())
}

/// RFC 5869 §A.1 to §A.3: the basic case, a case with 80-byte inputs and an
/// output spanning three blocks, and a case with no salt and no info at all.
///
/// The digest of a maximum-length expansion is not from the RFC; it was
/// generated with OpenSSL (via Python's `hmac`) rather than with this code. It
/// covers the block counter reaching 255, which is the one place here that
/// could overflow.
pub fn selftest() -> crate::selftest::Report {
    const PRK_1: [u8; DIGEST_LEN] = [
        0x07, 0x77, 0x09, 0x36, 0x2c, 0x2e, 0x32, 0xdf,
        0x0d, 0xdc, 0x3f, 0x0d, 0xc4, 0x7b, 0xba, 0x63,
        0x90, 0xb6, 0xc7, 0x3b, 0xb5, 0x0f, 0x9c, 0x31,
        0x22, 0xec, 0x84, 0x4a, 0xd7, 0xc2, 0xb3, 0xe5,
    ];
    const OKM_1: [u8; 42] = [
        0x3c, 0xb2, 0x5f, 0x25, 0xfa, 0xac, 0xd5, 0x7a,
        0x90, 0x43, 0x4f, 0x64, 0xd0, 0x36, 0x2f, 0x2a,
        0x2d, 0x2d, 0x0a, 0x90, 0xcf, 0x1a, 0x5a, 0x4c,
        0x5d, 0xb0, 0x2d, 0x56, 0xec, 0xc4, 0xc5, 0xbf,
        0x34, 0x00, 0x72, 0x08, 0xd5, 0xb8, 0x87, 0x18,
        0x58, 0x65,
    ];
    const PRK_2: [u8; DIGEST_LEN] = [
        0x06, 0xa6, 0xb8, 0x8c, 0x58, 0x53, 0x36, 0x1a,
        0x06, 0x10, 0x4c, 0x9c, 0xeb, 0x35, 0xb4, 0x5c,
        0xef, 0x76, 0x00, 0x14, 0x90, 0x46, 0x71, 0x01,
        0x4a, 0x19, 0x3f, 0x40, 0xc1, 0x5f, 0xc2, 0x44,
    ];
    const OKM_2: [u8; 82] = [
        0xb1, 0x1e, 0x39, 0x8d, 0xc8, 0x03, 0x27, 0xa1,
        0xc8, 0xe7, 0xf7, 0x8c, 0x59, 0x6a, 0x49, 0x34,
        0x4f, 0x01, 0x2e, 0xda, 0x2d, 0x4e, 0xfa, 0xd8,
        0xa0, 0x50, 0xcc, 0x4c, 0x19, 0xaf, 0xa9, 0x7c,
        0x59, 0x04, 0x5a, 0x99, 0xca, 0xc7, 0x82, 0x72,
        0x71, 0xcb, 0x41, 0xc6, 0x5e, 0x59, 0x0e, 0x09,
        0xda, 0x32, 0x75, 0x60, 0x0c, 0x2f, 0x09, 0xb8,
        0x36, 0x77, 0x93, 0xa9, 0xac, 0xa3, 0xdb, 0x71,
        0xcc, 0x30, 0xc5, 0x81, 0x79, 0xec, 0x3e, 0x87,
        0xc1, 0x4c, 0x01, 0xd5, 0xc1, 0xf3, 0x43, 0x4f,
        0x1d, 0x87,
    ];
    const PRK_3: [u8; DIGEST_LEN] = [
        0x19, 0xef, 0x24, 0xa3, 0x2c, 0x71, 0x7b, 0x16,
        0x7f, 0x33, 0xa9, 0x1d, 0x6f, 0x64, 0x8b, 0xdf,
        0x96, 0x59, 0x67, 0x76, 0xaf, 0xdb, 0x63, 0x77,
        0xac, 0x43, 0x4c, 0x1c, 0x29, 0x3c, 0xcb, 0x04,
    ];
    const OKM_3: [u8; 42] = [
        0x8d, 0xa4, 0xe7, 0x75, 0xa5, 0x63, 0xc1, 0x8f,
        0x71, 0x5f, 0x80, 0x2a, 0x06, 0x3c, 0x5a, 0x31,
        0xb8, 0xa1, 0x1f, 0x5c, 0x5e, 0xe1, 0x87, 0x9e,
        0xc3, 0x45, 0x4e, 0x5f, 0x3c, 0x73, 0x8d, 0x2d,
        0x9d, 0x20, 0x13, 0x95, 0xfa, 0xa4, 0xb6, 0x1a,
        0x96, 0xc8,
    ];

    /// Generated: SHA-256 of a full 255-block expansion of case 1's inputs.
    const FULL_EXPANSION: [u8; DIGEST_LEN] = [
        0x06, 0xce, 0x74, 0x19, 0x40, 0x5a, 0x88, 0xa6,
        0x6b, 0xa5, 0xc9, 0x79, 0x55, 0x79, 0xcb, 0x05,
        0x13, 0x0c, 0x85, 0x10, 0x19, 0x24, 0xd1, 0x87,
        0x55, 0x2a, 0x0f, 0x7f, 0x57, 0xde, 0xb0, 0x91,
    ];

    let mut report = crate::selftest::Report::new();

    // Case 1: a 22-byte secret, a 13-byte salt and a 10-byte info.
    let ikm_1 = [0x0b; 22];
    let mut salt_1 = [0u8; 13];
    for (i, byte) in salt_1.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut info_1 = [0u8; 10];
    for (i, byte) in info_1.iter_mut().enumerate() {
        *byte = (0xf0 + i) as u8;
    }
    let prk_1 = extract(&salt_1, &ikm_1);
    report.check("rfc 5869 case 1 extract", prk_1 == PRK_1);
    let mut okm_1 = [0u8; 42];
    let expanded = expand(&prk_1, &info_1, &mut okm_1).is_ok();
    report.check("rfc 5869 case 1 expand", expanded && okm_1 == OKM_1);

    // Case 2: 80 bytes of everything, and 82 bytes out, so three blocks with
    // the last one cut short.
    let mut ikm_2 = [0u8; 80];
    for (i, byte) in ikm_2.iter_mut().enumerate() {
        *byte = i as u8;
    }
    let mut salt_2 = [0u8; 80];
    for (i, byte) in salt_2.iter_mut().enumerate() {
        *byte = (0x60 + i) as u8;
    }
    let mut info_2 = [0u8; 80];
    for (i, byte) in info_2.iter_mut().enumerate() {
        *byte = (0xb0 + i) as u8;
    }
    let prk_2 = extract(&salt_2, &ikm_2);
    report.check("rfc 5869 case 2 extract, an 80-byte salt", prk_2 == PRK_2);
    let mut okm_2 = [0u8; 82];
    let expanded = expand(&prk_2, &info_2, &mut okm_2).is_ok();
    report.check("rfc 5869 case 2 expand, three blocks", expanded && okm_2 == OKM_2);

    // Case 3: no salt, no info.
    let prk_3 = extract(&[], &ikm_1);
    report.check("rfc 5869 case 3 extract, no salt", prk_3 == PRK_3);
    let mut okm_3 = [0u8; 42];
    let expanded = expand(&prk_3, &[], &mut okm_3).is_ok();
    report.check("rfc 5869 case 3 expand, no info", expanded && okm_3 == OKM_3);

    report.check(
        "an absent salt is a salt of zeros",
        extract(&[], &ikm_1) == extract(&[0u8; DIGEST_LEN], &ikm_1),
    );

    // The output is a prefix of the same stream whatever length is asked for,
    // so a short request has to agree with case 1's published material.
    let mut one_byte = [0u8; 1];
    let mut over_a_block = [0u8; 33];
    let short = expand(&prk_1, &info_1, &mut one_byte).is_ok();
    let long = expand(&prk_1, &info_1, &mut over_a_block).is_ok();
    report.check(
        "a partial last block is cut to length",
        short && long && one_byte[0] == OKM_1[0] && over_a_block[..] == OKM_1[..33],
    );

    let mut nothing = [0u8; 0];
    report.check("expanding nothing does nothing", expand(&prk_1, &info_1, &mut nothing).is_ok());

    // Eight kilobytes is too much for the boot stack, and this is the only
    // place in the module that needs a buffer that size.
    let mut largest = alloc::vec::Vec::new();
    largest.resize(MAX_OUTPUT + 1, 0u8);
    report.check(
        "expand refuses more than 255 blocks",
        expand(&prk_1, &info_1, &mut largest).is_err(),
    );
    largest.truncate(MAX_OUTPUT);
    let filled = expand(&prk_1, &info_1, &mut largest).is_ok();
    report.check(
        "expand fills the full 255 blocks",
        filled && sha256::digest(&largest) == FULL_EXPANSION,
    );

    report
}
