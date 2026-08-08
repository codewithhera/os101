//! The ChaCha20-Poly1305 AEAD of RFC 8439.
//!
//! TLS 1.3 calls this construction `TLS_CHACHA20_POLY1305_SHA256`, and every
//! record after the ClientHello — handshake and application data alike — passes
//! through [`seal`] or [`open`]. Both work in place on a `Vec`, because that is
//! the shape a TLS record wants: the tag lives immediately after the ciphertext,
//! so sealing is "append 16 bytes" and opening is "truncate 16 bytes".
//!
//! # Structure
//!
//! ChaCha20 is a stream cipher: it expands a key, a 96-bit nonce, and a 32-bit
//! block counter into a keystream that is XORed with the message. Poly1305 is a
//! one-time authenticator: it evaluates the message as a polynomial over
//! GF(2^130 - 5) at a secret point `r`, then blinds the result by adding a
//! secret `s`. Neither is safe if a key is reused, which is why the AEAD derives
//! a fresh `(r, s)` for every nonce from the counter-0 keystream block and
//! encrypts the payload from counter 1 onwards.
//!
//! # Caveats
//!
//! The arithmetic here is add/shift/multiply on fixed-width words with no
//! data-dependent branches or table lookups, so it does not leak through timing
//! or the cache by construction rather than by effort. The tag comparison in
//! [`open`] is explicitly constant time; a decryptor that returns early on the
//! first wrong tag byte hands an attacker a forgery oracle. None of this has
//! been audited — see the caveats in [`crate::crypto`].

use alloc::vec::Vec;

use crate::selftest::Report;

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 12;
pub const TAG_LEN: usize = 16;

/// One ChaCha20 keystream block: sixteen 32-bit words.
const CHACHA_BLOCK_LEN: usize = 64;

/// Poly1305 consumes the message in 16-byte chunks.
const POLY1305_BLOCK_LEN: usize = 16;

/// Zeros to pad a partial Poly1305 block with; at most 15 are ever needed.
const ZEROS: [u8; POLY1305_BLOCK_LEN] = [0; POLY1305_BLOCK_LEN];

/// Read four bytes as a little-endian word. Panics if fewer than four are
/// available, so every call site indexes a fixed-size array.
fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

// ---------------------------------------------------------------------------
// ChaCha20
// ---------------------------------------------------------------------------

/// "expand 32-byte k", the first row of the ChaCha state.
const CHACHA_CONSTANTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

