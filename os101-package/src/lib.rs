//! `.opk` — the OS101 package format.
//!
//! Shared by the kernel (which installs packages) and the host tooling
//! (which creates them), so the writer and the reader cannot drift apart.
//!
//! One installable app is one file: a fixed header, a text manifest, and an
//! ELF payload. Single-file packages keep "install" honest — copy one blob,
//! validate it, register it — instead of spreading an app across a directory
//! tree the installer would have to reassemble.
//!
//! ```text
//! offset  size  field
//!      0     8  magic "OS101PKG"
//!      8     2  format version (u16 LE)
//!     10     2  flags (u16 LE, reserved, must be 0)
//!     12     4  manifest length (u32 LE)
//!     16     4  payload length (u32 LE)
//!     20     4  payload CRC-32 (u32 LE)
//!     24     …  manifest: UTF-8 `key = value` lines
//!      …     …  payload: ELF64 executable
//! ```
//!
//! Everything in here parses untrusted input: a package can arrive from a
//! disk image or, later, a download. Each field is bounds-checked against a
//! ceiling before it is used to slice or allocate, because the kernel builds
//! with `panic = abort` — a bad length is a crash, not an error.

// `std` is pulled in only for the test harness; the kernel links this crate
// as `no_std`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub const MAGIC: [u8; 8] = *b"OS101PKG";
pub const FORMAT_VERSION: u16 = 1;
pub const HEADER_LEN: usize = 24;

/// Ceilings for untrusted package fields.
const MAX_MANIFEST_LEN: usize = 4 * 1024;
const MAX_PAYLOAD_LEN: usize = 4 * 1024 * 1024;
const MAX_NAME_LEN: usize = 32;
const MAX_FIELD_LEN: usize = 128;

/// A validated package, ready to install.
#[derive(Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    /// Icon hint; the launcher falls back to a generic icon if unknown.
    pub icon: String,
    pub payload: Vec<u8>,
}

/// A package name is used as a filesystem path component and shown in the UI,
/// so restrict it rather than sanitising later: letters, digits, `-`, `_`
/// and spaces only.
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ')
}

/// Lower-case, `-`-separated form of a name, for use as a file name.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            for l in c.to_lowercase() {
                out.push(l);
            }
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("app");
    }
    out
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// CRC-32 (IEEE 802.3, reflected) computed bitwise.
///
/// No lookup table: a 1 KiB table in a kernel that checksums a package at
/// most a few times per boot is not worth the static footprint.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Parse and fully validate a `.opk` image.
pub fn parse(bytes: &[u8]) -> Result<Package, &'static str> {
    if bytes.len() < HEADER_LEN {
        return Err("not a package: too short");
    }
    if bytes[0..8] != MAGIC {
        return Err("not a package: bad magic");
    }
    let version = read_u16(bytes, 8);
    if version != FORMAT_VERSION {
        return Err("unsupported package format version");
    }
    if read_u16(bytes, 10) != 0 {
        return Err("unsupported package flags");
    }

    let manifest_len = read_u32(bytes, 12) as usize;
    let payload_len = read_u32(bytes, 16) as usize;
    let expected_crc = read_u32(bytes, 20);

    if manifest_len > MAX_MANIFEST_LEN {
        return Err("package manifest too large");
    }
    if payload_len > MAX_PAYLOAD_LEN {
        return Err("package payload too large");
    }
    // Checked arithmetic: a hostile pair of lengths must not wrap and then
    // slice past the end of the buffer.
    let total = HEADER_LEN
        .checked_add(manifest_len)
        .and_then(|n| n.checked_add(payload_len))
        .ok_or("package length overflow")?;
    if bytes.len() < total {
        return Err("package truncated");
    }

    let manifest_bytes = &bytes[HEADER_LEN..HEADER_LEN + manifest_len];
    let payload = &bytes[HEADER_LEN + manifest_len..total];

    if crc32(payload) != expected_crc {
        return Err("package payload checksum mismatch");
    }

    let manifest = core::str::from_utf8(manifest_bytes)
        .map_err(|_| "package manifest is not valid UTF-8")?;
    let fields = parse_manifest(manifest);

    let name = fields
        .iter()
        .find(|(k, _)| k == "name")
        .map(|(_, v)| v.clone())
        .ok_or("package manifest has no `name`")?;
    if !is_valid_name(&name) {
        return Err("package name has invalid characters");
    }

    let get = |key: &str, default: &str| -> String {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| default.to_string())
    };

    validate_elf(payload)?;

    Ok(Package {
        name,
        version: get("version", "0.0.0"),
        description: get("description", ""),
        icon: get("icon", ""),
        payload: payload.to_vec(),
    })
}

/// Minimal ELF sanity check, so obviously-wrong payloads are rejected at
/// install time rather than faulting the loader later.
fn validate_elf(payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() < 64 {
        return Err("payload is not an ELF executable");
    }
    if payload[0..4] != [0x7F, b'E', b'L', b'F'] {
        return Err("payload is not an ELF executable");
    }
    if payload[4] != 2 {
        return Err("payload is not 64-bit");
    }
    if payload[5] != 1 {
        return Err("payload is not little-endian");
    }
    // e_machine at offset 18: 0x3E = x86-64.
    if read_u16(payload, 18) != 0x3E {
        return Err("payload is not an x86-64 binary");
    }
    // e_type at offset 16: 2 = ET_EXEC, 3 = ET_DYN. The SDK links apps at a
    // fixed address but emits ET_DYN, and the loader maps program headers by
    // their p_vaddr either way, so both are valid here.
    let e_type = read_u16(payload, 16);
    if e_type != 2 && e_type != 3 {
        return Err("payload is not an executable image");
    }
    Ok(())
}

