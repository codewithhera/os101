//! Shared bookkeeping for the kernel's boot-time self-tests.
//!
//! The kernel is a `no_std` binary for a bare-metal target, so it cannot run
//! `cargo test`. Subsystems whose logic is pure — checksums, parsers, layout —
//! exercise themselves at boot instead and report a single line each.

use alloc::string::String;
use alloc::vec::Vec;

/// How many failing check names to keep. One is not enough to debug with, and
/// a suite that fails more than this has something structurally wrong.
const MAX_RECORDED: usize = 6;

pub struct Report {
    pub passed: usize,
    pub failed: usize,
    failures: Vec<&'static str>,
}

impl Report {
    pub fn new() -> Self {
        Report { passed: 0, failed: 0, failures: Vec::new() }
    }

    pub fn check(&mut self, name: &'static str, ok: bool) {
        if ok {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        if self.failures.len() < MAX_RECORDED {
            self.failures.push(name);
        }
    }

    /// Fold another suite's results into this one.
    ///
    /// Lets a module built from several files report one line rather than
    /// four, without any of them knowing about the others.
    pub fn merge(&mut self, other: Report) {
        self.passed += other.passed;
        self.failed += other.failed;
        for name in other.failures {
            if self.failures.len() < MAX_RECORDED {
                self.failures.push(name);
            }
        }
    }

    /// The names that failed, comma-separated, with a count of any beyond the
    /// ones kept.
    pub fn failure_summary(&self) -> String {
        let mut out = String::new();
        for (i, name) in self.failures.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(name);
        }
        let hidden = self.failed - self.failures.len();
        if hidden > 0 {
            out.push_str(&alloc::format!(", and {} more", hidden));
        }
        out
    }
}
