//! A CSS parser covering the subset the renderer can actually honour.
//!
//! Selectors are simple: an optional tag name, plus any number of `#id` and
//! `.class` parts, optionally comma-separated into a list. Descendant and
//! child combinators are parsed but only their rightmost compound is matched,
//! which over-matches rather than under-matches — a page renders with too
//! much styling rather than none.
//!
//! At-rules (`@media`, `@font-face`, ...) are skipped wholesale, including
//! any block they carry.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::color::Color;

/// Guard against a hostile or merely enormous stylesheet.
const MAX_RULES: usize = 2000;

/// The size units resolve against, and the one font size the renderer has.
///
/// Viewport units are common on modern pages — `example.com` sets its column
/// with `width: 60vw` — so they are resolved for real rather than guessed at.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    /// Width of one character, for `ch`.
    pub char_w: f32,
    /// Height of one line, which stands in for the font size in `em`.
    pub line_h: f32,
}

pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

/// One compound selector: everything between two combinators.
#[derive(Debug, Default, Clone)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    /// `[name]` and `[name="value"]`, lowercased.
    pub attrs: Vec<(String, Option<String>)>,
}

impl Compound {
    fn specificity(&self) -> (usize, usize, usize) {
        (
            self.id.iter().count(),
            self.classes.len() + self.attrs.len(),
            self.tag.iter().count(),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// `a b` — anywhere below.
    Descendant,
    /// `a > b` — directly below.
    Child,
}

/// A full selector, such as `nav ul > li.active`.
///
/// `parts` reads left to right and the last compound is the subject — the
/// element the rule applies to. `combinators[i]` sits between `parts[i]` and
/// `parts[i + 1]`, so it always has one fewer entry than `parts`.
#[derive(Debug, Default, Clone)]
pub struct Selector {
    pub parts: Vec<Compound>,
    pub combinators: Vec<Combinator>,
}

impl Selector {
    /// The compound this selector applies to.
    pub fn subject(&self) -> Option<&Compound> {
        self.parts.last()
    }

    /// Cascade specificity, as (ids, classes, tags), summed over the parts.
    pub fn specificity(&self) -> (usize, usize, usize) {
        self.parts.iter().fold((0, 0, 0), |acc, part| {
            let s = part.specificity();
            (acc.0 + s.0, acc.1 + s.1, acc.2 + s.2)
        })
    }
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    /// A length in pixels. Other units are converted on the way in.
    Length(f32),
    /// A percentage of the containing block.
    Percent(f32),
    Color(Color),
}

impl Value {
    /// Resolve to pixels against a containing width, if this is a length.
    pub fn to_px(&self, containing: f32) -> Option<f32> {
        match self {
            Value::Length(px) => Some(*px),
            Value::Percent(p) => Some(containing * p / 100.0),
            Value::Keyword(k) if k == "auto" || k == "0" => Some(0.0),
            _ => None,
        }
    }

    pub fn keyword(&self) -> Option<&str> {
        match self {
            Value::Keyword(k) => Some(k.as_str()),
            _ => None,
        }
    }

    pub fn color(&self) -> Option<Color> {
        match self {
            Value::Color(c) => Some(*c),
            _ => None,
        }
    }
}

pub fn parse(source: &str, viewport: Viewport) -> Stylesheet {
    let mut parser = Parser { input: source.as_bytes(), pos: 0, viewport };
    let mut rules = Vec::new();

    loop {
        parser.skip_whitespace_and_comments();
        if parser.eof() {
            break;
        }
        if rules.len() >= MAX_RULES {
            break;
        }

        if parser.peek() == Some(b'@') {
            parser.skip_at_rule();
            continue;
        }

        match parser.parse_rule() {
            Some(rule) => rules.push(rule),
            // A malformed rule ends the sheet rather than risking a stall;
            // `parse_rule` only returns None when it cannot make progress.
            None => break,
        }
    }

    Stylesheet { rules }
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    viewport: Viewport,
}

impl<'a> Parser<'a> {
    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            let before = self.pos;
            while self.pos < self.input.len() && self.input[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.input[self.pos..].starts_with(b"/*") {
                match find(&self.input[self.pos..], b"*/") {
                    Some(i) => self.pos += i + 2,
                    None => self.pos = self.input.len(),
                }
            }
            if self.pos == before {
                return;
            }
        }
    }

    /// Skip an at-rule and its block, if it has one.
    fn skip_at_rule(&mut self) {
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b';' => {
                    self.pos += 1;
                    return;
                }
                b'{' => {
                    self.skip_block();
                    return;
                }
                _ => self.pos += 1,
            }
        }
    }

    /// Skip a `{ ... }` block, tracking nesting.
    fn skip_block(&mut self) {
        let mut depth = 0usize;
        while self.pos < self.input.len() {
            match self.input[self.pos] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    self.pos += 1;
                    if depth == 0 {
                        return;
                    }
                    continue;
                }
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn parse_rule(&mut self) -> Option<Rule> {
        let start = self.pos;
        let selectors = self.parse_selectors();

        if self.peek() != Some(b'{') {
            // No block: skip to the next one so a broken selector list does
            // not swallow the rest of the sheet.
            if self.pos == start {
                self.pos += 1;
            }
            return Some(Rule { selectors, declarations: Vec::new() });
        }
        self.pos += 1; // '{'

        let declarations = self.parse_declarations();
        Some(Rule { selectors, declarations })
    }

    fn parse_selectors(&mut self) -> Vec<Selector> {
        let mut out = Vec::new();
        let start = self.pos;
        while self.pos < self.input.len() && self.input[self.pos] != b'{' {
            self.pos += 1;
        }
        let text = String::from_utf8_lossy(&self.input[start..self.pos]);

        for group in text.split(',') {
            if let Some(sel) = parse_selector(group) {
                out.push(sel);
            }
        }
        out
    }

    fn parse_declarations(&mut self) -> Vec<Declaration> {
        let mut out = Vec::new();

        loop {
            self.skip_whitespace_and_comments();
            match self.peek() {
                None => break,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }

            // Name up to ':'.
            let name_start = self.pos;
            while self.pos < self.input.len()
                && self.input[self.pos] != b':'
                && self.input[self.pos] != b'}'
                && self.input[self.pos] != b';'
            {
                self.pos += 1;
            }
            let name = String::from_utf8_lossy(&self.input[name_start..self.pos])
                .trim()
                .to_ascii_lowercase();

            if self.peek() != Some(b':') {
                // Malformed; drop to the next declaration.
                if self.peek() == Some(b';') {
                    self.pos += 1;
                }
                if name.is_empty() {
                    break;
                }
                continue;
            }
            self.pos += 1; // ':'

            // Value up to ';' or '}', respecting parentheses so `rgb(1, 2, 3)`
            // survives.
            let value_start = self.pos;
            let mut paren = 0usize;
            while self.pos < self.input.len() {
                match self.input[self.pos] {
                    b'(' => paren += 1,
                    b')' => paren = paren.saturating_sub(1),
                    b';' | b'}' if paren == 0 => break,
                    _ => {}
                }
                self.pos += 1;
            }
            let raw = String::from_utf8_lossy(&self.input[value_start..self.pos])
                .trim()
                .to_string();
            if self.peek() == Some(b';') {
                self.pos += 1;
            }

            if name.is_empty() {
                continue;
            }

            let important = raw.to_ascii_lowercase().contains("!important");
            let cleaned = raw
                .split('!')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();

            // Shorthands expand into the longhands the layout code reads.
            expand(&name, &cleaned, important, self.viewport, &mut out);
        }

        out
    }
}

