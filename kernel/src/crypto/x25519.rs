//! X25519 — the Diffie-Hellman function of RFC 7748, over Curve25519.
//!
//! # What the handshake needs
//!
//! TLS 1.3 opens with a key share: the client picks a random 32-byte scalar,
//! puts [`public_key`] of it in the ClientHello, and when the server's own
//! public key comes back combines the two with [`scalar_mult`]. Both ends
//! arrive at the same 32 bytes, and every traffic key is derived from them.
//! That is the whole interface — no point type, no encoding to get wrong, 32
//! bytes in and 32 bytes out.
//!
//! # The caller must reject an all-zero shared secret
//!
//! A few u-coordinates — 0, 1, and a handful of others of small order —
//! multiply to zero whatever the scalar is. A peer that sends one either is
//! broken or is trying to force a shared secret it already knows, so RFC 7748
//! §6.1 says to check for the all-zero result and abort. This module
//! deliberately does not do it: zero is the mathematically correct answer, and
//! [`crate::net::tls`] is the layer that knows what an abort means. Anything
//! else that calls in here owes the same check.
//!
//! # Constant time
//!
//! The private scalar is a secret, and the classic way to leak one is to let
//! the machine spend measurably different amounts of time on a zero bit and a
//! one bit. The Montgomery ladder below does the same ten field
//! multiplications per bit either way; the only place a scalar bit is used is
//! [`cswap`], and that is written as an arithmetic mask rather than an `if`
//! for exactly this reason. See the comment there before touching it.
//!
//! Nothing here indexes memory by a secret either, which is the other half of
//! the problem — no tables, so no cache-timing signal.
//!
//! # Field arithmetic
//!
//! Elements of GF(2^255 - 19) are five 51-bit limbs in a `u64` each, with
//! products accumulated in `u128`. The representation is redundant on
//! purpose: leaving 13 spare bits per limb means additions and subtractions
//! need no carry propagation at all, and only multiplication has to tidy up.
//! See [`Fe`] for the exact bounds this relies on.

pub const KEY_LEN: usize = 32;

/// Low 51 bits — one limb's worth.
const MASK: u64 = (1 << 51) - 1;

/// 2^255 - 19, the field's characteristic, as limbs. Used by [`Fe::to_bytes`]
/// to reduce into canonical form on the way out.
const P: [u64; 5] = [(1 << 51) - 19, MASK, MASK, MASK, MASK];

