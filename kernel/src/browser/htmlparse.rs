//! HTML source into a [`Node`] tree.
//!
//! Not spec-compliant — the WHATWG parsing algorithm is enormous — but
//! forgiving in the ways real pages need: void elements never take children,
//! implicitly closed tags (`<li>` after `<li>`, `<p>` after `<p>`) are
//! handled, a stray close tag for something that is not open is ignored, and
//! anything still open at the end of input is closed.
//!
//! It never panics and never recurses: the tree is built with an explicit
//! stack, so a pathologically nested document costs heap, not stack, and the
//! heap use is capped.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::dom::{ElementData, Node, NodeKind, MAX_DEPTH, MAX_NODES, NO_NODE};
use super::entities::decode_entities;

/// Elements that are always empty, so `<br>` does not swallow the document.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
    "param", "source", "track", "wbr",
];

/// Elements whose content is raw text, not markup.
const RAW_TEXT: [&str; 3] = ["script", "style", "textarea"];

/// Tags that close an open one of the same kind, per HTML's optional end tags.
fn implicitly_closes(open: &str, new: &str) -> bool {
    match new {
        "li" => open == "li",
        "p" => matches!(
            open,
            "p" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
        ),
        "dt" | "dd" => matches!(open, "dt" | "dd"),
        "tr" => matches!(open, "tr" | "td" | "th"),
        "td" | "th" => matches!(open, "td" | "th"),
        "option" => open == "option",
        _ => false,
    }
}

struct OpenElement {
    tag: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

pub fn parse(input: &str) -> Node {
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let mut node_count = 0usize;

    // The root is an implicit element so there is always somewhere to put
    // content, even for a fragment with no <html>.
    let mut stack: Vec<OpenElement> = alloc::vec![OpenElement {
        tag: "document".to_string(),
        attrs: Vec::new(),
        children: Vec::new(),
    }];

    while pos < bytes.len() {
        if bytes[pos] == b'<' {
            // A '<' that does not begin a tag is literal text.
            match classify(bytes, pos) {
                Markup::Comment => {
                    pos = skip_comment(bytes, pos);
                    continue;
                }
                Markup::Declaration => {
                    pos = skip_to_gt(bytes, pos);
                    continue;
                }
                Markup::CloseTag => {
                    let (tag, next) = read_close_tag(bytes, pos);
                    pos = next;
                    close_tag(&mut stack, &tag);
                    continue;
                }
                Markup::OpenTag => {
                    let (tag, attrs, self_closing, next) = read_open_tag(bytes, pos);
                    pos = next;

                    if node_count >= MAX_NODES {
                        continue;
                    }
                    node_count += 1;

                    // Raw-text elements keep their contents verbatim; markup
                    // inside <script> must not become nodes.
                    if RAW_TEXT.contains(&tag.as_str()) {
                        let (text, next) = read_raw_text(bytes, pos, &tag);
                        pos = next;
                        push_child(
                            &mut stack,
                            Node::element(tag, attrs, alloc::vec![Node::text(text)]),
                        );
                        continue;
                    }

                    if self_closing || VOID.contains(&tag.as_str()) {
                        push_child(&mut stack, Node::element(tag, attrs, Vec::new()));
                        continue;
                    }

                    // Close anything this tag implicitly ends.
                    while stack.len() > 1 {
                        let open = stack[stack.len() - 1].tag.as_str();
                        if implicitly_closes(open, &tag) {
                            pop_into_parent(&mut stack);
                        } else {
                            break;
                        }
                    }

                    if stack.len() >= MAX_DEPTH {
                        // Too deep to lay out later; keep the content by
                        // flattening it into the current parent instead.
                        continue;
                    }
                    stack.push(OpenElement { tag, attrs, children: Vec::new() });
                    continue;
                }
                Markup::Text => {}
            }
        }

        let (text, next) = read_text(bytes, pos);
        pos = next;
        if !text.is_empty() && node_count < MAX_NODES {
            node_count += 1;
            push_child(&mut stack, Node::text(decode_entities(&text)));
        }
    }

    // Close whatever the document left open.
    while stack.len() > 1 {
        pop_into_parent(&mut stack);
    }

    let root = stack.pop().unwrap_or(OpenElement {
        tag: "document".to_string(),
        attrs: Vec::new(),
        children: Vec::new(),
    });
    Node {
        // Ids are handed out by `Node::assign_ids` once the tree is complete.
        id: NO_NODE,
        children: root.children,
        kind: NodeKind::Element(ElementData { tag: root.tag, attrs: root.attrs }),
    }
}

fn push_child(stack: &mut Vec<OpenElement>, node: Node) {
    if let Some(top) = stack.last_mut() {
        top.children.push(node);
    }
}

fn pop_into_parent(stack: &mut Vec<OpenElement>) {
    let Some(done) = stack.pop() else { return };
    let node = Node::element(done.tag, done.attrs, done.children);
    push_child(stack, node);
}

/// Close the nearest open element with this tag, ignoring the tag entirely
/// if nothing matching is open.
fn close_tag(stack: &mut Vec<OpenElement>, tag: &str) {
    let Some(depth) = stack.iter().rposition(|e| e.tag == tag) else {
        return;
    };
    if depth == 0 {
        return; // never close the implicit root
    }
    while stack.len() > depth {
        pop_into_parent(stack);
    }
}

enum Markup {
    OpenTag,
    CloseTag,
    Comment,
    Declaration,
    Text,
}

fn classify(bytes: &[u8], pos: usize) -> Markup {
    match bytes.get(pos + 1) {
        Some(b'!') => {
            if bytes[pos..].starts_with(b"<!--") {
                Markup::Comment
            } else {
                Markup::Declaration
            }
        }
        Some(b'?') => Markup::Declaration,
        Some(b'/') => {
            if matches!(bytes.get(pos + 2), Some(c) if c.is_ascii_alphabetic()) {
                Markup::CloseTag
            } else {
                Markup::Text
            }
        }
        Some(c) if c.is_ascii_alphabetic() => Markup::OpenTag,
        // A bare '<' followed by anything else is text, as in `a < b`.
        _ => Markup::Text,
    }
}

fn skip_comment(bytes: &[u8], pos: usize) -> usize {
    match find(&bytes[pos..], b"-->") {
        Some(i) => pos + i + 3,
        None => bytes.len(),
    }
}

fn skip_to_gt(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && bytes[pos] != b'>' {
        pos += 1;
    }
    pos + 1
}

fn read_close_tag(bytes: &[u8], pos: usize) -> (String, usize) {
    let start = pos + 2;
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'-') {
        end += 1;
    }
    let tag = lower(&bytes[start..end]);
    (tag, skip_to_gt(bytes, end))
}

