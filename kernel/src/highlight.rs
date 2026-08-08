//! A tiny C syntax highlighter for the Code Editor.
//!
//! This is a "good enough to read your own code" highlighter, not a real C
//! lexer: it does not understand digraphs, raw strings, or line
//! continuations inside a string. It walks the whole buffer once and hands
//! back one colour per character (same order, same count as
//! `src.chars()`), so the renderer can zip the colours against exactly the
//! same characters it is about to draw.

use alloc::string::String;
use alloc::vec::Vec;

use crate::color::Color;

pub const KEYWORD: Color = Color::hex(0xC792EA);
pub const TYPE_NAME: Color = Color::hex(0x82AAFF);
pub const STRING: Color = Color::hex(0xC3E88D);
pub const COMMENT: Color = Color::hex(0x8B93A7);
pub const NUMBER: Color = Color::hex(0xF78C6C);
pub const PREPROC: Color = Color::hex(0xF07178);
pub const PLAIN: Color = Color::hex(0xE2E8F0);

const KEYWORDS: &[&str] = &[
    "if", "else", "for", "while", "do", "switch", "case", "default", "break",
    "continue", "return", "goto", "sizeof", "struct", "union", "enum",
    "typedef", "static", "const", "volatile", "extern", "register", "inline",
    "void",
];

const TYPES: &[&str] = &[
    "int", "char", "float", "double", "long", "short", "unsigned", "signed",
    "size_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "int8_t",
    "int16_t", "int32_t", "int64_t", "bool", "FILE",
];

#[derive(Clone, Copy, PartialEq)]
enum St {
    Code,
    LineComment,
    BlockComment,
    Str,
    Char,
    Preproc,
}

/// Colour every character of `src`, in order. The returned vector always has
/// exactly `src.chars().count()` entries.
pub fn colorize(src: &str) -> Vec<Color> {
    let chars: Vec<char> = src.chars().collect();
    let mut colors: Vec<Color> = alloc::vec![PLAIN; chars.len()];
    let mut state = St::Code;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match state {
            St::LineComment => {
                colors[i] = COMMENT;
                if c == '\n' {
                    state = St::Code;
                }
                i += 1;
            }
            St::BlockComment => {
                colors[i] = COMMENT;
                if c == '*' && chars.get(i + 1) == Some(&'/') {
                    colors[i + 1] = COMMENT;
                    i += 2;
                    state = St::Code;
                } else {
                    i += 1;
                }
            }
            St::Str => {
                colors[i] = STRING;
                if c == '\\' && i + 1 < chars.len() {
                    colors[i + 1] = STRING;
                    i += 2;
                } else {
                    if c == '"' {
                        state = St::Code;
                    }
                    i += 1;
                }
            }
            St::Char => {
                colors[i] = STRING;
                if c == '\\' && i + 1 < chars.len() {
                    colors[i + 1] = STRING;
                    i += 2;
                } else {
                    if c == '\'' {
                        state = St::Code;
                    }
                    i += 1;
                }
            }
            St::Preproc => {
                colors[i] = PREPROC;
                if c == '\n' {
                    state = St::Code;
                }
                i += 1;
            }
            St::Code => {
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    colors[i] = COMMENT;
                    colors[i + 1] = COMMENT;
                    state = St::LineComment;
                    i += 2;
                } else if c == '/' && chars.get(i + 1) == Some(&'*') {
                    colors[i] = COMMENT;
                    colors[i + 1] = COMMENT;
                    state = St::BlockComment;
                    i += 2;
                } else if c == '"' {
                    colors[i] = STRING;
                    state = St::Str;
                    i += 1;
                } else if c == '\'' {
                    colors[i] = STRING;
                    state = St::Char;
                    i += 1;
                } else if c == '#' && at_line_start(&chars, i) {
                    colors[i] = PREPROC;
                    state = St::Preproc;
                    i += 1;
                } else if c.is_ascii_digit() {
                    while i < chars.len()
                        && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_')
                    {
                        colors[i] = NUMBER;
                        i += 1;
                    }
                } else if c.is_alphabetic() || c == '_' {
                    let start = i;
                    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    let color = if KEYWORDS.contains(&word.as_str()) {
                        KEYWORD
                    } else if TYPES.contains(&word.as_str()) {
                        TYPE_NAME
                    } else {
                        PLAIN
                    };
                    for slot in colors.iter_mut().take(i).skip(start) {
                        *slot = color;
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    colors
}

/// True if every character back to the previous newline (or buffer start) is
/// whitespace — used to recognise `#include` etc. as a directive rather than
/// a stray `#` inside an expression.
fn at_line_start(chars: &[char], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        match chars[j] {
            ' ' | '\t' => continue,
            '\n' => return true,
            _ => return false,
        }
    }
    true
}