/// The ChaCha quarter round (RFC 8439 §2.1) applied to four state words.
///
/// Every addition here is modulo 2^32 by definition, hence `wrapping_add`.
fn quarter_round(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(12);
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_left(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_left(7);
}

/// The ChaCha20 block function (RFC 8439 §2.3): 20 rounds over the state built
/// from key, counter and nonce, added back to that starting state.
fn chacha20_block(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    counter: u32,
) -> [u8; CHACHA_BLOCK_LEN] {
    let mut initial = [0u32; 16];
    initial[0..4].copy_from_slice(&CHACHA_CONSTANTS);
    for i in 0..8 {
        initial[4 + i] = le_u32(&key[i * 4..]);
    }
    initial[12] = counter;
    for i in 0..3 {
        initial[13 + i] = le_u32(&nonce[i * 4..]);
    }

    // Ten "double rounds": four column rounds then four diagonal rounds.
    let mut state = initial;
    for _ in 0..10 {
        quarter_round(&mut state, 0, 4, 8, 12);
        quarter_round(&mut state, 1, 5, 9, 13);
        quarter_round(&mut state, 2, 6, 10, 14);
        quarter_round(&mut state, 3, 7, 11, 15);
        quarter_round(&mut state, 0, 5, 10, 15);
        quarter_round(&mut state, 1, 6, 11, 12);
        quarter_round(&mut state, 2, 7, 8, 13);
        quarter_round(&mut state, 3, 4, 9, 14);
    }

    // Adding the original state is what makes the round function non-invertible.
    let mut out = [0u8; CHACHA_BLOCK_LEN];
    for i in 0..16 {
        let word = state[i].wrapping_add(initial[i]);
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// XOR `data` with the ChaCha20 keystream starting at `counter`.
///
/// Encryption and decryption are the same operation. The counter is 32 bits, so
/// this covers 256 GiB under one nonce; a TLS record is at most 16 KiB plus
/// overhead, which is 257 blocks.
fn chacha20_xor(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], counter: u32, data: &mut [u8]) {
    let mut counter = counter;
    for chunk in data.chunks_mut(CHACHA_BLOCK_LEN) {
        let keystream = chacha20_block(key, nonce, counter);
        for (byte, key_byte) in chunk.iter_mut().zip(keystream.iter()) {
            *byte ^= *key_byte;
        }
        counter = counter.wrapping_add(1);
    }
}

// ---------------------------------------------------------------------------
// Poly1305
// ---------------------------------------------------------------------------

/// Poly1305 (RFC 8439 §2.5), accumulating a message given in arbitrary pieces.
///
/// The accumulator is 130 bits held as five 26-bit limbs in `u32`s. Splitting it
/// this way leaves each limb six spare bits, which is enough headroom that a
/// whole multiply-and-reduce step fits in `u64` intermediates without any limb
/// overflowing — see [`Poly1305::absorb`].
///
/// The AEAD feeds this the additional data, its padding, the ciphertext, its
/// padding and the two length words as six separate `update` calls, so that no
/// intermediate copy of the record has to be built.
struct Poly1305 {
    /// The clamped evaluation point, in 26-bit limbs.
    r: [u32; 5],
    /// The blinding half of the key, as four little-endian words.
    s: [u32; 4],
    /// The accumulator, in 26-bit limbs.
    h: [u32; 5],
    /// A partial block held back until 16 bytes are available.
    partial: [u8; POLY1305_BLOCK_LEN],
    partial_len: usize,
}

impl Poly1305 {
    /// Split a 32-byte one-time key into the clamped `r` and the addend `s`.
    ///
    /// RFC 8439 §2.5 requires `r`'s bytes 3, 7, 11 and 15 to have their top four
    /// bits clear and bytes 4, 8 and 12 their bottom two bits clear — that is,
    /// `r &= 0x0ffffffc0ffffffc0ffffffc0fffffff`. Loading `r` as five
    /// overlapping words lets the clamp and the split into 26-bit limbs happen
    /// in one masking step each.
    fn new(key: &[u8; 32]) -> Self {
        Poly1305 {
            r: [
                le_u32(&key[0..]) & 0x03ff_ffff,
                (le_u32(&key[3..]) >> 2) & 0x03ff_ff03,
                (le_u32(&key[6..]) >> 4) & 0x03ff_c0ff,
                (le_u32(&key[9..]) >> 6) & 0x03f0_3fff,
                (le_u32(&key[12..]) >> 8) & 0x000f_ffff,
            ],
            s: [
                le_u32(&key[16..]),
                le_u32(&key[20..]),
                le_u32(&key[24..]),
                le_u32(&key[28..]),
            ],
            h: [0; 5],
            partial: [0; POLY1305_BLOCK_LEN],
            partial_len: 0,
        }
    }

    /// Fold one 16-byte block into the accumulator: `h = (h + block) * r mod 2^130 - 5`.
    ///
    /// `high` supplies the 129th bit — `1 << 24` in limb 4, since that limb
    /// starts at bit 104. A full block always sets it; the final short block
    /// carries its own terminator byte instead and passes zero.
    ///
    /// The reduction uses that `2^130 = 5 mod 2^130 - 5`, so a term that
    /// overflows limb 4 comes back multiplied by five into limb 0.
    fn absorb(&mut self, block: [u8; POLY1305_BLOCK_LEN], high: u32) {
        // h += block. Each limb was left reduced below 2^26 (bar a stray carry
        // bit in limb 1) and gains at most 2^26 here, so nothing overflows.
        let h: [u64; 5] = [
            (self.h[0] + (le_u32(&block[0..]) & 0x03ff_ffff)) as u64,
            (self.h[1] + ((le_u32(&block[3..]) >> 2) & 0x03ff_ffff)) as u64,
            (self.h[2] + ((le_u32(&block[6..]) >> 4) & 0x03ff_ffff)) as u64,
            (self.h[3] + ((le_u32(&block[9..]) >> 6) & 0x03ff_ffff)) as u64,
            (self.h[4] + ((le_u32(&block[12..]) >> 8) | high)) as u64,
        ];

        let r: [u64; 5] = [
            self.r[0] as u64,
            self.r[1] as u64,
            self.r[2] as u64,
            self.r[3] as u64,
            self.r[4] as u64,
        ];
        // Coefficients for the terms that land above limb 4 and wrap back down.
        // Index 0 is never used; keeping it makes the indices below line up.
        let r5: [u64; 5] = [0, r[1] * 5, r[2] * 5, r[3] * 5, r[4] * 5];

        // h *= r. Term h[i] * r[j] belongs in limb i + j, and one with i + j > 4
        // wraps into limb i + j - 5 scaled by five. Every limb is under 2^27 and
        // every coefficient under 2^29, so each of these sums stays under 2^56.
        let d0 = h[0] * r[0] + h[1] * r5[4] + h[2] * r5[3] + h[3] * r5[2] + h[4] * r5[1];
        let mut d1 = h[0] * r[1] + h[1] * r[0] + h[2] * r5[4] + h[3] * r5[3] + h[4] * r5[2];
        let mut d2 = h[0] * r[2] + h[1] * r[1] + h[2] * r[0] + h[3] * r5[4] + h[4] * r5[3];
        let mut d3 = h[0] * r[3] + h[1] * r[2] + h[2] * r[1] + h[3] * r[0] + h[4] * r5[4];
        let mut d4 = h[0] * r[4] + h[1] * r[3] + h[2] * r[2] + h[3] * r[1] + h[4] * r[0];

        // Partial carry propagation: enough to get back to 26-bit limbs, not
        // enough to make the accumulator canonical. That happens once, in
        // `finish`.
        let mut carry = d0 >> 26;
        self.h[0] = (d0 as u32) & 0x03ff_ffff;
        d1 += carry;
        carry = d1 >> 26;
        self.h[1] = (d1 as u32) & 0x03ff_ffff;
        d2 += carry;
        carry = d2 >> 26;
        self.h[2] = (d2 as u32) & 0x03ff_ffff;
        d3 += carry;
        carry = d3 >> 26;
        self.h[3] = (d3 as u32) & 0x03ff_ffff;
        d4 += carry;
        carry = d4 >> 26;
        self.h[4] = (d4 as u32) & 0x03ff_ffff;
        // The carry out of the top limb re-enters the bottom one times five.
        self.h[0] += carry as u32 * 5;
        let carry = self.h[0] >> 26;
        self.h[0] &= 0x03ff_ffff;
        self.h[1] += carry;
    }

    /// Absorb as much of `data` as forms whole blocks, holding back the rest.
    fn update(&mut self, data: &[u8]) {
        let mut data = data;

        if self.partial_len > 0 {
            let take = core::cmp::min(POLY1305_BLOCK_LEN - self.partial_len, data.len());
            self.partial[self.partial_len..self.partial_len + take].copy_from_slice(&data[..take]);
            self.partial_len += take;
            data = &data[take..];
            if self.partial_len < POLY1305_BLOCK_LEN {
                return;
            }
            let block = self.partial;
            self.absorb(block, 1 << 24);
            self.partial_len = 0;
        }

        let mut blocks = data.chunks_exact(POLY1305_BLOCK_LEN);
        for chunk in &mut blocks {
            let mut block = [0u8; POLY1305_BLOCK_LEN];
            block.copy_from_slice(chunk);
            self.absorb(block, 1 << 24);
        }

        let rest = blocks.remainder();
        self.partial[..rest.len()].copy_from_slice(rest);
        self.partial_len = rest.len();
    }

    /// Finish the accumulator and return the 16-byte tag.
    fn finish(mut self) -> [u8; TAG_LEN] {
        // A short final block is terminated by a 0x01 byte in place of the
        // implicit 129th bit, then zero-filled.
        if self.partial_len > 0 {
            let end = self.partial_len;
            self.partial[end] = 1;
            for byte in self.partial[end + 1..].iter_mut() {
                *byte = 0;
            }
            let block = self.partial;
            self.absorb(block, 0);
        }

        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;

        // Carry fully, so every limb is below 2^26 and h is below 2^130.
        let mut carry = h1 >> 26;
        h1 &= 0x03ff_ffff;
        h2 += carry;
        carry = h2 >> 26;
        h2 &= 0x03ff_ffff;
        h3 += carry;
        carry = h3 >> 26;
        h3 &= 0x03ff_ffff;
        h4 += carry;
        carry = h4 >> 26;
        h4 &= 0x03ff_ffff;
        h0 += carry * 5;
        carry = h0 >> 26;
        h0 &= 0x03ff_ffff;
        h1 += carry;

        // h may still be one multiple of 2^130 - 5 above canonical, so compute
        // h + 5 - 2^130 and keep it if that did not go negative. Both results
        // are computed either way and selected by mask, so the timing does not
        // depend on which one wins.
        let mut g0 = h0 + 5;
        let mut carry = g0 >> 26;
        g0 &= 0x03ff_ffff;
        let mut g1 = h1 + carry;
        carry = g1 >> 26;
        g1 &= 0x03ff_ffff;
        let mut g2 = h2 + carry;
        carry = g2 >> 26;
        g2 &= 0x03ff_ffff;
        let mut g3 = h3 + carry;
        carry = g3 >> 26;
        g3 &= 0x03ff_ffff;
        let mut g4 = (h4 + carry).wrapping_sub(1 << 26);

        // Borrow out of g4 means h < 2^130 - 5 and h is already canonical.
        let mut mask = (g4 >> 31).wrapping_sub(1);
        g0 &= mask;
        g1 &= mask;
        g2 &= mask;
        g3 &= mask;
        g4 &= mask;
        mask = !mask;
        h0 = (h0 & mask) | g0;
        h1 = (h1 & mask) | g1;
        h2 = (h2 & mask) | g2;
        h3 = (h3 & mask) | g3;
        h4 = (h4 & mask) | g4;

        // Repack the 26-bit limbs into four 32-bit words, discarding above 2^128.
        let w0 = h0 | (h1 << 26);
        let w1 = (h1 >> 6) | (h2 << 20);
        let w2 = (h2 >> 12) | (h3 << 14);
        let w3 = (h3 >> 18) | (h4 << 8);

        // tag = (h + s) mod 2^128, carried through a u64 a word at a time.
        let mut tag = [0u8; TAG_LEN];
        let mut sum = w0 as u64 + self.s[0] as u64;
        tag[0..4].copy_from_slice(&(sum as u32).to_le_bytes());
        sum = w1 as u64 + self.s[1] as u64 + (sum >> 32);
        tag[4..8].copy_from_slice(&(sum as u32).to_le_bytes());
        sum = w2 as u64 + self.s[2] as u64 + (sum >> 32);
        tag[8..12].copy_from_slice(&(sum as u32).to_le_bytes());
        sum = w3 as u64 + self.s[3] as u64 + (sum >> 32);
        tag[12..16].copy_from_slice(&(sum as u32).to_le_bytes());
        tag
    }
}

/// Authenticate `message` under a 32-byte one-time key.
fn poly1305_mac(key: &[u8; 32], message: &[u8]) -> [u8; TAG_LEN] {
    let mut mac = Poly1305::new(key);
    mac.update(message);
    mac.finish()
}

// ---------------------------------------------------------------------------
// The AEAD
// ---------------------------------------------------------------------------

/// Zeros needed to round `len` up to a whole Poly1305 block.
fn padding_len(len: usize) -> usize {
    (POLY1305_BLOCK_LEN - (len % POLY1305_BLOCK_LEN)) % POLY1305_BLOCK_LEN
}

/// Derive the Poly1305 one-time key for this nonce (RFC 8439 §2.6).
///
/// It is the first 32 bytes of the counter-**0** keystream block. The payload
/// then starts at counter 1, so no keystream byte is ever used twice.
fn one_time_key(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN]) -> [u8; 32] {
    let block = chacha20_block(key, nonce, 0);
    let mut out = [0u8; 32];
    out.copy_from_slice(&block[..32]);
    out
}

/// The tag over the AEAD's authenticated input (RFC 8439 §2.8): the additional
/// data padded to a block boundary, the ciphertext padded likewise, then both
/// lengths as little-endian `u64`s.
///
/// The lengths are what stop an attacker moving the boundary between the
/// additional data and the ciphertext, which the padding alone would allow.
fn compute_tag(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> [u8; TAG_LEN] {
    let mut mac = Poly1305::new(&one_time_key(key, nonce));
    mac.update(aad);
    mac.update(&ZEROS[..padding_len(aad.len())]);
    mac.update(ciphertext);
    mac.update(&ZEROS[..padding_len(ciphertext.len())]);
    mac.update(&(aad.len() as u64).to_le_bytes());
    mac.update(&(ciphertext.len() as u64).to_le_bytes());
    mac.finish()
}

/// Encrypt in place and append the 16-byte tag.
///
/// `buffer` arrives holding the plaintext and comes back holding the ciphertext
/// followed by the tag, which is how TLS wants to build a record.
///
/// The `(key, nonce)` pair must never repeat. TLS 1.3 guarantees that by
/// deriving the nonce from a per-connection record sequence number.
pub fn seal(key: &[u8; KEY_LEN], nonce: &[u8; NONCE_LEN], aad: &[u8], buffer: &mut Vec<u8>) {
    chacha20_xor(key, nonce, 1, buffer);
    let tag = compute_tag(key, nonce, aad, buffer);
    buffer.extend_from_slice(&tag);
}

/// Verify and decrypt in place, truncating away the tag.
///
/// Returns `Err` and leaves `buffer` still holding the untouched ciphertext if
/// the tag does not match. The caller must treat a failure as fatal to the
/// connection: under TLS 1.3 there is no legitimate way for a record to fail
/// authentication, so one that does is either corruption or an attack, and
/// either way the key schedule cannot continue.
pub fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    buffer: &mut Vec<u8>,
) -> Result<(), &'static str> {
    if buffer.len() < TAG_LEN {
        return Err("chacha20poly1305: record is shorter than its tag");
    }
    let ciphertext_len = buffer.len() - TAG_LEN;

    let expected = compute_tag(key, nonce, aad, &buffer[..ciphertext_len]);

    // Constant time: accumulate every byte's difference and test once. An early
    // return on the first mismatch would let an attacker guess a forged tag one
    // byte at a time, which is 16 * 256 tries instead of 2^128.
    let mut difference = 0u8;
    for (byte, expected_byte) in buffer[ciphertext_len..].iter().zip(expected.iter()) {
        difference |= byte ^ expected_byte;
    }
    if difference != 0 {
        return Err("chacha20poly1305: tag mismatch");
    }

    buffer.truncate(ciphertext_len);
    chacha20_xor(key, nonce, 1, buffer);
    Ok(())
}