/// Parse a full selector such as `nav ul > li.active`.
///
/// Public because `querySelector` needs the same parser the stylesheet uses.
///
/// The sibling combinators `+` and `~` are parsed as descendant, which
/// over-matches rather than under-matches: a page ends up with slightly too
/// much styling rather than none of it.
pub fn parse_selector(text: &str) -> Option<Selector> {
    let mut sel = Selector::default();
    let mut pending = Combinator::Descendant;

    // Split on whitespace and on the combinator characters, keeping track of
    // which combinator separated each pair.
    let mut token = String::new();
    let mut chars = text.chars().peekable();
    let mut saw_combinator = false;

    while let Some(c) = chars.next() {
        match c {
            '>' | '+' | '~' => {
                push_compound(&mut sel, &token, pending, &mut saw_combinator);
                token.clear();
                pending = if c == '>' { Combinator::Child } else { Combinator::Descendant };
                saw_combinator = true;
            }
            c if c.is_whitespace() => {
                if !token.is_empty() {
                    push_compound(&mut sel, &token, pending, &mut saw_combinator);
                    token.clear();
                    pending = Combinator::Descendant;
                    saw_combinator = true;
                }
            }
            _ => token.push(c),
        }
    }
    push_compound(&mut sel, &token, pending, &mut saw_combinator);

    if sel.parts.is_empty() {
        return None;
    }
    Some(sel)
}