/// Read `<tag attr="value" ...>`, returning the name, attributes, whether it
/// was self-closing, and the offset just past the tag.
fn read_open_tag(bytes: &[u8], pos: usize) -> (String, Vec<(String, String)>, bool, usize) {
    let start = pos + 1;
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    let tag = lower(&bytes[start..i]);
    let mut attrs = Vec::new();
    let mut self_closing = false;

    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        match bytes.get(i) {
            None => break,
            Some(b'>') => {
                i += 1;
                break;
            }
            Some(b'/') => {
                self_closing = true;
                i += 1;
                continue;
            }
            _ => {}
        }

        // Attribute name.
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if i == name_start {
            // Made no progress; skip a byte so this cannot loop forever.
            i += 1;
            continue;
        }
        let name = lower(&bytes[name_start..i]);

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }

        let value = if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => {
                    i += 1;
                    let vs = i;
                    while i < bytes.len() && bytes[i] != q {
                        i += 1;
                    }
                    let v = String::from_utf8_lossy(&bytes[vs..i]).into_owned();
                    i += 1; // closing quote
                    v
                }
                _ => {
                    let vs = i;
                    while i < bytes.len()
                        && !bytes[i].is_ascii_whitespace()
                        && bytes[i] != b'>'
                    {
                        i += 1;
                    }
                    String::from_utf8_lossy(&bytes[vs..i]).into_owned()
                }
            }
        } else {
            // A valueless attribute, like `disabled`.
            String::new()
        };

        if attrs.len() < 32 {
            attrs.push((name, decode_entities(&value)));
        }
    }

    (tag, attrs, self_closing, i)
}

/// Everything up to the matching close tag, uninterpreted.
fn read_raw_text(bytes: &[u8], pos: usize, tag: &str) -> (String, usize) {
    let needle = alloc::format!("</{}", tag);
    match find_ci(&bytes[pos..], needle.as_bytes()) {
        Some(i) => {
            let text = String::from_utf8_lossy(&bytes[pos..pos + i]).into_owned();
            (text, skip_to_gt(bytes, pos + i))
        }
        None => (
            String::from_utf8_lossy(&bytes[pos..]).into_owned(),
            bytes.len(),
        ),
    }
}

fn read_text(bytes: &[u8], pos: usize) -> (String, usize) {
    let mut end = pos;
    // Always consume at least one byte, so a literal '<' cannot stall.
    if end < bytes.len() && bytes[end] == b'<' {
        end += 1;
    }
    while end < bytes.len() && bytes[end] != b'<' {
        end += 1;
    }
    (String::from_utf8_lossy(&bytes[pos..end]).into_owned(), end)
}

fn lower(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_ascii_lowercase()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}
