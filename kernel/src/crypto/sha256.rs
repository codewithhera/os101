//! SHA-256, as specified in FIPS 180-4.
//!
//! TLS 1.3 asks for this hash in three places: the handshake transcript hash,
//! the core of [`super::hmac`] and therefore of [`super::hkdf`], and folding
//! an over-long HMAC key down to one block. The transcript is what shapes the
//! API. A client has to hash "every handshake message so far" at several
//! points — after ServerHello to derive handshake traffic keys, again before
//! Finished — and keep appending afterwards, so [`Sha256::finish`] borrows
//! instead of consuming and the state is `Clone`.
//!
//! There are no lookup tables here, so there is nothing for the data cache to
//! leak. That falls out of SHA-256 being pure shifts, xors and additions
//! rather than from any precaution on our part.

/// Bytes in a SHA-256 digest.
pub const DIGEST_LEN: usize = 32;

/// Bytes in a SHA-256 compression block. HMAC's key padding is defined in
/// terms of the block size, so it is part of the interface.
pub const BLOCK_LEN: usize = 64;

/// The initial hash value: the first thirty-two bits of the fractional parts
/// of the square roots of the first eight primes (FIPS 180-4 §5.3.3).
const INITIAL: [u32; 8] = [
    0x6a09_e667, 0xbb67_ae85, 0x3c6e_f372, 0xa54f_f53a,
    0x510e_527f, 0x9b05_688c, 0x1f83_d9ab, 0x5be0_cd19,
];

/// Round constants: the first thirty-two bits of the fractional parts of the
/// cube roots of the first sixty-four primes (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a_2f98, 0x7137_4491, 0xb5c0_fbcf, 0xe9b5_dba5,
    0x3956_c25b, 0x59f1_11f1, 0x923f_82a4, 0xab1c_5ed5,
    0xd807_aa98, 0x1283_5b01, 0x2431_85be, 0x550c_7dc3,
    0x72be_5d74, 0x80de_b1fe, 0x9bdc_06a7, 0xc19b_f174,
    0xe49b_69c1, 0xefbe_4786, 0x0fc1_9dc6, 0x240c_a1cc,
    0x2de9_2c6f, 0x4a74_84aa, 0x5cb0_a9dc, 0x76f9_88da,
    0x983e_5152, 0xa831_c66d, 0xb003_27c8, 0xbf59_7fc7,
    0xc6e0_0bf3, 0xd5a7_9147, 0x06ca_6351, 0x1429_2967,
    0x27b7_0a85, 0x2e1b_2138, 0x4d2c_6dfc, 0x5338_0d13,
    0x650a_7354, 0x766a_0abb, 0x81c2_c92e, 0x9272_2c85,
    0xa2bf_e8a1, 0xa81a_664b, 0xc24b_8b70, 0xc76c_51a3,
    0xd192_e819, 0xd699_0624, 0xf40e_3585, 0x106a_a070,
    0x19a4_c116, 0x1e37_6c08, 0x2748_774c, 0x34b0_bcb5,
    0x391c_0cb3, 0x4ed8_aa4a, 0x5b9c_ca4f, 0x682e_6ff3,
    0x748f_82ee, 0x78a5_636f, 0x84c8_7814, 0x8cc7_0208,
    0x90be_fffa, 0xa450_6ceb, 0xbef9_a3f7, 0xc671_78f2,
];