fn push_compound(sel: &mut Selector, token: &str, combinator: Combinator, pending: &mut bool) {
    let Some(compound) = parse_compound(token) else { return };
    if !sel.parts.is_empty() {
        sel.combinators.push(combinator);
    }
    sel.parts.push(compound);
    *pending = false;
}

/// Parse one compound selector such as `div#main.wide[type="text"]`.
fn parse_compound(text: &str) -> Option<Compound> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let mut compound = Compound::default();

    // Attribute selectors are pulled out first so their contents cannot be
    // mistaken for class or id parts.
    let mut rest = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '[' {
            rest.push(c);
            continue;
        }
        let mut inner = String::new();
        for c in chars.by_ref() {
            if c == ']' {
                break;
            }
            inner.push(c);
        }
        if let Some(attr) = parse_attribute(&inner) {
            compound.attrs.push(attr);
        }
    }

    // Pseudo-classes and pseudo-elements are not supported; dropping them
    // leaves the element part, which still matches usefully.
    let rest = rest.split(':').next().unwrap_or("").to_string();
    if rest.is_empty() || rest == "*" {
        return Some(compound);
    }

    let mut current = String::new();
    let mut mode = b' ';
    for c in rest.chars() {
        match c {
            '#' | '.' => {
                commit(&mut compound, mode, &mut current);
                mode = if c == '#' { b'#' } else { b'.' };
            }
            _ => current.push(c),
        }
    }
    commit(&mut compound, mode, &mut current);

    Some(compound)
}

/// Parse the inside of `[...]`. Only presence and exact equality are honoured;
/// the substring forms (`^=`, `*=`, `$=`) are treated as presence tests.
fn parse_attribute(inner: &str) -> Option<(String, Option<String>)> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    match inner.find('=') {
        None => Some((inner.to_ascii_lowercase(), None)),
        Some(eq) => {
            let (raw_name, raw_value) = inner.split_at(eq);
            let name = raw_name.trim_end_matches(['^', '*', '$', '|', '~']).trim();
            if name.is_empty() {
                return None;
            }
            let exact = raw_name.len() == name.len();
            let value = raw_value[1..].trim().trim_matches(['"', '\'']).to_ascii_lowercase();
            Some((
                name.to_ascii_lowercase(),
                if exact { Some(value) } else { None },
            ))
        }
    }
}

fn commit(compound: &mut Compound, mode: u8, current: &mut String) {
    if current.is_empty() {
        return;
    }
    let value = core::mem::take(current).to_ascii_lowercase();
    match mode {
        b'#' => compound.id = Some(value),
        b'.' => compound.classes.push(value),
        _ => compound.tag = Some(value),
    }
}

