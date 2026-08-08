//! The style tree: DOM nodes paired with their computed declarations.
//!
//! Rules are matched against each element, sorted by origin and specificity,
//! and applied in order. Inherited properties are passed down from the parent,
//! which is what makes `body { color: ... }` reach the text inside it.
//!
//! Matching keeps an ancestor stack as it descends, so `nav ul > li` can be
//! evaluated properly rather than being reduced to its rightmost compound.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::color::Color;
use crate::framebuffer::TextSize;

use super::css::{Combinator, Compound, Declaration, Selector, Stylesheet, Value, Viewport};
use super::dom::{ElementData, Node, NodeId, NodeKind, MAX_DEPTH, NO_NODE};
use super::layout::TextStyle;

/// Properties a child takes from its parent unless it sets its own.
const INHERITED: [&str; 8] = [
    "color",
    "font-weight",
    "font-style",
    "font-size",
    "text-align",
    "text-decoration",
    "white-space",
    "list-style-type",
];

/// Body text colour when nothing says otherwise.
const DEFAULT_TEXT: Color = Color::hex(0x1E293B);
/// Links get the conventional blue when the page does not colour them.
const DEFAULT_LINK: Color = Color::hex(0x1D4ED8);

pub type PropertyMap = BTreeMap<String, Value>;

pub struct StyledNode<'a> {
    pub node: &'a Node,
    pub specified: PropertyMap,
    pub children: Vec<StyledNode<'a>>,
}

impl<'a> StyledNode<'a> {
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.specified.get(name)
    }

    pub fn keyword(&self, name: &str) -> Option<&str> {
        self.value(name).and_then(|v| v.keyword())
    }

    pub fn color(&self, name: &str) -> Option<Color> {
        self.value(name).and_then(|v| v.color())
    }

    pub fn length(&self, name: &str, containing: f32) -> f32 {
        self.value(name)
            .and_then(|v| v.to_px(containing))
            .unwrap_or(0.0)
    }

    /// The declared value of a property that only means anything as a length,
    /// left unresolved.
    ///
    /// A keyword counts as absent, which is the distinction a replaced element
    /// needs: `width: auto` is not zero, it is a request to work the width out
    /// from the picture instead. The value is handed back rather than resolved
    /// because a percentage is a share of the containing block, and an inline
    /// image does not learn that until its line is wrapped.
    pub fn length_value(&self, name: &str) -> Option<&Value> {
        match self.value(name) {
            Some(value) if value.keyword().is_none() => Some(value),
            _ => None,
        }
    }

    /// `display`, defaulting to inline the way CSS does.
    pub fn display(&self) -> Display {
        match self.keyword("display") {
            Some("block" | "flex" | "grid" | "table" | "flow-root") => Display::Block,
            // Row groups have no layout of their own here: their rows behave
            // as if they were children of the table.
            Some("table-row-group" | "table-header-group" | "table-footer-group") => {
                Display::Block
            }
            Some("table-row") => Display::TableRow,
            Some("table-cell") => Display::TableCell,
            Some("none") => Display::None,
            Some("list-item") => Display::ListItem,
            Some(_) => Display::Inline,
            None => Display::Inline,
        }
    }

    pub fn is_bold(&self) -> bool {
        matches!(
            self.keyword("font-weight"),
            Some("bold" | "bolder" | "600" | "700" | "800" | "900")
        )
    }

    pub fn is_underlined(&self) -> bool {
        matches!(self.keyword("text-decoration"), Some(d) if d.contains("underline"))
    }

    pub fn is_struck_through(&self) -> bool {
        matches!(
            self.keyword("text-decoration"),
            Some(d) if d.contains("line-through")
        )
    }

    /// The nearest drawable size to this node's `font-size`.
    pub fn font_size(&self) -> TextSize {
        match self.value("font-size").and_then(|v| v.to_px(16.0)) {
            Some(px) if px > 0.0 => TextSize::nearest(px),
            _ => TextSize::Normal,
        }
    }

    pub fn text_align(&self) -> TextAlign {
        match self.keyword("text-align") {
            Some("center") => TextAlign::Center,
            Some("right" | "end") => TextAlign::Right,
            _ => TextAlign::Left,
        }
    }

    /// Everything the painter needs to draw this node's text.
    ///
    /// `owner` is the element the text belongs to, which is what a click has
    /// to be dispatched against; a text node is not an event target.
    pub fn text_style(&self, link: Option<usize>, owner: NodeId) -> TextStyle {
        let default = if link.is_some() { DEFAULT_LINK } else { DEFAULT_TEXT };
        TextStyle {
            color: self.color("color").unwrap_or(default),
            size: self.font_size(),
            bold: self.is_bold(),
            underline: self.is_underlined() || link.is_some(),
            strike: self.is_struck_through(),
            background: self.color("background-color"),
            link,
            node: owner,
        }
    }

    /// The element id to attribute this node's content to.
    pub fn owner(&self) -> NodeId {
        match self.node.kind {
            NodeKind::Element(_) => self.node.id,
            NodeKind::Text(_) => NO_NODE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Inline,
    Block,
    ListItem,
    /// Lays its children out side by side rather than stacked.
    TableRow,
    /// A block that takes its width from the column it sits in.
    TableCell,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Build the style tree.
///
/// `author` is the page's own CSS, applied after the user-agent sheet so it
/// wins ties.
pub fn build<'a>(
    root: &'a Node,
    ua: &Stylesheet,
    author: &Stylesheet,
    viewport: Viewport,
) -> StyledNode<'a> {
    let mut ancestors: Vec<&ElementData> = Vec::new();
    build_inner(root, ua, author, viewport, &PropertyMap::new(), &mut ancestors, 0)
}