/// An incremental SHA-256.
///
/// Feed it with [`Sha256::update`] as often as you like and read the digest
/// with [`Sha256::finish`], which leaves the state alone so the same running
/// hash can be read again later.
#[derive(Clone)]
pub struct Sha256 {
    /// The eight working words, advanced once per complete block.
    state: [u32; 8],
    /// Bytes accepted but not yet part of a complete block.
    block: [u8; BLOCK_LEN],
    /// How much of `block` is filled. Always less than `BLOCK_LEN`: a block is
    /// compressed and the buffer emptied the moment it is full.
    filled: usize,
    /// Every byte ever fed in. The padding encodes this as a bit count.
    total: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 { state: INITIAL, block: [0; BLOCK_LEN], filled: 0, total: 0 }
    }

    pub fn update(&mut self, data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);

        // Finish off a partial block first. If `data` cannot fill it there is
        // nothing else to do; if it can, the buffer is empty from here on.
        let mut data = data;
        if self.filled > 0 {
            let want = BLOCK_LEN - self.filled;
            let take = if data.len() < want { data.len() } else { want };
            self.block[self.filled..self.filled + take].copy_from_slice(&data[..take]);
            self.filled += take;
            data = &data[take..];
            if self.filled < BLOCK_LEN {
                return;
            }
            compress(&mut self.state, &self.block);
            self.filled = 0;
        }

        let mut blocks = data.chunks_exact(BLOCK_LEN);
        for chunk in &mut blocks {
            let mut block = [0u8; BLOCK_LEN];
            block.copy_from_slice(chunk);
            compress(&mut self.state, &block);
        }

        let tail = blocks.remainder();
        self.block[..tail.len()].copy_from_slice(tail);
        self.filled = tail.len();
    }

    /// The digest of everything fed in so far.
    ///
    /// Takes `&self`, so a transcript hash can be read at several points in a
    /// handshake and still be appended to afterwards. The padding is applied
    /// to copies of the state and the pending block.
    pub fn finish(&self) -> [u8; DIGEST_LEN] {
        let mut state = self.state;
        let mut block = self.block;

        // Padding is a single 1 bit, then zeros, then the message length in
        // bits as a big-endian u64 (FIPS 180-4 §5.1.1). `filled` is at most 63,
        // so the 1 bit always fits in the block already buffered.
        let mut end = self.filled;
        block[end] = 0x80;
        end += 1;

        // If the length no longer fits after that bit, the zeros run to the
        // end of this block and the length goes in one of its own.
        if end > BLOCK_LEN - 8 {
            for byte in &mut block[end..] {
                *byte = 0;
            }
            compress(&mut state, &block);
            end = 0;
        }
        for byte in &mut block[end..BLOCK_LEN - 8] {
            *byte = 0;
        }
        block[BLOCK_LEN - 8..].copy_from_slice(&self.total.wrapping_mul(8).to_be_bytes());
        compress(&mut state, &block);

        let mut digest = [0u8; DIGEST_LEN];
        for (word, out) in state.iter().zip(digest.chunks_exact_mut(4)) {
            out.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }
}

/// The digest of `data` in one call, for callers holding the whole message.
pub fn digest(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

/// Mix one full block into `state` (FIPS 180-4 §6.2.2).
///
/// Every addition here is modulo 2^32 by definition, hence `wrapping_add`
/// throughout rather than relying on release-mode wrapping.
fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_LEN]) {
    let mut w = [0u32; 64];
    for (word, chunk) in w[..16].iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(choose)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *word = word.wrapping_add(add);
    }
}

