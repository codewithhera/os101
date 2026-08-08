//! Where OS101's random numbers come from, and how good they are.
//!
//! # Be honest about this
//!
//! A TLS handshake needs two unpredictable values: the 32-byte client random,
//! and the X25519 private key whose secrecy the whole session rests on. An
//! attacker who can guess the private key and who recorded the traffic can
//! decrypt it afterwards. So the quality of what follows is a security
//! property, not a detail.
//!
//! OS101 has no good entropy source. There is no hardware RNG it can count on,
//! no disk seek timing worth the name, no stored seed from last boot, and on a
//! freshly booted virtual machine there is barely any user input either. What
//! it has is:
//!
//! - **RDRAND**, when the CPU advertises it. This is a real hardware entropy
//!   source and the pool prefers it — but QEMU's default `qemu64` CPU model
//!   does not expose it, so on the emulator it is usually absent.
//! - **Time-stamp counter jitter.** Sampling the TSC across short, variable
//!   pieces of work yields a few genuinely unpredictable low bits per sample,
//!   from cache state, interrupt timing and the host's own scheduling. It is
//!   the weakest of the classic sources and the one everybody falls back on.
//! - **Whatever the machine has already done** — the tick count, the MAC
//!   address, heap addresses, the pointer positions the user has generated —
//!   mixed in at boot. None of it is secret; it only serves to make two
//!   otherwise identical machines diverge.
//!
//! These are stirred into a 32-byte pool with SHA-256, and output is generated
//! by hashing the pool with a counter, so the pool is never handed out
//! directly and output cannot be run backwards into it.
//!
//! **The result is good enough that two boots do not produce the same session
//! key, and not good enough to resist a determined attacker who knows the OS
//! is running under an emulator with a predictable startup.** That is an
//! honest description of a hobby kernel's position, and it is why the TLS
//! module says plainly that it protects against passive eavesdropping and not
//! much else.

use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

use super::sha256;