// ---------------------------------------------------------------------------
// Self-tests
// ---------------------------------------------------------------------------

/// The plaintext RFC 8439 uses in §2.4.2 and §2.8.2.
const SUNSCREEN: &[u8] = b"Ladies and Gentlemen of the class of '99: If I could offer \
you only one tip for the future, sunscreen would be it.";

pub fn selftest() -> Report {
    let mut report = Report::new();

    chacha20_vectors(&mut report);
    poly1305_vectors(&mut report);
    poly1305_edge_cases(&mut report);
    aead_vectors(&mut report);
    aead_round_trips(&mut report);
    aead_rejections(&mut report);

    report
}

fn chacha20_vectors(report: &mut Report) {
    // RFC 8439 §2.3.2. XORing a zeroed block yields the keystream itself, so
    // this is the block function's published output verbatim.
    let key: [u8; KEY_LEN] = core::array::from_fn(|i| i as u8);
    let nonce: [u8; NONCE_LEN] = [0, 0, 0, 0x09, 0, 0, 0, 0x4A, 0, 0, 0, 0];
    let expected: [u8; 64] = [
        0x10, 0xF1, 0xE7, 0xE4, 0xD1, 0x3B, 0x59, 0x15, 0x50, 0x0F, 0xDD, 0x1F, 0xA3, 0x20, 0x71, 0xC4,
        0xC7, 0xD1, 0xF4, 0xC7, 0x33, 0xC0, 0x68, 0x03, 0x04, 0x22, 0xAA, 0x9A, 0xC3, 0xD4, 0x6C, 0x4E,
        0xD2, 0x82, 0x64, 0x46, 0x07, 0x9F, 0xAA, 0x09, 0x14, 0xC2, 0xD7, 0x05, 0xD9, 0x8B, 0x02, 0xA2,
        0xB5, 0x12, 0x9C, 0xD1, 0xDE, 0x16, 0x4E, 0xB9, 0xCB, 0xD0, 0x83, 0xE8, 0xA2, 0x50, 0x3C, 0x4E,
    ];
    let mut block = [0u8; 64];
    chacha20_xor(&key, &nonce, 1, &mut block);
    report.check("chacha20 block function (rfc 8439 2.3.2)", block == expected);

    // RFC 8439 §2.4.2: the same key, nonce 00..4a.., counter 1, over 114 bytes —
    // one whole block and most of a second.
    let nonce: [u8; NONCE_LEN] = [0, 0, 0, 0, 0, 0, 0, 0x4A, 0, 0, 0, 0];
    let expected: [u8; 114] = [
        0x6E, 0x2E, 0x35, 0x9A, 0x25, 0x68, 0xF9, 0x80, 0x41, 0xBA, 0x07, 0x28, 0xDD, 0x0D, 0x69, 0x81,
        0xE9, 0x7E, 0x7A, 0xEC, 0x1D, 0x43, 0x60, 0xC2, 0x0A, 0x27, 0xAF, 0xCC, 0xFD, 0x9F, 0xAE, 0x0B,
        0xF9, 0x1B, 0x65, 0xC5, 0x52, 0x47, 0x33, 0xAB, 0x8F, 0x59, 0x3D, 0xAB, 0xCD, 0x62, 0xB3, 0x57,
        0x16, 0x39, 0xD6, 0x24, 0xE6, 0x51, 0x52, 0xAB, 0x8F, 0x53, 0x0C, 0x35, 0x9F, 0x08, 0x61, 0xD8,
        0x07, 0xCA, 0x0D, 0xBF, 0x50, 0x0D, 0x6A, 0x61, 0x56, 0xA3, 0x8E, 0x08, 0x8A, 0x22, 0xB6, 0x5E,
        0x52, 0xBC, 0x51, 0x4D, 0x16, 0xCC, 0xF8, 0x06, 0x81, 0x8C, 0xE9, 0x1A, 0xB7, 0x79, 0x37, 0x36,
        0x5A, 0xF9, 0x0B, 0xBF, 0x74, 0xA3, 0x5B, 0xE6, 0xB4, 0x0B, 0x8E, 0xED, 0xF2, 0x78, 0x5E, 0x42,
        0x87, 0x4D,
    ];
    let mut buffer = Vec::from(SUNSCREEN);
    chacha20_xor(&key, &nonce, 1, &mut buffer);
    report.check("chacha20 keystream (rfc 8439 2.4.2)", buffer.as_slice() == expected);

    chacha20_xor(&key, &nonce, 1, &mut buffer);
    report.check("chacha20 xor is its own inverse", buffer.as_slice() == SUNSCREEN);

    // Stopping one byte into the second block must use a truncated keystream,
    // not a whole one, so the prefix has to agree with the full vector.
    let mut short = Vec::from(&SUNSCREEN[..65]);
    chacha20_xor(&key, &nonce, 1, &mut short);
    report.check("chacha20 truncates the final block", short.as_slice() == &expected[..65]);
}