/// A field element mod 2^255 - 19, as five little-endian 51-bit limbs:
/// `v = l0 + l1*2^51 + l2*2^102 + l3*2^153 + l4*2^204`.
///
/// Limbs are allowed to exceed 51 bits, and the bounds are what keeps the
/// whole file from overflowing:
///
/// * *Reduced* means every limb is below 2^51 + 2^18. Everything out of
///   [`Fe::mul`], [`Fe::square`], [`Fe::mul_a24`] and [`Fe::from_bytes`] is
///   reduced, as are [`Fe::ZERO`] and [`Fe::ONE`].
/// * [`Fe::add`] and [`Fe::sub`] take reduced inputs and return limbs below
///   2^53, without touching carries.
/// * [`Fe::mul`] and [`Fe::square`] accept limbs below 2^53 — their `u128`
///   accumulators peak around 2^113 there, with fifteen bits to spare.
///
/// So an add or a subtract may feed a multiply, but not another add: the
/// ladder is written to respect that, and [`Fe::to_bytes`] additionally wants
/// its input reduced.
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// Decode a u-coordinate, little-endian.
    ///
    /// RFC 7748 §5 requires the most significant bit of the last byte to be
    /// ignored; here that happens for free, since bit 255 falls outside the
    /// 51-bit window of the top limb and is masked off with the rest.
    ///
    /// A u-coordinate of 2^255 - 19 or above is not rejected either — also per
    /// §5, which says to accept it and let it reduce.
    fn from_bytes(b: &[u8; KEY_LEN]) -> Fe {
        // Eight bytes starting at `i`, which is how the 51-bit limbs are read:
        // each one starts inside a byte, so it needs a whole word and a shift.
        let word = |i: usize| -> u64 {
            let mut w = [0u8; 8];
            w.copy_from_slice(&b[i..i + 8]);
            u64::from_le_bytes(w)
        };
        Fe([
            word(0) & MASK,
            (word(6) >> 3) & MASK,
            (word(12) >> 6) & MASK,
            (word(19) >> 1) & MASK,
            (word(24) >> 12) & MASK,
        ])
    }

    /// Encode, little-endian, fully reduced.
    ///
    /// "Fully" is the interesting part: the redundant representation can hold
    /// the same field element several ways, and two peers must agree on the
    /// bytes or the derived keys differ. Everything from 2^255 - 19 up folds
    /// down, so p itself encodes as zero and p + 1 as one.
    ///
    /// The input must be reduced in the sense of [`Fe`].
    fn to_bytes(self) -> [u8; KEY_LEN] {
        let mut t = self.0;

        // Two passes to get every limb under 51 bits. One is not enough: the
        // first pass folds the top carry back into limb 0 as 19, which can
        // push limb 0 over again. After the second the value is a plain
        // integer below 2^255, congruent to the input, so it is either the
        // canonical residue or that residue plus p.
        carry_pass(&mut t);
        carry_pass(&mut t);

        // Add 19 and let it wrap. If the value was p or above, the carry out
        // of 2^255 comes back as another 19, leaving `v - p + 19`; if it was
        // below p, nothing wraps and it is `v + 19`. Either way what is left
        // is the canonical residue, offset by 19 and so certainly non-zero.
        t[0] += 19;
        carry_pass(&mut t);

        // Undo the offset the only way that keeps the limbs unsigned: add p,
        // which is 2^255 - 19, and drop the 2^255 bit at the end. The sum is
        // at least 2^255 precisely because of the offset above.
        for i in 0..5 {
            t[i] += P[i];
        }
        for i in 0..4 {
            t[i + 1] += t[i] >> 51;
            t[i] &= MASK;
        }
        t[4] &= MASK;

        // Repack: limb boundaries fall inside bytes, so each output word is
        // the tail of one limb and the head of the next.
        let mut out = [0u8; KEY_LEN];
        let words = [
            t[0] | (t[1] << 51),
            (t[1] >> 13) | (t[2] << 38),
            (t[2] >> 26) | (t[3] << 25),
            (t[3] >> 39) | (t[4] << 12),
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// Limbwise sum. No carries: see [`Fe`] for why that is safe.
    fn add(self, other: Fe) -> Fe {
        let mut h = [0u64; 5];
        for i in 0..5 {
            h[i] = self.0[i] + other.0[i];
        }
        Fe(h)
    }

    /// Limbwise difference, computed as `self + 2p - other` so no limb can go
    /// negative in unsigned arithmetic. 2p is congruent to zero, and each of
    /// its limbs is larger than any limb of a reduced `other`.
    fn sub(self, other: Fe) -> Fe {
        // 2p = 2^256 - 38, spread over five limbs.
        const TWO_P: [u64; 5] = [
            (1 << 52) - 38,
            (1 << 52) - 2,
            (1 << 52) - 2,
            (1 << 52) - 2,
            (1 << 52) - 2,
        ];
        let mut h = [0u64; 5];
        for i in 0..5 {
            h[i] = self.0[i] + TWO_P[i] - other.0[i];
        }
        Fe(h)
    }

    /// Schoolbook product. Terms that land past limb 4 come back multiplied
    /// by 19, since 2^255 ≡ 19 (mod p) and each limb up there is 2^255 times
    /// a limb down here.
    fn mul(self, other: Fe) -> Fe {
        let f = self.0;
        let g = other.0;
        // Pre-scaled so the folded terms are one multiplication, not two.
        // Below 2^58 for inputs below 2^53, so these stay inside a u64.
        let g1_19 = 19 * g[1];
        let g2_19 = 19 * g[2];
        let g3_19 = 19 * g[3];
        let g4_19 = 19 * g[4];
        let m = |a: u64, b: u64| (a as u128) * (b as u128);
        carry_reduce([
            m(f[0], g[0]) + m(f[1], g4_19) + m(f[2], g3_19) + m(f[3], g2_19) + m(f[4], g1_19),
            m(f[0], g[1]) + m(f[1], g[0]) + m(f[2], g4_19) + m(f[3], g3_19) + m(f[4], g2_19),
            m(f[0], g[2]) + m(f[1], g[1]) + m(f[2], g[0]) + m(f[3], g4_19) + m(f[4], g3_19),
            m(f[0], g[3]) + m(f[1], g[2]) + m(f[2], g[1]) + m(f[3], g[0]) + m(f[4], g4_19),
            m(f[0], g[4]) + m(f[1], g[3]) + m(f[2], g[2]) + m(f[3], g[1]) + m(f[4], g[0]),
        ])
    }

    /// `self * self`. The same product as [`Fe::mul`] with the symmetric
    /// terms collected — ten multiplications instead of twenty-five, which is
    /// worth having when the ladder does four of these per scalar bit.
    fn square(self) -> Fe {
        let r = self.0;
        // Doubled and 19-scaled factors, all below 2^59 for inputs below
        // 2^53. `r0*r0` and the like appear once, cross terms twice.
        let d0 = 2 * r[0];
        let d1 = 2 * r[1];
        let d2 = 38 * r[2];
        let r3_19 = 19 * r[3];
        let r4_19 = 19 * r[4];
        let d4 = 2 * r4_19;
        let m = |a: u64, b: u64| (a as u128) * (b as u128);
        carry_reduce([
            m(r[0], r[0]) + m(d4, r[1]) + m(d2, r[3]),
            m(d0, r[1]) + m(d4, r[2]) + m(r[3], r3_19),
            m(d0, r[2]) + m(r[1], r[1]) + m(d4, r[3]),
            m(d0, r[3]) + m(d1, r[2]) + m(r[4], r4_19),
            m(d0, r[4]) + m(d1, r[3]) + m(r[2], r[2]),
        ])
    }

    /// `self * a24`, where a24 = 121665 is the curve constant `(A - 2) / 4`
    /// from RFC 7748 §5. One small factor, so no cross terms — but 121665 is
    /// 17 bits, which is more than a 51-bit limb has spare, hence `u128`.
    fn mul_a24(self) -> Fe {
        let mut h = [0u128; 5];
        for i in 0..5 {
            h[i] = (self.0[i] as u128) * 121665;
        }
        carry_reduce(h)
    }

    /// `self^(p-2)`, which by Fermat's little theorem is `1/self` — and zero
    /// for zero, which is what the low-order-point cases want.
    ///
    /// `p - 2 = 2^255 - 21` has every bit set except bits 2 and 4, so
    /// square-and-multiply over it is a fixed sequence of 254 squarings and
    /// 252 multiplications. Branching on the exponent is fine: it is this
    /// constant, the same for every call, and nothing about it is secret.
    fn invert(self) -> Fe {
        // Bit 254 is set, so the accumulator starts at `self` and the loop
        // picks up from bit 253.
        let mut acc = self;
        for bit in (0..254).rev() {
            acc = acc.square();
            if bit != 2 && bit != 4 {
                acc = acc.mul(self);
            }
        }
        acc
    }
}

/// One carry pass over 51-bit limbs, folding what leaves the top back into
/// limb 0 as 19. Limbs must be below 2^52 on entry.
fn carry_pass(t: &mut [u64; 5]) {
    for i in 0..4 {
        t[i + 1] += t[i] >> 51;
        t[i] &= MASK;
    }
    t[0] += 19 * (t[4] >> 51);
    t[4] &= MASK;
}

/// Bring a freshly multiplied accumulator back to reduced limbs.
///
/// The chain runs once through the limbs, folds the overflow past 2^255 into
/// limb 0 as 19, and then carries limb 0 one more time — that fold can be as
/// much as 2^68 and would otherwise leave limb 0 far too wide.
fn carry_reduce(mut h: [u128; 5]) -> Fe {
    const M: u128 = MASK as u128;
    for i in 0..4 {
        h[i + 1] += h[i] >> 51;
        h[i] &= M;
    }
    h[0] += 19 * (h[4] >> 51);
    h[4] &= M;
    h[1] += h[0] >> 51;
    h[0] &= M;
    Fe([h[0] as u64, h[1] as u64, h[2] as u64, h[3] as u64, h[4] as u64])
}

/// Exchange `a` and `b` if `swap` is 1, leave them alone if it is 0.
///
/// `swap` comes straight from a bit of the private scalar, so this must not
/// be an `if`: a branch here is a branch on key material, and anything that
/// can watch the timing or the branch predictor can read the key out one bit
/// at a time. Instead `mask` is all-ones or all-zeros — `0 - 1` wraps to
/// `0xffff_ffff_ffff_ffff` — and the xor trick swaps or does nothing with the
/// same instructions either way, no jump involved.
///
/// Please do not "simplify" this.
fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a.0[i] ^ b.0[i]);
        a.0[i] ^= t;
        b.0[i] ^= t;
    }
}

