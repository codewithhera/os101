//! The cryptography TLS needs, and nothing else.
//!
//! # Scope
//!
//! This is not a general-purpose crypto library. It exists to let
//! [`crate::net::tls`] complete one specific handshake — TLS 1.3 with
//! `TLS_CHACHA20_POLY1305_SHA256` over an X25519 key exchange — so every
//! algorithm here is one that handshake names, and there is nothing else.
//!
//! ChaCha20-Poly1305 rather than AES-GCM on purpose. AES in software wants
//! either lookup tables, which leak through the cache, or the AES-NI
//! instructions, which a kernel that still boots on a 2008 machine cannot
//! assume. ChaCha20 is add-rotate-xor on 32-bit words: no tables, no lookups,
//! constant time by construction, and about as fast in software as anything
//! gets. Google's servers offer it, which is what matters here.
//!
//! # What this is not
//!
//! **None of this is hardened against side channels beyond the algorithmic
//! choices above, and nothing here has been audited.** The field arithmetic in
//! [`x25519`] is written to be constant-time because the alternative leaks a
//! private key, but "written to be" is not "proven to be". OS101 has no
//! secrets worth this attention today — it has no user accounts, no stored
//! credentials, and no multi-tenant anything. Do not lift this code into
//! somewhere that does.

pub mod chacha20poly1305;
pub mod hkdf;
pub mod hmac;
pub mod random;
pub mod sha256;
pub mod x25519;

/// Every algorithm's own checks, as one report.
pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();
    report.merge(sha256::selftest());
    report.merge(hmac::selftest());
    report.merge(hkdf::selftest());
    report.merge(chacha20poly1305::selftest());
    report.merge(x25519::selftest());
    report.merge(random::selftest());
    report
}