/// `key = value` lines; `#` starts a comment.
fn parse_manifest(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let mut value = v.trim().to_string();
        value.truncate(MAX_FIELD_LEN);
        out.push((key, value));
    }
    out
}

/// Assemble a `.opk` image. Used by the in-OS packager and by host tooling.
pub fn build(manifest: &str, payload: &[u8]) -> Vec<u8> {
    let manifest_bytes = manifest.as_bytes();
    let mut out = Vec::with_capacity(HEADER_LEN + manifest_bytes.len() + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(manifest_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(payload).to_le_bytes());
    out.extend_from_slice(manifest_bytes);
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but structurally valid ELF64 x86-64 executable header.
    fn fake_elf() -> Vec<u8> {
        let mut e = vec![0u8; 64];
        e[0..4].copy_from_slice(&[0x7F, b'E', b'L', b'F']);
        e[4] = 2; // 64-bit
        e[5] = 1; // little-endian
        e[6] = 1; // ELF version
        e[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        e[18..20].copy_from_slice(&0x3Eu16.to_le_bytes()); // x86-64
        e
    }

    fn manifest() -> &'static str {
        "name = Test App\nversion = 2.1.0\ndescription = hello\n"
    }

    fn good_package() -> Vec<u8> {
        build(manifest(), &fake_elf())
    }

    #[test]
    fn round_trips_a_package() {
        let pkg = parse(&good_package()).expect("valid package should parse");
        assert_eq!(pkg.name, "Test App");
        assert_eq!(pkg.version, "2.1.0");
        assert_eq!(pkg.description, "hello");
        assert_eq!(pkg.payload, fake_elf());
    }

    #[test]
    fn defaults_missing_optional_fields() {
        let pkg = parse(&build("name = Bare\n", &fake_elf())).unwrap();
        assert_eq!(pkg.version, "0.0.0");
        assert_eq!(pkg.description, "");
    }

    #[test]
    fn manifest_ignores_comments_and_blank_lines() {
        let text = "# a comment\n\n  name = Spaced  \n\nversion=9\n";
        let pkg = parse(&build(text, &fake_elf())).unwrap();
        assert_eq!(pkg.name, "Spaced");
        assert_eq!(pkg.version, "9");
    }

    #[test]
    fn rejects_short_input() {
        assert!(parse(b"").is_err());
        assert!(parse(b"OS101PK").is_err());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = good_package();
        bytes[0] = b'X';
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes = good_package();
        bytes[8..10].copy_from_slice(&99u16.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn rejects_unknown_flags() {
        let mut bytes = good_package();
        bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut bytes = good_package();
        bytes.truncate(bytes.len() - 8);
        assert!(parse(&bytes).is_err());
    }

    /// Lengths that sum past `usize::MAX` must be caught before any slicing.
    #[test]
    fn rejects_length_overflow() {
        let mut bytes = good_package();
        bytes[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn rejects_corrupted_payload() {
        let mut bytes = good_package();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(
            parse(&bytes).unwrap_err(),
            "package payload checksum mismatch"
        );
    }

    #[test]
    fn rejects_missing_name() {
        assert!(parse(&build("version = 1\n", &fake_elf())).is_err());
    }

    #[test]
    fn rejects_hostile_name() {
        // Path separators would escape /apps when the package is stored.
        assert!(parse(&build("name = ../../etc\n", &fake_elf())).is_err());
        assert!(parse(&build("name = \n", &fake_elf())).is_err());
    }

    #[test]
    fn rejects_non_elf_payload() {
        assert!(parse(&build(manifest(), &[0u8; 128])).is_err());
    }

    #[test]
    fn rejects_wrong_architecture() {
        let mut elf = fake_elf();
        elf[18..20].copy_from_slice(&0xB7u16.to_le_bytes()); // aarch64
        assert!(parse(&build(manifest(), &elf)).is_err());
    }

    #[test]
    fn rejects_32_bit_payload() {
        let mut elf = fake_elf();
        elf[4] = 1;
        assert!(parse(&build(manifest(), &elf)).is_err());
    }

    /// The SDK emits ET_DYN, so both executable types must be accepted.
    #[test]
    fn accepts_pie_payload() {
        let mut elf = fake_elf();
        elf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        assert!(parse(&build(manifest(), &elf)).is_ok());
    }

    #[test]
    fn crc32_matches_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn slugify_produces_safe_file_names() {
        assert_eq!(slugify("Demo App"), "demo-app");
        assert_eq!(slugify("My  Cool_Thing!"), "my-cool-thing");
        assert_eq!(slugify("!!!"), "app");
    }

    #[test]
    fn name_validation_covers_edges() {
        assert!(is_valid_name("Fine Name_1-2"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("has/slash"));
        assert!(!is_valid_name("has.dot"));
        assert!(!is_valid_name(&"x".repeat(33)));
    }
}