/// The base point, u = 9.
const BASE_POINT: [u8; KEY_LEN] = {
    let mut b = [0u8; KEY_LEN];
    b[0] = 9;
    b
};

/// The scalar multiplication of RFC 7748: `scalar * point` on Curve25519.
///
/// The scalar is clamped internally, so callers may pass raw random bytes.
/// The high bit of `point` is ignored, as §5 requires.
///
/// A result of all zeros means `point` had small order; see the note at the
/// top of this module about whose job it is to reject that.
pub fn scalar_mult(scalar: &[u8; KEY_LEN], point: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    // RFC 7748 §5 clamping. The low three bits go so the scalar is a multiple
    // of the cofactor, which kills any small-order component the peer may
    // have slipped in; bit 254 is forced on and bit 255 off so every scalar
    // has the same bit length, and the ladder therefore the same trip count.
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;

    // The ladder, as written out in §5. It carries two points at once, in
    // projective (x : z) form so there is no division per step, and keeps
    // them one apart — the difference is always the input point, which is
    // what lets it get away without y-coordinates entirely.
    let x1 = Fe::from_bytes(point);
    let mut x2 = Fe::ONE;
    let mut z2 = Fe::ZERO;
    let mut x3 = x1;
    let mut z3 = Fe::ONE;

    // Deferred swap: rather than swap before and after each step, the ladder
    // tracks whether the pair is currently the wrong way round and swaps only
    // when consecutive bits differ. Purely an optimisation, and it costs
    // nothing in timing since `cswap` runs regardless.
    let mut swap = 0u64;

    for t in (0..255).rev() {
        let bit = ((k[t >> 3] >> (t & 7)) & 1) as u64;
        swap ^= bit;
        cswap(swap, &mut x2, &mut x3);
        cswap(swap, &mut z2, &mut z3);
        swap = bit;

        let a = x2.add(z2);
        let aa = a.square();
        let b = x2.sub(z2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).square();
        z3 = x1.mul(da.sub(cb).square());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(e.mul_a24()));
    }

    cswap(swap, &mut x2, &mut x3);
    cswap(swap, &mut z2, &mut z3);

    // Back out of projective form. `1/0` comes out as 0, which is how the
    // small-order inputs end up as an all-zero result rather than an error.
    x2.mul(z2.invert()).to_bytes()
}