/// Turn a declaration into one or more longhand declarations.
fn expand(
    name: &str,
    raw: &str,
    important: bool,
    viewport: Viewport,
    out: &mut Vec<Declaration>,
) {
    let mut push = |n: &str, v: Value| {
        out.push(Declaration { name: n.to_string(), value: v, important });
    };

    match name {
        // `margin: a b c d` and its shorter forms.
        "margin" | "padding" => {
            let parts: Vec<&str> = raw.split_whitespace().collect();
            let sides = expand_box(&parts, viewport);
            for (side, value) in ["top", "right", "bottom", "left"].iter().zip(sides) {
                if let Some(v) = value {
                    push(&alloc::format!("{}-{}", name, side), v);
                }
            }
        }
        "border" => {
            // Only the width and colour of a uniform border are used.
            for token in raw.split_whitespace() {
                if let Some(v) = parse_value(token, viewport) {
                    match v {
                        Value::Length(_) => {
                            for side in ["top", "right", "bottom", "left"] {
                                push(&alloc::format!("border-{}-width", side), v.clone());
                            }
                        }
                        Value::Color(_) => push("border-color", v),
                        _ => {}
                    }
                }
            }
        }
        "border-width" => {
            let parts: Vec<&str> = raw.split_whitespace().collect();
            let sides = expand_box(&parts, viewport);
            for (side, value) in ["top", "right", "bottom", "left"].iter().zip(sides) {
                if let Some(v) = value {
                    push(&alloc::format!("border-{}-width", side), v);
                }
            }
        }
        "background" => {
            // Of the background shorthand, only a flat colour is drawable.
            for token in raw.split_whitespace() {
                if let Some(v @ Value::Color(_)) = parse_value(token, viewport) {
                    push("background-color", v);
                    break;
                }
            }
        }
        "font" => {
            if raw.contains("bold") {
                push("font-weight", Value::Keyword("bold".to_string()));
            }
        }
        _ => {
            if let Some(v) = parse_value(raw, viewport) {
                push(name, v);
            }
        }
    }
}

/// Apply the 1-to-4 value box shorthand rules.
fn expand_box(parts: &[&str], viewport: Viewport) -> [Option<Value>; 4] {
    let v = |s: &str| parse_value(s, viewport);
    match parts.len() {
        1 => {
            let a = v(parts[0]);
            [a.clone(), a.clone(), a.clone(), a]
        }
        2 => {
            let (a, b) = (v(parts[0]), v(parts[1]));
            [a.clone(), b.clone(), a, b]
        }
        3 => {
            let (a, b, c) = (v(parts[0]), v(parts[1]), v(parts[2]));
            [a, b.clone(), c, b]
        }
        n if n >= 4 => [v(parts[0]), v(parts[1]), v(parts[2]), v(parts[3])],
        _ => [None, None, None, None],
    }
}

/// Parse a single component value.
pub fn parse_value(raw: &str, viewport: Viewport) -> Option<Value> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(c) = parse_color(s) {
        return Some(Value::Color(c));
    }

    if let Some(num) = s.strip_suffix('%') {
        if let Some(n) = parse_number(num) {
            return Some(Value::Percent(n));
        }
    }
    // A unit suffix only makes this a length if what precedes it is a number.
    // Keywords ending in a unit — `list-item`, `inline-flex` — must not be
    // mistaken for one and discarded.
    //
    // Longer suffixes come first so `rem` is not read as `em`, and `vmin` not
    // as `vh`'s sibling `vm`.
    let vmin = viewport.width.min(viewport.height) / 100.0;
    let vmax = viewport.width.max(viewport.height) / 100.0;
    for (suffix, scale) in [
        ("vmin", vmin),
        ("vmax", vmax),
        ("rem", viewport.line_h),
        ("px", 1.0),
        // The renderer has a single font size, so font-relative units all
        // resolve against the one line height it draws with.
        ("em", viewport.line_h),
        ("pt", 96.0 / 72.0),
        ("ex", viewport.line_h / 2.0),
        ("ch", viewport.char_w),
        ("vw", viewport.width / 100.0),
        ("vh", viewport.height / 100.0),
    ] {
        if let Some(num) = s.strip_suffix(suffix) {
            if let Some(n) = parse_number(num) {
                return Some(Value::Length(n * scale));
            }
        }
    }
    if let Some(n) = parse_number(s) {
        return Some(Value::Length(n));
    }

    Some(Value::Keyword(s.to_string()))
}