fn build_inner<'a>(
    node: &'a Node,
    ua: &Stylesheet,
    author: &Stylesheet,
    viewport: Viewport,
    inherited: &PropertyMap,
    ancestors: &mut Vec<&'a ElementData>,
    depth: usize,
) -> StyledNode<'a> {
    let specified = match &node.kind {
        NodeKind::Element(_) => {
            specified_values(node, ua, author, viewport, inherited, ancestors)
        }
        // Text nodes have no rules of their own; they render with whatever
        // the enclosing element resolved to.
        NodeKind::Text(_) => inherited.clone(),
    };

    let children = if depth >= MAX_DEPTH {
        Vec::new()
    } else {
        let for_children = inheritable(&specified);
        if let NodeKind::Element(element) = &node.kind {
            ancestors.push(element);
        }
        let built = node
            .children
            .iter()
            .map(|c| {
                build_inner(c, ua, author, viewport, &for_children, ancestors, depth + 1)
            })
            .collect();
        if matches!(node.kind, NodeKind::Element(_)) {
            ancestors.pop();
        }
        built
    };

    StyledNode { node, specified, children }
}

fn inheritable(from: &PropertyMap) -> PropertyMap {
    let mut out = PropertyMap::new();
    for key in INHERITED {
        if let Some(v) = from.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    out
}

fn specified_values(
    node: &Node,
    ua: &Stylesheet,
    author: &Stylesheet,
    viewport: Viewport,
    inherited: &PropertyMap,
    ancestors: &[&ElementData],
) -> PropertyMap {
    let mut values = inherited.clone();

    // Matched rules, weakest first: user agent, then author, each ordered by
    // specificity and then document order.
    let mut matched: Vec<(bool, usize, (usize, usize, usize), usize, &Declaration)> = Vec::new();
    for (origin, sheet) in [(0usize, ua), (1usize, author)] {
        for (index, rule) in sheet.rules.iter().enumerate() {
            let Some(spec) = best_match(node, ancestors, &rule.selectors) else { continue };
            for decl in &rule.declarations {
                matched.push((decl.important, origin, spec, index, decl));
            }
        }
    }

    matched.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });

    for (_, _, _, _, decl) in matched {
        values.insert(decl.name.clone(), decl.value.clone());
    }

    // An inline `style` attribute beats every rule.
    if let Some(inline) = node.as_element().and_then(|e| e.attr("style")) {
        let sheet = super::css::parse(&alloc::format!("*{{{}}}", inline), viewport);
        for rule in &sheet.rules {
            for decl in &rule.declarations {
                values.insert(decl.name.clone(), decl.value.clone());
            }
        }
    }

    values
}

/// Specificity of the most specific selector in the list that matches.
fn best_match(
    node: &Node,
    ancestors: &[&ElementData],
    selectors: &[Selector],
) -> Option<(usize, usize, usize)> {
    selectors
        .iter()
        .filter(|s| matches(node, ancestors, s))
        .map(|s| s.specificity())
        .max()
}

/// Does this selector match the element, given the chain above it?
///
/// The subject is checked first, then the remaining compounds are matched
/// right to left against the ancestor stack — the same order a real engine
/// uses, and the reason it can bail out early on the common case of a
/// mismatched tag name.
pub fn matches(node: &Node, ancestors: &[&ElementData], selector: &Selector) -> bool {
    let Some(element) = node.as_element() else { return false };
    let Some(subject) = selector.subject() else { return false };
    if !matches_compound(element, subject) {
        return false;
    }

    // Walk leftwards through the remaining compounds.
    let mut remaining = ancestors.len();
    for i in (0..selector.parts.len() - 1).rev() {
        let compound = &selector.parts[i];
        let combinator = selector.combinators.get(i).copied().unwrap_or(Combinator::Descendant);

        match combinator {
            Combinator::Child => {
                if remaining == 0 {
                    return false;
                }
                remaining -= 1;
                if !matches_compound(ancestors[remaining], compound) {
                    return false;
                }
            }
            Combinator::Descendant => {
                let mut found = false;
                while remaining > 0 {
                    remaining -= 1;
                    if matches_compound(ancestors[remaining], compound) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
            }
        }
    }

    true
}

fn matches_compound(element: &ElementData, compound: &Compound) -> bool {
    if let Some(tag) = &compound.tag {
        if !element.tag.eq_ignore_ascii_case(tag) {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        match element.id() {
            Some(actual) if actual.eq_ignore_ascii_case(id) => {}
            _ => return false,
        }
    }
    for class in &compound.classes {
        if !element.classes().any(|c| c.eq_ignore_ascii_case(class)) {
            return false;
        }
    }
    for (name, expected) in &compound.attrs {
        match (element.attr(name), expected) {
            (None, _) => return false,
            (Some(_), None) => {}
            (Some(actual), Some(want)) if actual.eq_ignore_ascii_case(want) => {}
            _ => return false,
        }
    }
    true
}