/// The public key for a private scalar — [`scalar_mult`] against the base
/// point 9.
pub fn public_key(scalar: &[u8; KEY_LEN]) -> [u8; KEY_LEN] {
    scalar_mult(scalar, &BASE_POINT)
}

/// A 64-character hex string as bytes. Keeps the test vectors below looking
/// like the ones printed in the RFC, which is the difference between being
/// able to check them by eye and not.
///
/// `const fn`, and only ever used to initialise `const` items, so a mistyped
/// vector is a build failure rather than a boot-time panic.
const fn hex(s: &str) -> [u8; KEY_LEN] {
    let text = s.as_bytes();
    if text.len() != KEY_LEN * 2 {
        panic!("test vector is not 32 bytes of hex");
    }
    let mut out = [0u8; KEY_LEN];
    let mut i = 0;
    while i < KEY_LEN {
        out[i] = (nibble(text[i * 2]) << 4) | nibble(text[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => panic!("test vector has a non-hex digit"),
    }
}

/// Boot-time checks.
///
/// Eleven of them, and eleven scalar multiplications, which is what the cost
/// is: 0.3 ms measured on a fast host, a few milliseconds on anything this
/// kernel actually boots on.
///
/// RFC 7748 §5.2 also gives an iterated test at 1, 1000 and 1,000,000 rounds.
/// Only the first round is here. A thousand rounds measures 27 ms on that
/// same fast host, so a fifth of a second or worse here — a hundred times the
/// rest of this suite, on every boot, to re-run the same ladder against a
/// thousand more inputs. It is a fine thing to run once by hand and a bad
/// thing to run at boot; the millionth is minutes away and not worth
/// discussing.
pub fn selftest() -> crate::selftest::Report {
    // RFC 7748 §5.2, the two single scalar-multiplication vectors.
    const SCALAR1: [u8; KEY_LEN] =
        hex("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
    const POINT1: [u8; KEY_LEN] =
        hex("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
    const WANT1: [u8; KEY_LEN] =
        hex("c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552");
    const SCALAR2: [u8; KEY_LEN] =
        hex("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d");
    const POINT2: [u8; KEY_LEN] =
        hex("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493");
    const WANT2: [u8; KEY_LEN] =
        hex("95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957");
    // §5.2's iterated test, after one round.
    const ITERATED: [u8; KEY_LEN] =
        hex("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079");
    // §6.1's Diffie-Hellman example.
    const ALICE_PRIVATE: [u8; KEY_LEN] =
        hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
    const ALICE_PUBLIC: [u8; KEY_LEN] =
        hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    const BOB_PRIVATE: [u8; KEY_LEN] =
        hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
    const BOB_PUBLIC: [u8; KEY_LEN] =
        hex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
    const SHARED: [u8; KEY_LEN] =
        hex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");

    let mut report = crate::selftest::Report::new();

    report.check("rfc 7748 vector 1", scalar_mult(&SCALAR1, &POINT1) == WANT1);
    report.check("rfc 7748 vector 2", scalar_mult(&SCALAR2, &POINT2) == WANT2);

    // One round of the iterated test. Scalar and point are both the base
    // point here, which the two vectors above never exercise.
    report.check(
        "rfc 7748 iterated once",
        scalar_mult(&BASE_POINT, &BASE_POINT) == ITERATED,
    );

    // §6.1 in full: two public keys, and both sides of the exchange arriving
    // at the published secret. This is exactly the shape of what TLS does
    // with this code, so it is the check that matters most.
    report.check("alice's public key", public_key(&ALICE_PRIVATE) == ALICE_PUBLIC);
    report.check("bob's public key", public_key(&BOB_PRIVATE) == BOB_PUBLIC);
    report.check(
        "alice derives the shared secret",
        scalar_mult(&ALICE_PRIVATE, &BOB_PUBLIC) == SHARED,
    );
    report.check(
        "bob derives the same secret",
        scalar_mult(&BOB_PRIVATE, &ALICE_PUBLIC) == SHARED,
    );

    // Clamping really is internal: mangle exactly the bits clamping is
    // supposed to overwrite — the low three, and the top two — and the first
    // vector's answer must come back unchanged.
    let mut unclamped = SCALAR1;
    unclamped[0] |= 0x07;
    unclamped[31] = (unclamped[31] & 0x3f) | 0x80;
    report.check("clamps the scalar", scalar_mult(&unclamped, &POINT1) == WANT1);

    // Low-order points, the ones the TLS layer has to reject.
    let zero = [0u8; KEY_LEN];
    let mut one = [0u8; KEY_LEN];
    one[0] = 1;
    report.check("zero point gives zero", scalar_mult(&SCALAR1, &zero) == zero);
    report.check("point one gives zero", scalar_mult(&SCALAR1, &one) == zero);

    // §5: the high bit of the u-coordinate is not part of it.
    let mut high_bit_set = POINT1;
    high_bit_set[31] |= 0x80;
    report.check(
        "ignores the high bit of u",
        scalar_mult(&SCALAR1, &high_bit_set) == WANT1,
    );

    report
}
