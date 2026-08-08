//! The document object model.
//!
//! A plain tree of element and text nodes. Nodes own their children rather
//! than being held in an arena with indices: the tree is built once per page
//! and thrown away on the next navigation, so there is nothing to gain from
//! the extra indirection.

use alloc::string::String;
use alloc::vec::Vec;

/// Hard ceilings, because the input is untrusted. A page cannot be allowed
/// to exhaust the kernel heap or run the recursive passes off the stack.
pub const MAX_NODES: usize = 20_000;
pub const MAX_DEPTH: usize = 48;

/// How many `<script>` elements one document may have honoured. Each one is an
/// evaluation with its own time budget, and an external one is a blocking
/// request on the thread the GUI runs on, so an unbounded count is an unbounded
/// freeze.
pub const MAX_SCRIPTS: usize = 32;

/// One `<script>` element, as the page lifecycle needs to see it.
pub struct ScriptRef {
    /// Which element it was, so a diagnostic can say where.
    pub node: NodeId,
    /// The `src` attribute, exactly as written. `None` for an inline script.
    pub src: Option<String>,
    /// The element's own text. Empty when `src` is set.
    pub source: String,
    pub defer: bool,
    pub is_async: bool,
}

/// A stable identity for a node within one document.
///
/// Scripts hold references to elements and the layout keeps track of which
/// element each run of text came from, but the tree is rebuilt on every
/// relayout and `Node` values move when their parent's `Vec` grows. An id
/// assigned at parse time survives all of that; [`Node::by_id`] turns one back
/// into a reference.
pub type NodeId = usize;

/// The id given to content that belongs to no element, such as a list marker.
pub const NO_NODE: NodeId = usize::MAX;

pub struct Node {
    pub id: NodeId,
    pub children: Vec<Node>,
    pub kind: NodeKind,
}

pub enum NodeKind {
    Text(String),
    Element(ElementData),
}

pub struct ElementData {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
}

impl ElementData {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// The `class` attribute split on whitespace.
    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_whitespace()
    }

    /// Set an attribute, replacing any existing one of the same name.
    pub fn set_attr(&mut self, name: &str, value: &str) {
        let name = name.to_ascii_lowercase();
        if let Some((_, slot)) = self.attrs.iter_mut().find(|(k, _)| *k == name) {
            *slot = value.into();
        } else if self.attrs.len() < 64 {
            self.attrs.push((name, value.into()));
        }
    }

    pub fn remove_attr(&mut self, name: &str) {
        self.attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
    }
}

impl Node {
    pub fn text(data: String) -> Node {
        Node { id: NO_NODE, children: Vec::new(), kind: NodeKind::Text(data) }
    }

    pub fn element(tag: String, attrs: Vec<(String, String)>, children: Vec<Node>) -> Node {
        Node { id: NO_NODE, children, kind: NodeKind::Element(ElementData { tag, attrs }) }
    }

    /// Give an id to every node in the tree that does not have one.
    ///
    /// Called after parsing and again whenever a script has added nodes.
    /// Existing ids are left alone: a script may be holding a reference to an
    /// element, and renumbering the tree would silently repoint it at
    /// something else. Anything below [`MAX_DEPTH`] keeps [`NO_NODE`] and is
    /// therefore unreachable from script, which is the same treatment layout
    /// gives it.
    pub fn assign_ids(&mut self, next: &mut NodeId) {
        self.assign_ids_inner(next, 0);
    }

    fn assign_ids_inner(&mut self, next: &mut NodeId, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        if self.id == NO_NODE {
            self.id = *next;
            *next += 1;
        }
        for child in self.children.iter_mut() {
            child.assign_ids_inner(next, depth + 1);
        }
    }

    /// Find a node by id.
    ///
    /// Iterative rather than recursive: this runs on every property access
    /// from script, and the kernel stack is far smaller than a user process's.
    pub fn by_id(&self, id: NodeId) -> Option<&Node> {
        let mut stack: Vec<&Node> = alloc::vec![self];
        while let Some(node) = stack.pop() {
            if node.id == id {
                return Some(node);
            }
            stack.extend(node.children.iter());
        }
        None
    }