/// The accumulated entropy. Never handed out; only ever hashed.
static POOL: Mutex<[u8; 32]> = Mutex::new([0; 32]);
/// Makes every request produce different output even if the pool has not
/// changed, so two calls in the same microsecond cannot collide.
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn rdtsc() -> u64 {
    // SAFETY: reads a counter register. No operands, no memory, unprivileged.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Does this CPU have RDRAND? CPUID leaf 1, ECX bit 30.
fn has_rdrand() -> bool {
    core::arch::x86_64::__cpuid(1).ecx & (1 << 30) != 0
}

/// One hardware random word, or None if the CPU has no RDRAND or it refused.
///
/// RDRAND is specified to fail occasionally when its entropy pool is drained,
/// and Intel's guidance is to retry a bounded number of times and then treat
/// it as unavailable rather than spin.
fn hardware_word() -> Option<u64> {
    if !has_rdrand() {
        return None;
    }
    // SAFETY: guarded by the CPUID check above.
    unsafe { hardware_word_inner() }
}

#[target_feature(enable = "rdrand")]
unsafe fn hardware_word_inner() -> Option<u64> {
    let mut value = 0u64;
    for _ in 0..10 {
        if core::arch::x86_64::_rdrand64_step(&mut value) == 1 {
            return Some(value);
        }
    }
    None
}

/// Sample the time-stamp counter across variable work.
///
/// The work between samples has to actually vary, or every sample differs by
/// the same constant and contributes nothing. Feeding the previous sample back
/// into the loop bound is what makes the interval depend on the machine's own
/// unpredictable state.
fn jitter(into: &mut [u8; 64]) {
    let mut previous = rdtsc();
    for slot in 0..8 {
        let spins = 16 + (previous & 0x3F);
        let mut mixed = previous;
        for _ in 0..spins {
            mixed = mixed.rotate_left(7) ^ rdtsc();
            core::hint::spin_loop();
        }
        let now = rdtsc();
        into[slot * 8..slot * 8 + 8].copy_from_slice(&(now ^ mixed).to_le_bytes());
        previous = now;
    }
}

/// Stir arbitrary bytes into the pool.
///
/// Nothing passed here has to be secret or unpredictable: hashing can only
/// add, never subtract, so mixing in something an attacker knows leaves the
/// pool no worse than it was.
pub fn stir(extra: &[u8]) {
    let mut pool = POOL.lock();
    let mut hash = sha256::Sha256::new();
    hash.update(&*pool);
    hash.update(extra);
    hash.update(&rdtsc().to_le_bytes());
    if let Some(word) = hardware_word() {
        hash.update(&word.to_le_bytes());
    }
    *pool = hash.finish();
}

/// Seed the pool at boot from everything the machine can tell us about itself.
///
/// Called once networking is up, so the MAC address is available — it is not
/// secret, but it is the one value that reliably differs between two machines
/// booting the same image.
pub fn seed(mac: [u8; 6]) {
    let mut samples = [0u8; 64];
    jitter(&mut samples);

    let stack_probe = &samples as *const _ as usize as u64;
    let heap_probe = alloc::boxed::Box::new(0u8);
    let heap_address = (&*heap_probe as *const u8) as usize as u64;

    let mut extra = [0u8; 64 + 6 + 24];
    extra[..64].copy_from_slice(&samples);
    extra[64..70].copy_from_slice(&mac);
    extra[70..78].copy_from_slice(&stack_probe.to_le_bytes());
    extra[78..86].copy_from_slice(&heap_address.to_le_bytes());
    extra[86..94].copy_from_slice(&crate::clock::ticks().to_le_bytes());
    stir(&extra);
}

/// Fill `out` with random bytes.
///
/// Each call stirs fresh jitter in first, so a long-running session keeps
/// gaining entropy rather than expanding one boot-time seed forever.
pub fn fill(out: &mut [u8]) {
    let mut samples = [0u8; 64];
    jitter(&mut samples);
    stir(&samples);

    let pool = *POOL.lock();
    let mut written = 0;
    while written < out.len() {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut hash = sha256::Sha256::new();
        hash.update(&pool);
        hash.update(&counter.to_le_bytes());
        hash.update(&rdtsc().to_le_bytes());
        let block = hash.finish();

        let take = (out.len() - written).min(block.len());
        out[written..written + take].copy_from_slice(&block[..take]);
        written += take;
    }
}

/// A fixed-size draw, for keys.
pub fn bytes32() -> [u8; 32] {
    let mut out = [0u8; 32];
    fill(&mut out);
    out
}

pub fn selftest() -> crate::selftest::Report {
    let mut report = crate::selftest::Report::new();

    // Two draws must differ. This is a weak test — it would pass for a
    // counter — but a generator that returns the same bytes twice is the
    // failure that actually happens, and it is catastrophic here.
    let first = bytes32();
    let second = bytes32();
    report.check("two draws differ", first != second);
    report.check("a draw is not all zeros", first != [0u8; 32]);

    // Every byte of a request must be written, including a partial final
    // block: an off-by-one here would leave key material as zeros.
    let mut odd = [0xAAu8; 100];
    fill(&mut odd);
    report.check("an odd-sized request is filled", odd.iter().any(|b| *b != 0xAA));
    report.check("the tail of a request is filled", odd[99] != 0xAA || odd[98] != 0xAA);

    let mut empty: [u8; 0] = [];
    fill(&mut empty);
    report.check("an empty request does not panic", true);

    // Crude distribution check. Over 4096 bytes every value should appear
    // roughly 16 times; what this is really looking for is a stuck generator
    // producing one value, or output confined to a narrow range.
    let mut sample = [0u8; 4096];
    fill(&mut sample);
    let mut seen = [0u32; 256];
    for byte in sample {
        seen[byte as usize] += 1;
    }
    let missing = seen.iter().filter(|c| **c == 0).count();
    report.check("output covers the byte range", missing < 16);
    let ones: u32 = sample.iter().map(|b| b.count_ones()).sum();
    let expected = (sample.len() * 4) as u32;
    report.check(
        "bits are roughly balanced",
        ones.abs_diff(expected) < expected / 8,
    );

    // The pool must actually move. If `stir` were a no-op, output would still
    // vary through the counter alone, and that is the bug this catches.
    let before = *POOL.lock();
    stir(b"selftest");
    let after = *POOL.lock();
    report.check("stirring changes the pool", before != after);

    report
}