/// Parse a decimal number without `str::parse::<f32>`, which needs more of
/// core's float formatting machinery than this target links.
fn parse_number(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };

    let mut whole: f32 = 0.0;
    let mut frac: f32 = 0.0;
    let mut scale: f32 = 0.1;
    let mut seen_digit = false;
    let mut in_frac = false;

    for c in digits.chars() {
        match c {
            '0'..='9' => {
                seen_digit = true;
                let d = (c as u8 - b'0') as f32;
                if in_frac {
                    frac += d * scale;
                    scale *= 0.1;
                } else {
                    whole = whole * 10.0 + d;
                }
            }
            '.' if !in_frac => in_frac = true,
            _ => return None,
        }
    }

    if !seen_digit {
        return None;
    }
    let value = whole + frac;
    Some(if negative { -value } else { value })
}

/// Parse `#rgb`, `#rrggbb`, `rgb()`/`rgba()`, and the common colour keywords.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();

    if let Some(hex) = s.strip_prefix('#') {
        return match hex.len() {
            3 => {
                let r = hex_digit(hex.as_bytes()[0])?;
                let g = hex_digit(hex.as_bytes()[1])?;
                let b = hex_digit(hex.as_bytes()[2])?;
                // #abc means #aabbcc.
                Some(Color::rgb(r * 17, g * 17, b * 17))
            }
            6 | 8 => {
                let b = hex.as_bytes();
                let r = hex_digit(b[0])? * 16 + hex_digit(b[1])?;
                let g = hex_digit(b[2])? * 16 + hex_digit(b[3])?;
                let bl = hex_digit(b[4])? * 16 + hex_digit(b[5])?;
                Some(Color::rgb(r, g, bl))
            }
            _ => None,
        };
    }

    if let Some(rest) = s.strip_prefix("rgb(").or_else(|| s.strip_prefix("rgba(")) {
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        let mut it = inner.split([',', ' ', '/']).filter(|p| !p.trim().is_empty());
        let r = parse_number(it.next()?)? as i32;
        let g = parse_number(it.next()?)? as i32;
        let b = parse_number(it.next()?)? as i32;
        return Some(Color::rgb(
            r.clamp(0, 255) as u8,
            g.clamp(0, 255) as u8,
            b.clamp(0, 255) as u8,
        ));
    }

    named_color(s)
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn named_color(name: &str) -> Option<Color> {
    let c = match name {
        "black" => 0x000000,
        "silver" => 0xC0C0C0,
        "gray" | "grey" => 0x808080,
        "white" => 0xFFFFFF,
        "maroon" => 0x800000,
        "red" => 0xFF0000,
        "purple" => 0x800080,
        "fuchsia" | "magenta" => 0xFF00FF,
        "green" => 0x008000,
        "lime" => 0x00FF00,
        "olive" => 0x808000,
        "yellow" => 0xFFFF00,
        "navy" => 0x000080,
        "blue" => 0x0000FF,
        "teal" => 0x008080,
        "aqua" | "cyan" => 0x00FFFF,
        "orange" => 0xFFA500,
        "pink" => 0xFFC0CB,
        "brown" => 0xA52A2A,
        "gold" => 0xFFD700,
        "indigo" => 0x4B0082,
        "violet" => 0xEE82EE,
        "beige" => 0xF5F5DC,
        "ivory" => 0xFFFFF0,
        "khaki" => 0xF0E68C,
        "lavender" => 0xE6E6FA,
        "salmon" => 0xFA8072,
        "tan" => 0xD2B48C,
        "turquoise" => 0x40E0D0,
        "crimson" => 0xDC143C,
        "darkblue" => 0x00008B,
        "darkgreen" => 0x006400,
        "darkred" => 0x8B0000,
        "darkgray" | "darkgrey" => 0xA9A9A9,
        "lightgray" | "lightgrey" => 0xD3D3D3,
        "lightblue" => 0xADD8E6,
        "whitesmoke" => 0xF5F5F5,
        "gainsboro" => 0xDCDCDC,
        "steelblue" => 0x4682B4,
        "royalblue" => 0x4169E1,
        "dodgerblue" => 0x1E90FF,
        "slategray" | "slategrey" => 0x708090,
        "transparent" => return None,
        _ => return None,
    };
    Some(Color::hex(c))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