    pub fn by_id_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let mut stack: Vec<&mut Node> = alloc::vec![self];
        while let Some(node) = stack.pop() {
            if node.id == id {
                return Some(node);
            }
            stack.extend(node.children.iter_mut());
        }
        None
    }

    /// The id of this node's parent, searching from `self` as the root.
    pub fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        let mut stack: Vec<&Node> = alloc::vec![self];
        while let Some(node) = stack.pop() {
            if node.children.iter().any(|c| c.id == id) {
                return Some(node.id);
            }
            stack.extend(node.children.iter());
        }
        None
    }

    /// Every element in the tree, in document order.
    pub fn descendants(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        let mut stack: Vec<&Node> = alloc::vec![self];
        while let Some(node) = stack.pop() {
            out.push(node);
            // Pushed in reverse so the pop order is document order.
            stack.extend(node.children.iter().rev());
        }
        out
    }

    pub fn as_element(&self) -> Option<&ElementData> {
        match &self.kind {
            NodeKind::Element(e) => Some(e),
            NodeKind::Text(_) => None,
        }
    }

    pub fn tag(&self) -> &str {
        match &self.kind {
            NodeKind::Element(e) => &e.tag,
            NodeKind::Text(_) => "",
        }
    }

    /// Concatenated text of this node and everything under it.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out, 0);
        out
    }

    fn collect_text(&self, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match &self.kind {
            NodeKind::Text(t) => out.push_str(t),
            NodeKind::Element(_) => {
                for child in &self.children {
                    child.collect_text(out, depth + 1);
                }
            }
        }
    }

    /// Depth-first search for the first element with this tag name.
    pub fn find_tag(&self, tag: &str) -> Option<&Node> {
        self.find_tag_inner(tag, 0)
    }

    fn find_tag_inner(&self, tag: &str, depth: usize) -> Option<&Node> {
        if depth > MAX_DEPTH {
            return None;
        }
        if self.tag().eq_ignore_ascii_case(tag) {
            return Some(self);
        }
        self.children
            .iter()
            .find_map(|c| c.find_tag_inner(tag, depth + 1))
    }

    pub fn as_element_mut(&mut self) -> Option<&mut ElementData> {
        match &mut self.kind {
            NodeKind::Element(e) => Some(e),
            NodeKind::Text(_) => None,
        }
    }

    /// Replace all children with a single text node, as `textContent =` does.
    pub fn set_text_content(&mut self, text: String) {
        match &mut self.kind {
            NodeKind::Text(slot) => *slot = text,
            NodeKind::Element(_) => {
                self.children.clear();
                self.children.push(Node::text(text));
            }
        }
    }

    /// The scripts in the tree, in document order.
    ///
    /// A `<script src=...>` is reported with its address and no source: only
    /// the caller knows what this document's address is, and therefore what a
    /// relative `src` resolves against, and only the caller can decide whether
    /// a network round trip is worth making.
    pub fn collect_scripts(&self) -> Vec<ScriptRef> {
        let mut out = Vec::new();
        for node in self.descendants() {
            if !node.tag().eq_ignore_ascii_case("script") {
                continue;
            }
            let Some(element) = node.as_element() else { continue };
            // A type attribute naming anything other than JavaScript means
            // the contents are data, not code.
            match element.attr("type") {
                None => {}
                Some(t) if is_javascript_type(t) => {}
                Some(_) => continue,
            }

            let src = element
                .attr("src")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from);
            let source = if src.is_some() { String::new() } else { node.text_content() };
            if src.is_none() && source.trim().is_empty() {
                continue;
            }
            out.push(ScriptRef {
                node: node.id,
                src,
                source,
                defer: element.attr("defer").is_some(),
                is_async: element.attr("async").is_some(),
            });
            if out.len() >= MAX_SCRIPTS {
                break;
            }
        }
        out
    }

    /// Collect the text of every `<style>` element in the tree.
    pub fn collect_styles(&self, out: &mut String) {
        self.collect_styles_inner(out, 0);
    }

    fn collect_styles_inner(&self, out: &mut String, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        if self.tag().eq_ignore_ascii_case("style") {
            out.push_str(&self.text_content());
            out.push('\n');
            return;
        }
        for child in &self.children {
            child.collect_styles_inner(out, depth + 1);
        }
    }
}

fn is_javascript_type(t: &str) -> bool {
    let t = t.trim();
    t.eq_ignore_ascii_case("text/javascript")
        || t.eq_ignore_ascii_case("application/javascript")
        || t.eq_ignore_ascii_case("module")
        || t.is_empty()
}