fn poly1305_vectors(report: &mut Report) {
    // RFC 8439 §2.5.2.
    let key: [u8; 32] = [
        0x85, 0xD6, 0xBE, 0x78, 0x57, 0x55, 0x6D, 0x33, 0x7F, 0x44, 0x52, 0xFE, 0x42, 0xD5, 0x06, 0xA8,
        0x01, 0x03, 0x80, 0x8A, 0xFB, 0x0D, 0xB2, 0xFD, 0x4A, 0xBF, 0xF6, 0xAF, 0x41, 0x49, 0xF5, 0x1B,
    ];
    let message = b"Cryptographic Forum Research Group";
    let expected: [u8; TAG_LEN] = [
        0xA8, 0x06, 0x1D, 0xC1, 0x30, 0x51, 0x36, 0xC6, 0xC2, 0x2B, 0x8B, 0xAF, 0x0C, 0x01, 0x27, 0xA9,
    ];
    report.check("poly1305 (rfc 8439 2.5.2)", poly1305_mac(&key, message) == expected);

    // Feeding the same message in ragged pieces — either side of a block
    // boundary and across two of them — must land on the same tag, since the
    // AEAD always calls `update` six times with lengths it does not control.
    let mut mac = Poly1305::new(&key);
    let pieces = [&message[..1], &message[1..6], &message[6..17], &message[17..18], &message[18..]];
    for piece in pieces {
        mac.update(piece);
    }
    report.check("poly1305 accepts a split message", mac.finish() == expected);

    // With no message the polynomial is empty and the tag is just s.
    let mut s = [0u8; TAG_LEN];
    s.copy_from_slice(&key[16..]);
    report.check("poly1305 of an empty message is s", poly1305_mac(&key, &[]) == s);
}

