//! HTML character reference decoding.
//!
//! The renderer draws a monospace Latin font, so characters outside it are
//! mapped to a close ASCII stand-in (typographic quotes to plain ones, dashes
//! to hyphens) rather than left as boxes.

use alloc::string::{String, ToString};

/// How far past an `&` a `;` may be and still be a character reference.
/// `&thetasym;` is ten bytes; twelve leaves room and keeps a stray ampersand
/// in ordinary prose from scanning the rest of the line.
const MAX_REFERENCE_LEN: usize = 12;

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let mut rest = s;

    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];

        // A reference is short; anything longer is a stray ampersand. The
        // limit is a byte count, so it has to be applied by walking
        // characters: slicing at a fixed byte offset lands inside a
        // multi-byte character sooner or later — a page with a `ü` twelve
        // bytes past an `&` panicked the kernel — and a character boundary
        // is the only place a `&str` may be cut.
        let semi = rest
            .char_indices()
            .take_while(|(offset, _)| *offset < MAX_REFERENCE_LEN)
            .find(|(_, ch)| *ch == ';')
            .map(|(offset, _)| offset);
        let Some(semi) = semi else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };

        match lookup(&rest[1..semi]) {
            Some(replacement) => {
                out.push_str(replacement);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }

    out.push_str(rest);
    out
}

fn lookup(entity: &str) -> Option<&'static str> {
    let named = match entity {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => " ",
        "copy" => "(c)",
        "reg" => "(R)",
        "trade" => "(TM)",
        "mdash" | "ndash" | "minus" => "-",
        "hellip" => "...",
        "lsquo" | "rsquo" | "sbquo" => "'",
        "ldquo" | "rdquo" | "bdquo" => "\"",
        "laquo" => "<<",
        "raquo" => ">>",
        "middot" | "bull" => "*",
        "deg" => "deg",
        "times" => "x",
        "divide" => "/",
        "frac12" => "1/2",
        "pound" => "GBP",
        "euro" => "EUR",
        "yen" => "YEN",
        "cent" => "c",
        "sect" => "S",
        "para" => "P",
        "dagger" => "+",
        "permil" => "%o",
        "larr" => "<-",
        "rarr" => "->",
        "harr" => "<->",
        "shy" | "zwj" | "zwnj" | "thinsp" | "ensp" | "emsp" => "",
        _ => return numeric(entity),
    };
    Some(named)
}

fn numeric(entity: &str) -> Option<&'static str> {
    let digits = entity.strip_prefix('#')?;
    let code = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        digits.parse::<u32>().ok()?
    };
    Some(from_code(code))
}

/// Map a code point to something drawable.
///
/// Returning `&'static str` keeps this allocation-free; ASCII code points
/// index a table of single-character strings.
fn from_code(code: u32) -> &'static str {
    const ASCII: [&str; 128] = [
        "\0", "\u{1}", "\u{2}", "\u{3}", "\u{4}", "\u{5}", "\u{6}", "\u{7}",
        "\u{8}", "\t", "\n", "\u{b}", "\u{c}", "\r", "\u{e}", "\u{f}",
        "\u{10}", "\u{11}", "\u{12}", "\u{13}", "\u{14}", "\u{15}", "\u{16}", "\u{17}",
        "\u{18}", "\u{19}", "\u{1a}", "\u{1b}", "\u{1c}", "\u{1d}", "\u{1e}", "\u{1f}",
        " ", "!", "\"", "#", "$", "%", "&", "'",
        "(", ")", "*", "+", ",", "-", ".", "/",
        "0", "1", "2", "3", "4", "5", "6", "7",
        "8", "9", ":", ";", "<", "=", ">", "?",
        "@", "A", "B", "C", "D", "E", "F", "G",
        "H", "I", "J", "K", "L", "M", "N", "O",
        "P", "Q", "R", "S", "T", "U", "V", "W",
        "X", "Y", "Z", "[", "\\", "]", "^", "_",
        "`", "a", "b", "c", "d", "e", "f", "g",
        "h", "i", "j", "k", "l", "m", "n", "o",
        "p", "q", "r", "s", "t", "u", "v", "w",
        "x", "y", "z", "{", "|", "}", "~", "\u{7f}",
    ];

    if let Some(s) = ASCII.get(code as usize) {
        return s;
    }

    match code {
        0x2018 | 0x2019 => "'",
        0x201C | 0x201D => "\"",
        0x2013 | 0x2014 => "-",
        0x2026 => "...",
        0x00A0 => " ",
        0x00A9 => "(c)",
        0x00AE => "(R)",
        0x2022 | 0x00B7 => "*",
        _ => "?",
    }
}