/// The FIPS 180-4 example digests, plus the incremental behaviour the TLS
/// transcript hash depends on.
///
/// The digests marked as generated are not from a standards document — no
/// standard publishes a vector for a 63-byte message — and were produced with
/// OpenSSL (via Python's `hashlib`) rather than with this code. They are here
/// because the length padding is what a hand-written SHA-256 gets wrong, and
/// it goes wrong at exactly 55/56/57 and 63/64/65 bytes of input.
pub fn selftest() -> crate::selftest::Report {
    /// FIPS 180-4 §D.2's two-block example.
    const MSG_56: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    /// FIPS 180-4 §D.3's longer example.
    const MSG_112: &[u8] = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmn\
                             hijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
    /// RFC 6234 §8.5 TEST4, ten copies of this sixty-four byte run.
    const MSG_640: &[u8] = b"0123456701234567012345670123456701234567012345670123456701234567";

    const EMPTY: [u8; DIGEST_LEN] = [
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
        0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
        0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
        0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
    ];
    const ABC: [u8; DIGEST_LEN] = [
        0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea,
        0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
        0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c,
        0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
    ];
    const OF_56: [u8; DIGEST_LEN] = [
        0x24, 0x8d, 0x6a, 0x61, 0xd2, 0x06, 0x38, 0xb8,
        0xe5, 0xc0, 0x26, 0x93, 0x0c, 0x3e, 0x60, 0x39,
        0xa3, 0x3c, 0xe4, 0x59, 0x64, 0xff, 0x21, 0x67,
        0xf6, 0xec, 0xed, 0xd4, 0x19, 0xdb, 0x06, 0xc1,
    ];
    const OF_112: [u8; DIGEST_LEN] = [
        0xcf, 0x5b, 0x16, 0xa7, 0x78, 0xaf, 0x83, 0x80,
        0x03, 0x6c, 0xe5, 0x9e, 0x7b, 0x04, 0x92, 0x37,
        0x0b, 0x24, 0x9b, 0x11, 0xe8, 0xf0, 0x7a, 0x51,
        0xaf, 0xac, 0x45, 0x03, 0x7a, 0xfe, 0xe9, 0xd1,
    ];
    const OF_MILLION: [u8; DIGEST_LEN] = [
        0xcd, 0xc7, 0x6e, 0x5c, 0x99, 0x14, 0xfb, 0x92,
        0x81, 0xa1, 0xc7, 0xe2, 0x84, 0xd7, 0x3e, 0x67,
        0xf1, 0x80, 0x9a, 0x48, 0xa4, 0x97, 0x20, 0x0e,
        0x04, 0x6d, 0x39, 0xcc, 0xc7, 0x11, 0x2c, 0xd0,
    ];
    const OF_640: [u8; DIGEST_LEN] = [
        0x59, 0x48, 0x47, 0x32, 0x84, 0x51, 0xbd, 0xfa,
        0x85, 0x05, 0x62, 0x25, 0x46, 0x2c, 0xc1, 0xd8,
        0x67, 0xd8, 0x77, 0xfb, 0x38, 0x8d, 0xf0, 0xce,
        0x35, 0xf2, 0x5a, 0xb5, 0x56, 0x2b, 0xfb, 0xb5,
    ];

    // Generated: 0x00, 0x01, 0x02, ... truncated to each awkward length.
    const COUNTING_55: [u8; DIGEST_LEN] = [
        0x46, 0x3e, 0xb2, 0x8e, 0x72, 0xf8, 0x2e, 0x0a,
        0x96, 0xc0, 0xa4, 0xcc, 0x53, 0x69, 0x0c, 0x57,
        0x12, 0x81, 0x13, 0x1f, 0x67, 0x2a, 0xa2, 0x29,
        0xe0, 0xd4, 0x5a, 0xe5, 0x9b, 0x59, 0x8b, 0x59,
    ];
    const COUNTING_56: [u8; DIGEST_LEN] = [
        0xda, 0x2a, 0xe4, 0xd6, 0xb3, 0x67, 0x48, 0xf2,
        0xa3, 0x18, 0xf2, 0x3e, 0x7a, 0xb1, 0xdf, 0xdf,
        0x45, 0xac, 0xdc, 0x9d, 0x04, 0x9b, 0xd8, 0x0e,
        0x59, 0xde, 0x82, 0xa6, 0x08, 0x95, 0xf5, 0x62,
    ];
    const COUNTING_57: [u8; DIGEST_LEN] = [
        0x2f, 0xe7, 0x41, 0xaf, 0x80, 0x1c, 0xc2, 0x38,
        0x60, 0x2a, 0xc0, 0xec, 0x6a, 0x7b, 0x0c, 0x3a,
        0x8a, 0x87, 0xc7, 0xfc, 0x7d, 0x7f, 0x02, 0xa3,
        0xfe, 0x03, 0xd1, 0xc1, 0x2e, 0xac, 0x4d, 0x8f,
    ];
    const COUNTING_63: [u8; DIGEST_LEN] = [
        0x29, 0xaf, 0x26, 0x86, 0xfd, 0x53, 0x37, 0x4a,
        0x36, 0xb0, 0x84, 0x66, 0x94, 0xcc, 0x34, 0x21,
        0x77, 0xe4, 0x28, 0xd1, 0x64, 0x75, 0x15, 0xf0,
        0x78, 0x78, 0x4d, 0x69, 0xcd, 0xb9, 0xe4, 0x88,
    ];
    const COUNTING_64: [u8; DIGEST_LEN] = [
        0xfd, 0xea, 0xb9, 0xac, 0xf3, 0x71, 0x03, 0x62,
        0xbd, 0x26, 0x58, 0xcd, 0xc9, 0xa2, 0x9e, 0x8f,
        0x9c, 0x75, 0x7f, 0xcf, 0x98, 0x11, 0x60, 0x3a,
        0x8c, 0x44, 0x7c, 0xd1, 0xd9, 0x15, 0x11, 0x08,
    ];
    const COUNTING_65: [u8; DIGEST_LEN] = [
        0x4b, 0xfd, 0x2c, 0x8b, 0x6f, 0x1e, 0xec, 0x7a,
        0x2a, 0xfe, 0xb4, 0x8b, 0x93, 0x4e, 0xe4, 0xb2,
        0x69, 0x41, 0x82, 0x02, 0x7e, 0x6d, 0x0f, 0xc0,
        0x75, 0x07, 0x4f, 0x2f, 0xab, 0xb3, 0x17, 0x81,
    ];

    let mut report = crate::selftest::Report::new();

    report.check("digest of the empty string", digest(b"") == EMPTY);
    report.check("digest of abc", digest(b"abc") == ABC);
    report.check("digest of the 56-byte example", digest(MSG_56) == OF_56);
    report.check("digest of the 112-byte example", digest(MSG_112) == OF_112);

    // Ten updates of a whole block each, which is the aligned fast path.
    let mut aligned = Sha256::new();
    for _ in 0..10 {
        aligned.update(MSG_640);
    }
    report.check("digest of the 640-byte example", aligned.finish() == OF_640);

    // FIPS 180-4's million-character example, fed a thousand bytes at a time
    // so the buffer never sees a block-aligned update. Around 15600
    // compressions, which is a few milliseconds even under emulation.
    let mut millions = Sha256::new();
    let thousand_a = [b'a'; 1000];
    for _ in 0..1000 {
        millions.update(&thousand_a);
    }
    report.check("digest of one million a", millions.finish() == OF_MILLION);

    // Lengths either side of both padding thresholds: 56 is where the length
    // no longer fits beside the message, 64 is where the buffer turns over.
    let mut counting = [0u8; 65];
    for (i, byte) in counting.iter_mut().enumerate() {
        *byte = i as u8;
    }
    report.check("digest of 55 bytes", digest(&counting[..55]) == COUNTING_55);
    report.check("digest of 56 bytes", digest(&counting[..56]) == COUNTING_56);
    report.check("digest of 57 bytes", digest(&counting[..57]) == COUNTING_57);
    report.check("digest of 63 bytes", digest(&counting[..63]) == COUNTING_63);
    report.check("digest of 64 bytes", digest(&counting[..64]) == COUNTING_64);
    report.check("digest of 65 bytes", digest(&counting[..65]) == COUNTING_65);

    // A byte at a time, checked against the published digest rather than
    // against our own one-shot path, so both cannot be wrong together.
    let mut dribbled = Sha256::new();
    for byte in MSG_112 {
        dribbled.update(&[*byte]);
    }
    report.check("a byte at a time matches the published digest", dribbled.finish() == OF_112);

    // Chunk sizes that straddle the block boundary. These sum to 320, so the
    // last update is an empty slice, which must also be harmless.
    let mut straddling = [0u8; 320];
    for (i, byte) in straddling.iter_mut().enumerate() {
        *byte = (i * 7 + 1) as u8;
    }
    let mut chunked = Sha256::new();
    let mut pending = &straddling[..];
    for size in [1usize, 63, 64, 65, 1, 62, 64] {
        let take = if size < pending.len() { size } else { pending.len() };
        chunked.update(&pending[..take]);
        pending = &pending[take..];
    }
    chunked.update(pending);
    report.check("odd-sized chunks match one shot", chunked.finish() == digest(&straddling));

    let mut twice = Sha256::new();
    twice.update(b"abc");
    let first = twice.finish();
    report.check("finish is repeatable", first == ABC && twice.finish() == ABC);

    // What the transcript hash actually does: read the running hash, then keep
    // appending to it.
    let mut transcript = Sha256::new();
    transcript.update(b"abc");
    let midway = transcript.finish();
    transcript.update(MSG_112);
    let mut joined = [0u8; 3 + 112];
    joined[..3].copy_from_slice(b"abc");
    joined[3..].copy_from_slice(MSG_112);
    report.check(
        "finishing midway leaves the state alone",
        midway == ABC && transcript.finish() == digest(&joined),
    );

    // And what a clone is for: binding a signature to the transcript so far
    // without disturbing the copy that carries on.
    let mut trunk = Sha256::new();
    trunk.update(b"ab");
    let mut branch = trunk.clone();
    trunk.update(b"c");
    branch.update(b"cd");
    report.check(
        "a clone forks the transcript",
        trunk.finish() == ABC && branch.finish() == digest(b"abcd"),
    );

    report
}