/// RFC 8439 §A.3, the eleven vectors aimed at the carry propagation and final
/// reduction of a hand-rolled 130-bit accumulator.
fn poly1305_edge_cases(report: &mut Report) {
    // Vectors #2 and #3 share this text; they differ in which half of the key
    // is zero.
    const IETF: &[u8] = b"Any submission to the IETF intended by the Contributor for \
publication as all or part of an IETF Internet-Draft or RFC and any statement made within \
the context of an IETF activity is considered an \"IETF Contribution\". Such statements \
include oral statements in IETF sessions, as well as written and electronic communications \
made at any time or place, which are addressed to";

    const JABBERWOCKY: &[u8] = b"'Twas brillig, and the slithy toves\nDid gyre and gimble \
in the wabe:\nAll mimsy were the borogoves,\nAnd the mome raths outgrabe.";

    let zero = [0u8; 32];
    report.check(
        "poly1305 a.3 #1 (zero key, zero message)",
        poly1305_mac(&zero, &[0u8; 64]) == [0u8; TAG_LEN],
    );

    let mut key = [0u8; 32];
    let s: [u8; TAG_LEN] = [
        0x36, 0xE5, 0xF6, 0xB5, 0xC5, 0xE0, 0x60, 0x70, 0xF0, 0xEF, 0xCA, 0x96, 0x22, 0x7A, 0x86, 0x3E,
    ];
    key[16..].copy_from_slice(&s);
    report.check("poly1305 a.3 #2 (r is zero)", poly1305_mac(&key, IETF) == s);

    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&s);
    let expected: [u8; TAG_LEN] = [
        0xF3, 0x47, 0x7E, 0x7C, 0xD9, 0x54, 0x17, 0xAF, 0x89, 0xA6, 0xB8, 0x79, 0x4C, 0x31, 0x0C, 0xF0,
    ];
    report.check("poly1305 a.3 #3 (s is zero)", poly1305_mac(&key, IETF) == expected);

    let key: [u8; 32] = [
        0x1C, 0x92, 0x40, 0xA5, 0xEB, 0x55, 0xD3, 0x8A, 0xF3, 0x33, 0x88, 0x86, 0x04, 0xF6, 0xB5, 0xF0,
        0x47, 0x39, 0x17, 0xC1, 0x40, 0x2B, 0x80, 0x09, 0x9D, 0xCA, 0x5C, 0xBC, 0x20, 0x70, 0x75, 0xC0,
    ];
    let expected: [u8; TAG_LEN] = [
        0x45, 0x41, 0x66, 0x9A, 0x7E, 0xAA, 0xEE, 0x61, 0xE7, 0x08, 0xDC, 0x7C, 0xBC, 0xC5, 0xEB, 0x62,
    ];
    report.check("poly1305 a.3 #4 (127-byte message)", poly1305_mac(&key, JABBERWOCKY) == expected);

    /// Build a one-time key from the `R` and `S` halves the RFC lists separately.
    fn key_from(r: [u8; 16], s: [u8; 16]) -> [u8; 32] {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(&r);
        key[16..].copy_from_slice(&s);
        key
    }

    const ONE: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const TWO: [u8; 16] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const THREE: [u8; 16] = [3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const ZERO16: [u8; 16] = [0; 16];
    const ONES: [u8; 16] = [0xFF; 16];

    // #5: is a partially reduced final result reduced the rest of the way?
    report.check(
        "poly1305 a.3 #5 (partial result not fully reduced)",
        poly1305_mac(&key_from(TWO, ZERO16), &ONES) == THREE,
    );

    // #6: does adding s overflow modulo 2^128 correctly?
    report.check(
        "poly1305 a.3 #6 (adding s overflows 2^128)",
        poly1305_mac(&key_from(TWO, ONES), &TWO) == THREE,
    );

    // #7: an all-ones limb with a carry coming in from below.
    let mut second = ONES;
    second[0] = 0xF0;
    let mut message = [0u8; 48];
    message[..16].copy_from_slice(&ONES);
    message[16..32].copy_from_slice(&second);
    message[32] = 0x11;
    let five: [u8; 16] = [5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    report.check(
        "poly1305 a.3 #7 (carry out of an all-ones limb)",
        poly1305_mac(&key_from(ONE, ZERO16), &message) == five,
    );

    // #8: the polynomial part lands on exactly 2^130 - 5, which reduces to zero.
    let mut second = [0xFEu8; 16];
    second[0] = 0xFB;
    let mut message = [0u8; 48];
    message[..16].copy_from_slice(&ONES);
    message[16..32].copy_from_slice(&second);
    message[32..].copy_from_slice(&[0x01; 16]);
    report.check(
        "poly1305 a.3 #8 (result is exactly 2^130-5)",
        poly1305_mac(&key_from(ONE, ZERO16), &message) == ZERO16,
    );

    // #9: one less than that, which must stay put.
    let mut message = ONES;
    message[0] = 0xFD;
    let mut expected = ONES;
    expected[0] = 0xFA;
    report.check(
        "poly1305 a.3 #9 (result is exactly 2^130-6)",
        poly1305_mac(&key_from(TWO, ZERO16), &message) == expected,
    );

    // #10 and #11: does folding the top of the product back down by five
    // produce a 131-bit intermediate, and then a 131-bit final result?
    let mut message = [0u8; 64];
    message[..16]
        .copy_from_slice(&[0xE3, 0x35, 0x94, 0xD7, 0x50, 0x5E, 0x43, 0xB9, 0, 0, 0, 0, 0, 0, 0, 0]);
    message[16..32]
        .copy_from_slice(&[0x33, 0x94, 0xD7, 0x50, 0x5E, 0x43, 0x79, 0xCD, 1, 0, 0, 0, 0, 0, 0, 0]);
    message[48] = 0x01;
    let r: [u8; 16] = [0x01, 0, 0, 0, 0, 0, 0, 0, 0x04, 0, 0, 0, 0, 0, 0, 0];
    let expected: [u8; 16] = [0x14, 0, 0, 0, 0, 0, 0, 0, 0x55, 0, 0, 0, 0, 0, 0, 0];
    report.check(
        "poly1305 a.3 #10 (131-bit intermediate)",
        poly1305_mac(&key_from(r, ZERO16), &message) == expected,
    );

    let mut expected = ZERO16;
    expected[0] = 0x13;
    report.check(
        "poly1305 a.3 #11 (131-bit final result)",
        poly1305_mac(&key_from(r, ZERO16), &message[..48]) == expected,
    );
}

/// The key, nonce, additional data, ciphertext and tag of RFC 8439 §2.8.2.
const AEAD_KEY: [u8; KEY_LEN] = [
    0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
];
const AEAD_NONCE: [u8; NONCE_LEN] = [0x07, 0, 0, 0, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47];
const AEAD_AAD: [u8; 12] = [0x50, 0x51, 0x52, 0x53, 0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7];

fn aead_vectors(report: &mut Report) {
    // The one-time key is the counter-0 block, distinct from the counter-1 block
    // the payload uses. Getting the counters the wrong way round still produces
    // a self-consistent AEAD that no peer can talk to.
    let expected: [u8; 32] = [
        0x7B, 0xAC, 0x2B, 0x25, 0x2D, 0xB4, 0x47, 0xAF, 0x09, 0xB6, 0x7A, 0x55, 0xA4, 0xE9, 0x55, 0x84,
        0x0A, 0xE1, 0xD6, 0x73, 0x10, 0x75, 0xD9, 0xEB, 0x2A, 0x93, 0x75, 0x78, 0x3E, 0xD5, 0x53, 0xFF,
    ];
    report.check(
        "aead one-time key from counter 0 (rfc 8439 2.8.2)",
        one_time_key(&AEAD_KEY, &AEAD_NONCE) == expected,
    );

    let ciphertext: [u8; 114] = [
        0xD3, 0x1A, 0x8D, 0x34, 0x64, 0x8E, 0x60, 0xDB, 0x7B, 0x86, 0xAF, 0xBC, 0x53, 0xEF, 0x7E, 0xC2,
        0xA4, 0xAD, 0xED, 0x51, 0x29, 0x6E, 0x08, 0xFE, 0xA9, 0xE2, 0xB5, 0xA7, 0x36, 0xEE, 0x62, 0xD6,
        0x3D, 0xBE, 0xA4, 0x5E, 0x8C, 0xA9, 0x67, 0x12, 0x82, 0xFA, 0xFB, 0x69, 0xDA, 0x92, 0x72, 0x8B,
        0x1A, 0x71, 0xDE, 0x0A, 0x9E, 0x06, 0x0B, 0x29, 0x05, 0xD6, 0xA5, 0xB6, 0x7E, 0xCD, 0x3B, 0x36,
        0x92, 0xDD, 0xBD, 0x7F, 0x2D, 0x77, 0x8B, 0x8C, 0x98, 0x03, 0xAE, 0xE3, 0x28, 0x09, 0x1B, 0x58,
        0xFA, 0xB3, 0x24, 0xE4, 0xFA, 0xD6, 0x75, 0x94, 0x55, 0x85, 0x80, 0x8B, 0x48, 0x31, 0xD7, 0xBC,
        0x3F, 0xF4, 0xDE, 0xF0, 0x8E, 0x4B, 0x7A, 0x9D, 0xE5, 0x76, 0xD2, 0x65, 0x86, 0xCE, 0xC6, 0x4B,
        0x61, 0x16,
    ];
    let expected_tag: [u8; TAG_LEN] = [
        0x1A, 0xE1, 0x0B, 0x59, 0x4F, 0x09, 0xE2, 0x6A, 0x7E, 0x90, 0x2E, 0xCB, 0xD0, 0x60, 0x06, 0x91,
    ];

    let mut buffer = Vec::from(SUNSCREEN);
    seal(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer);
    report.check(
        "aead seal ciphertext (rfc 8439 2.8.2)",
        buffer.len() == SUNSCREEN.len() + TAG_LEN && buffer[..SUNSCREEN.len()] == ciphertext,
    );
    report.check(
        "aead seal tag (rfc 8439 2.8.2)",
        buffer[SUNSCREEN.len()..] == expected_tag,
    );

    let opened = open(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer);
    report.check(
        "aead open recovers the plaintext (rfc 8439 2.8.2)",
        opened.is_ok() && buffer.as_slice() == SUNSCREEN,
    );

    aead_decryption_vector(report);
}

/// RFC 8439 §A.5, a decryption worked through with a 265-byte record — four
/// whole ChaCha blocks and a nine-byte tail.
fn aead_decryption_vector(report: &mut Report) {
    let key: [u8; KEY_LEN] = [
        0x1C, 0x92, 0x40, 0xA5, 0xEB, 0x55, 0xD3, 0x8A, 0xF3, 0x33, 0x88, 0x86, 0x04, 0xF6, 0xB5, 0xF0,
        0x47, 0x39, 0x17, 0xC1, 0x40, 0x2B, 0x80, 0x09, 0x9D, 0xCA, 0x5C, 0xBC, 0x20, 0x70, 0x75, 0xC0,
    ];
    let nonce: [u8; NONCE_LEN] = [0, 0, 0, 0, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let aad: [u8; 12] = [0xF3, 0x33, 0x88, 0x86, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4E, 0x91];
    let ciphertext: [u8; 265] = [
        0x64, 0xA0, 0x86, 0x15, 0x75, 0x86, 0x1A, 0xF4, 0x60, 0xF0, 0x62, 0xC7, 0x9B, 0xE6, 0x43, 0xBD,
        0x5E, 0x80, 0x5C, 0xFD, 0x34, 0x5C, 0xF3, 0x89, 0xF1, 0x08, 0x67, 0x0A, 0xC7, 0x6C, 0x8C, 0xB2,
        0x4C, 0x6C, 0xFC, 0x18, 0x75, 0x5D, 0x43, 0xEE, 0xA0, 0x9E, 0xE9, 0x4E, 0x38, 0x2D, 0x26, 0xB0,
        0xBD, 0xB7, 0xB7, 0x3C, 0x32, 0x1B, 0x01, 0x00, 0xD4, 0xF0, 0x3B, 0x7F, 0x35, 0x58, 0x94, 0xCF,
        0x33, 0x2F, 0x83, 0x0E, 0x71, 0x0B, 0x97, 0xCE, 0x98, 0xC8, 0xA8, 0x4A, 0xBD, 0x0B, 0x94, 0x81,
        0x14, 0xAD, 0x17, 0x6E, 0x00, 0x8D, 0x33, 0xBD, 0x60, 0xF9, 0x82, 0xB1, 0xFF, 0x37, 0xC8, 0x55,
        0x97, 0x97, 0xA0, 0x6E, 0xF4, 0xF0, 0xEF, 0x61, 0xC1, 0x86, 0x32, 0x4E, 0x2B, 0x35, 0x06, 0x38,
        0x36, 0x06, 0x90, 0x7B, 0x6A, 0x7C, 0x02, 0xB0, 0xF9, 0xF6, 0x15, 0x7B, 0x53, 0xC8, 0x67, 0xE4,
        0xB9, 0x16, 0x6C, 0x76, 0x7B, 0x80, 0x4D, 0x46, 0xA5, 0x9B, 0x52, 0x16, 0xCD, 0xE7, 0xA4, 0xE9,
        0x90, 0x40, 0xC5, 0xA4, 0x04, 0x33, 0x22, 0x5E, 0xE2, 0x82, 0xA1, 0xB0, 0xA0, 0x6C, 0x52, 0x3E,
        0xAF, 0x45, 0x34, 0xD7, 0xF8, 0x3F, 0xA1, 0x15, 0x5B, 0x00, 0x47, 0x71, 0x8C, 0xBC, 0x54, 0x6A,
        0x0D, 0x07, 0x2B, 0x04, 0xB3, 0x56, 0x4E, 0xEA, 0x1B, 0x42, 0x22, 0x73, 0xF5, 0x48, 0x27, 0x1A,
        0x0B, 0xB2, 0x31, 0x60, 0x53, 0xFA, 0x76, 0x99, 0x19, 0x55, 0xEB, 0xD6, 0x31, 0x59, 0x43, 0x4E,
        0xCE, 0xBB, 0x4E, 0x46, 0x6D, 0xAE, 0x5A, 0x10, 0x73, 0xA6, 0x72, 0x76, 0x27, 0x09, 0x7A, 0x10,
        0x49, 0xE6, 0x17, 0xD9, 0x1D, 0x36, 0x10, 0x94, 0xFA, 0x68, 0xF0, 0xFF, 0x77, 0x98, 0x71, 0x30,
        0x30, 0x5B, 0xEA, 0xBA, 0x2E, 0xDA, 0x04, 0xDF, 0x99, 0x7B, 0x71, 0x4D, 0x6C, 0x6F, 0x2C, 0x29,
        0xA6, 0xAD, 0x5C, 0xB4, 0x02, 0x2B, 0x02, 0x70, 0x9B,
    ];
    let tag: [u8; TAG_LEN] = [
        0xEE, 0xAD, 0x9D, 0x67, 0x89, 0x0C, 0xBB, 0x22, 0x39, 0x23, 0x36, 0xFE, 0xA1, 0x85, 0x1F, 0x38,
    ];
    // The RFC's plaintext quotes "work in progress" with a stray slash before
    // each curly quote; the bytes are what they are.
    let plaintext: &[u8] = b"Internet-Drafts are draft documents valid for a maximum of six \
months and may be updated, replaced, or obsoleted by other documents at any time. It is \
inappropriate to use Internet-Drafts as reference material or to cite them other than as \
/\xE2\x80\x9Cwork in progress./\xE2\x80\x9D";

    let mut buffer = Vec::with_capacity(ciphertext.len() + TAG_LEN);
    buffer.extend_from_slice(&ciphertext);
    buffer.extend_from_slice(&tag);
    let opened = open(&key, &nonce, &aad, &mut buffer);
    report.check(
        "aead open (rfc 8439 a.5)",
        opened.is_ok() && buffer.as_slice() == plaintext,
    );
}

fn aead_round_trips(report: &mut Report) {
    // Lengths either side of the 64-byte ChaCha block and the 16-byte Poly1305
    // block, plus one that needs several of both.
    let mut all_ok = true;
    for length in [0usize, 1, 15, 16, 17, 63, 64, 65, 200] {
        let plaintext: Vec<u8> = (0..length).map(|i| (i * 7 + 3) as u8).collect();
        let mut buffer = plaintext.clone();
        seal(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer);
        if buffer.len() != length + TAG_LEN {
            all_ok = false;
            continue;
        }
        match open(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer) {
            Ok(()) => all_ok &= buffer == plaintext,
            Err(_) => all_ok = false,
        }
    }
    report.check("aead round-trips across the block boundaries", all_ok);

    // An empty record with no additional data is all tag, and TLS 1.3 does send
    // such records.
    let mut buffer = Vec::new();
    seal(&AEAD_KEY, &AEAD_NONCE, &[], &mut buffer);
    let sealed_len = buffer.len();
    let opened = open(&AEAD_KEY, &AEAD_NONCE, &[], &mut buffer);
    report.check(
        "aead seals an empty plaintext with empty aad",
        sealed_len == TAG_LEN && opened.is_ok() && buffer.is_empty(),
    );
}

fn aead_rejections(report: &mut Report) {
    let plaintext: [u8; 40] = core::array::from_fn(|i| (i as u8) ^ 0x5A);
    let sealed = {
        let mut buffer = Vec::from(&plaintext[..]);
        seal(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer);
        buffer
    };

    // Every single-bit change to the record — ciphertext or tag — must fail.
    let mut all_rejected = true;
    for index in 0..sealed.len() {
        for bit in 0..8 {
            let mut buffer = sealed.clone();
            buffer[index] ^= 1u8 << bit;
            all_rejected &= open(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer).is_err();
        }
    }
    report.check("aead open rejects any flipped ciphertext or tag bit", all_rejected);

    // The additional data is authenticated but not encrypted, so a change there
    // has to be caught too.
    let mut all_rejected = true;
    for index in 0..AEAD_AAD.len() {
        for bit in 0..8 {
            let mut aad = AEAD_AAD;
            aad[index] ^= 1u8 << bit;
            let mut buffer = sealed.clone();
            all_rejected &= open(&AEAD_KEY, &AEAD_NONCE, &aad, &mut buffer).is_err();
        }
    }
    report.check("aead open rejects any flipped aad bit", all_rejected);

    // Anything too short to hold a tag has to be an error rather than a panic;
    // `open` is fed whatever arrives off the wire.
    let mut all_rejected = true;
    for length in 0..TAG_LEN {
        let mut buffer = Vec::from(&sealed[..length]);
        all_rejected &= open(&AEAD_KEY, &AEAD_NONCE, &AEAD_AAD, &mut buffer).is_err();
    }
    report.check("aead open rejects a record shorter than the tag", all_rejected);

    // A record whose tag is right for a different nonce must not open either.
    let mut nonce = AEAD_NONCE;
    nonce[0] ^= 1;
    let mut buffer = sealed.clone();
    report.check(
        "aead open rejects the wrong nonce",
        open(&AEAD_KEY, &nonce, &AEAD_AAD, &mut buffer).is_err(),
    );
}
