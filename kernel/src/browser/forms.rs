//! Form controls: the state behind a field on a page, and where a press goes.
//!
//! A control's value cannot live in the box tree. That tree is thrown away and
//! built again whenever a picture arrives or a script touches the document, and
//! a field somebody is halfway through typing into has to survive both. So the
//! values live here, in a table keyed by [`NodeId`], and layout only ever asks
//! how large a control's box should be — everything the user put in it is
//! looked up at drawing time, exactly as a picture's pixels are.
//!
//! Only `method="get"` is honoured. The kernel's HTTP client issues GET
//! requests and nothing else, so a form asking for POST is submitted as a query
//! string too. That is wrong, and it is still much better than a search box
//! that does nothing at all when you press Enter.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::dom::{ElementData, Node, NodeId, MAX_DEPTH, NO_NODE};
use super::search::encode;

/// Most controls one page may have. A form generator can emit thousands of
/// hidden inputs, and every one of them would be walked on each keystroke.
pub const MAX_CONTROLS: usize = 256;

/// Longest value a field will hold. Long enough for any query somebody types
/// and short enough that a script filling fields in a loop cannot eat the heap.
pub const MAX_VALUE_CHARS: usize = 512;

/// Room left inside a control's box so its text does not touch the edge.
/// Shared by layout, painting and drawing, which all have to agree on it or
/// the caret lands beside the character it is supposed to be next to.
pub const INSET: f32 = 3.0;

/// A text field with no `size` attribute and no width in the cascade.
const DEFAULT_COLUMNS: usize = 20;
/// A `<textarea>` with no `rows`, which is what HTML itself defaults to.
const DEFAULT_ROWS: usize = 2;
/// Ceilings on what a control may ask for, since the page wrote them.
const MAX_COLUMNS: usize = 200;
const MAX_ROWS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A one-line field: `text`, and the newer types that behave as one.
    Text,
    /// The same field with its contents disguised.
    Password,
    /// A `<textarea>`, where Enter puts a line break in rather than submitting.
    Area,
    /// Submits its form when pressed.
    Submit,
    /// A button that submits nothing, because its page meant to drive it from
    /// script.
    Push,
    /// Contributes a value on submission and occupies no space at all.
    Hidden,
    /// A control this engine has no interface for: a checkbox, a radio, a file
    /// picker. It keeps a box so that the layout around it keeps its shape —
    /// vanishing would silently collapse the row of a settings page.
    Unsupported,
}

impl Kind {
    /// Can the caret go in it?
    pub fn editable(self) -> bool {
        matches!(self, Kind::Text | Kind::Password | Kind::Area)
    }

    pub fn submits(self) -> bool {
        matches!(self, Kind::Submit)
    }

    /// Drawn with its contents hidden.
    pub fn masked(self) -> bool {
        matches!(self, Kind::Password)
    }
}

/// One control, with whatever the user has done to it.
pub struct Control {
    pub node: NodeId,
    /// The `<form>` this control is inside, or [`NO_NODE`] when it is loose in
    /// the document and therefore has nothing to submit to.
    pub form: NodeId,
    pub kind: Kind,
    pub name: String,
    pub value: String,
    /// Where the caret sits, counted in characters rather than bytes so that a
    /// value with anything non-ASCII in it cannot be split down the middle of a
    /// character.
    pub caret: usize,
    /// True for a control the page disabled, and for one whose type this browser
    /// has no interface for: neither takes the caret, and neither contributes
    /// anything when the form is sent.
    pub disabled: bool,
}

impl Control {
    /// Put `text` in the field and leave the caret after it, as assigning to
    /// `input.value` does.
    pub fn set_value(&mut self, text: &str) {
        self.value = text.chars().take(MAX_VALUE_CHARS).collect();
        self.caret = self.value.chars().count();
    }

    pub fn insert(&mut self, c: char) {
        // A line break only means anything in a textarea; everywhere else the
        // key that produces one is the key that submits.
        if c == '\n' && self.kind != Kind::Area {
            return;
        }
        if c != '\n' && (c < ' ' || c == '\x7f') {
            return;
        }
        if self.value.chars().count() >= MAX_VALUE_CHARS {
            return;
        }
        let at = self.byte_of(self.caret);
        self.value.insert(at, c);
        self.caret += 1;
    }

    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        let from = self.byte_of(self.caret - 1);
        let to = self.byte_of(self.caret);
        self.value.replace_range(from..to, "");
        self.caret -= 1;
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.length());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.length();
    }

    /// Move the caret to a character offset, clamped to the value.
    pub fn place_caret(&mut self, at: usize) {
        self.caret = at.min(self.length());
    }

    pub fn length(&self) -> usize {
        self.value.chars().count()
    }

    fn byte_of(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }
}

/// Every control on a page, and which one has the caret.
#[derive(Default)]
pub struct Forms {
    controls: Vec<Control>,
    focus: Option<NodeId>,
}

impl Forms {
    /// Rediscover the controls in `dom`, keeping what the user has typed.
    ///
    /// Called from every relayout, which is why the carry-over matters: a
    /// picture arriving lower down the page must not empty the field somebody
    /// is typing a search into. A control keeps its value as long as its node
    /// is still a control of the same kind; anything else — a script replacing
    /// the form, an id reused for something different — starts fresh.
    pub fn rebuild(&mut self, dom: &Node) {
        let previous = core::mem::take(&mut self.controls);
        collect(dom, NO_NODE, 0, &mut self.controls);

        for control in self.controls.iter_mut() {
            let carried = previous
                .iter()
                .find(|old| old.node == control.node && old.kind == control.kind);
            if let Some(old) = carried {
                control.value.clone_from(&old.value);
                control.caret = old.caret.min(control.length());
            }
        }

        if let Some(focus) = self.focus {
            if self.editable(focus).is_none() {
                self.focus = None;
            }
        }
    }

    pub fn get(&self, node: NodeId) -> Option<&Control> {
        self.controls.iter().find(|c| c.node == node)
    }

    pub fn get_mut(&mut self, node: NodeId) -> Option<&mut Control> {
        self.controls.iter_mut().find(|c| c.node == node)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Control> {
        self.controls.iter()
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focus
    }

    pub fn focused_control_mut(&mut self) -> Option<&mut Control> {
        let node = self.focus?;
        self.get_mut(node)
    }

    /// Put the caret in a field at a character offset. False when the node is
    /// not something the caret can go in.
    pub fn focus_at(&mut self, node: NodeId, at: usize) -> bool {
        if self.editable(node).is_none() {
            return false;
        }
        self.focus = Some(node);
        if let Some(control) = self.get_mut(node) {
            control.place_caret(at);
        }
        true
    }

    pub fn blur(&mut self) {
        self.focus = None;
    }

    fn editable(&self, node: NodeId) -> Option<&Control> {
        self.get(node).filter(|c| c.kind.editable() && !c.disabled)
    }

    /// The query string a submission from this form carries.
    ///
    /// The successful controls, in document order — HTML's own term for the
    /// ones that contribute. A control with no name is left out because
    /// whatever answers the form has no way to tell what it was; a disabled one
    /// is left out because that is what disabled means; and a button
    /// contributes only when it is the one that was pressed.
    pub fn query(&self, form: NodeId, pressed: Option<NodeId>) -> String {
        let mut out = String::new();
        for control in self.controls.iter().filter(|c| c.form == form) {
            if control.name.is_empty() || control.disabled {
                continue;
            }
            match control.kind {
                Kind::Text | Kind::Password | Kind::Area | Kind::Hidden => {}
                Kind::Submit | Kind::Push => {
                    if Some(control.node) != pressed || control.value.is_empty() {
                        continue;
                    }
                }
                Kind::Unsupported => continue,
            }
            if !out.is_empty() {
                out.push('&');
            }
            out.push_str(&encode(&control.name));
            out.push('=');
            out.push_str(&encode(&control.value));
        }
        out
    }

    /// Where a submission started from `control` should navigate to.
    ///
    /// `control` is either the button that was pressed or the field Enter was
    /// struck in; a pressed button is the only one that contributes its own
    /// name and value. Nothing comes back for a control outside any form, which
    /// is what a real browser does with one too.
    pub fn submission(&self, dom: &Node, control: NodeId, page_url: &str) -> Option<String> {
        let control = self.get(control)?;
        if control.form == NO_NODE || control.disabled {
            return None;
        }
        // Only a submit button, or a field the caret is in, can start one:
        // pressing a checkbox this browser cannot tick, or a button whose page
        // meant to drive it from script, must leave the page where it is.
        if !control.kind.submits() && !control.kind.editable() {
            return None;
        }
        let form = dom.by_id(control.form)?.as_element()?;
        let pressed = if control.kind.submits() { Some(control.node) } else { None };
        Some(action_url(
            page_url,
            form.attr("action").unwrap_or(""),
            &self.query(control.form, pressed),
        ))
    }
}

/// The address a form's `action` names, with the submission's query on it.
pub fn action_url(page_url: &str, action: &str, query: &str) -> String {
    let action = action.trim();
    let resolved = if action.is_empty() {
        page_url.to_string()
    } else {
        super::resolve_url(page_url, action)
    };

    // A GET submission replaces whatever query the action already carried
    // rather than adding to it, and a fragment is never part of a request.
    let base = resolved.split('#').next().unwrap_or("");
    let base = base.split('?').next().unwrap_or(base);
    if query.is_empty() {
        base.to_string()
    } else {
        alloc::format!("{}?{}", base, query)
    }
}

/// Is this a tag whose contents are its value rather than page text?
pub fn is_control(tag: &str) -> bool {
    tag.eq_ignore_ascii_case("input")
        || tag.eq_ignore_ascii_case("textarea")
        || tag.eq_ignore_ascii_case("button")
}

// ── The box a control asks for ──────────────────────────────────────────────

/// What layout needs to give a control a box.
pub struct Shape {
    pub kind: Kind,
    /// Width in characters, from `size` or `cols` or the width of a label.
    pub columns: usize,
    pub rows: usize,
    /// Drawn inside the box, for a button. Empty for a field, whose contents
    /// are not known to layout at all.
    pub label: String,
}

/// The box `node` asks for, or nothing when it is not a control or takes up no
/// room.
pub fn shape_of(node: &Node) -> Option<Shape> {
    let element = node.as_element()?;
    let kind = kind_of(element)?;
    if kind == Kind::Hidden {
        return None;
    }

    let label = label_of(node, element, kind);
    let columns = match kind {
        Kind::Area => count_attr(element.attr("cols")).unwrap_or(DEFAULT_COLUMNS),
        // A button is as wide as what is written on it, with a character of
        // room at each end so two of them side by side do not touch.
        Kind::Submit | Kind::Push => label.chars().count() + 2,
        // Nothing is drawn in it, so it only has to read as a control.
        Kind::Unsupported => 2,
        _ => count_attr(element.attr("size")).unwrap_or(DEFAULT_COLUMNS),
    };
    let rows = match kind {
        Kind::Area => count_attr(element.attr("rows")).unwrap_or(DEFAULT_ROWS),
        _ => 1,
    };

    // A control the page has disabled is drawn exactly like one whose type this
    // browser cannot offer, because from where the user sits they are the same
    // thing: a box that will not take the caret. Its size still comes from the
    // type it really is, so the layout around it does not move.
    let drawn = if element.attr("disabled").is_some() { Kind::Unsupported } else { kind };

    Some(Shape {
        kind: drawn,
        columns: columns.clamp(1, MAX_COLUMNS),
        rows: rows.clamp(1, MAX_ROWS),
        label,
    })
}

// ── Drawing a field ─────────────────────────────────────────────────────────

/// What a field shows, and where the caret is in it.
pub struct View {
    pub rows: Vec<String>,
    pub caret_row: usize,
    pub caret_col: usize,
}

/// Fit a value into a box `room` characters across and `rows` deep.
///
/// A one-line field scrolls sideways to keep the caret in view, which is the
/// same compromise the address bar makes; a textarea wraps and scrolls down.
/// Either way the caret is always on a row that is drawn, because a caret you
/// cannot see is worse than no caret at all.
pub fn view(value: &str, caret: usize, room: usize, rows: usize, mask: bool) -> View {
    let room = room.max(1);
    let rows = rows.max(1);
    let chars: Vec<char> = if mask {
        value.chars().map(|_| '*').collect()
    } else {
        value.chars().collect()
    };
    let caret = caret.min(chars.len());

    if rows == 1 {
        let start = caret.saturating_sub(room);
        let end = (start + room).min(chars.len());
        return View {
            rows: alloc::vec![chars[start..end].iter().collect()],
            caret_row: 0,
            caret_col: caret - start,
        };
    }

    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut column = 0usize;
    let mut caret_row = 0usize;
    let mut caret_col = 0usize;

    for (i, c) in chars.iter().copied().enumerate() {
        if i == caret {
            caret_row = lines.len();
            caret_col = column;
        }
        if c == '\n' {
            lines.push(core::mem::take(&mut line));
            column = 0;
            continue;
        }
        line.push(c);
        column += 1;
        if column == room {
            lines.push(core::mem::take(&mut line));
            column = 0;
        }
    }
    if caret == chars.len() {
        caret_row = lines.len();
        caret_col = column;
    }
    lines.push(line);

    let first = (caret_row + 1).saturating_sub(rows);
    View {
        rows: lines.into_iter().skip(first).take(rows).collect(),
        caret_row: caret_row - first,
        caret_col,
    }
}

// ── Discovery ───────────────────────────────────────────────────────────────

fn collect(node: &Node, form: NodeId, depth: usize, out: &mut Vec<Control>) {
    if depth >= MAX_DEPTH || out.len() >= MAX_CONTROLS {
        return;
    }

    let mut form = form;
    if let Some(element) = node.as_element() {
        if element.tag.eq_ignore_ascii_case("form") {
            form = node.id;
        } else if let Some(control) = control_of(node, element, form) {
            out.push(control);
        }
    }

    for child in &node.children {
        collect(child, form, depth + 1, out);
    }
}

fn control_of(node: &Node, element: &ElementData, form: NodeId) -> Option<Control> {
    // A node the parser numbered past its depth limit is unreachable from a
    // click, so a control there could never be used.
    if node.id == NO_NODE {
        return None;
    }
    let kind = kind_of(element)?;
    let mut control = Control {
        node: node.id,
        form,
        kind,
        name: element.attr("name").unwrap_or("").to_string(),
        value: String::new(),
        caret: 0,
        disabled: element.attr("disabled").is_some() || kind == Kind::Unsupported,
    };
    control.set_value(&initial_value(node, element, kind));
    Some(control)
}

fn kind_of(element: &ElementData) -> Option<Kind> {
    let declared = element.attr("type").unwrap_or("").trim();
    if element.tag.eq_ignore_ascii_case("textarea") {
        return Some(Kind::Area);
    }
    if element.tag.eq_ignore_ascii_case("button") {
        // A button submits unless it says otherwise, which is why a page with
        // one unlabelled button in a form still works.
        return Some(if declared.is_empty() || declared.eq_ignore_ascii_case("submit") {
            Kind::Submit
        } else {
            Kind::Push
        });
    }
    if !element.tag.eq_ignore_ascii_case("input") {
        return None;
    }
    if declared.is_empty() {
        return Some(Kind::Text);
    }
    // `image` is a submit button drawn as a picture; treated as an ordinary one
    // because the picture would have to be fetched to know how big it is.
    for (name, kind) in [
        ("text", Kind::Text),
        ("search", Kind::Text),
        ("url", Kind::Text),
        ("email", Kind::Text),
        ("tel", Kind::Text),
        ("number", Kind::Text),
        ("password", Kind::Password),
        ("submit", Kind::Submit),
        ("image", Kind::Submit),
        ("button", Kind::Push),
        ("reset", Kind::Push),
        ("hidden", Kind::Hidden),
    ] {
        if declared.eq_ignore_ascii_case(name) {
            return Some(kind);
        }
    }
    Some(Kind::Unsupported)
}

fn initial_value(node: &Node, element: &ElementData, kind: Kind) -> String {
    match kind {
        // A textarea's contents are its value. HTML drops one line break
        // immediately after the open tag, which is how a hand-written textarea
        // avoids starting with a blank line.
        Kind::Area => {
            let text = node.text_content();
            text.strip_prefix('\n').unwrap_or(&text).to_string()
        }
        // A button's value is only what the page wrote, never the label drawn
        // in place of one: a nameless "Submit" is a caption, and sending it as
        // a value would put a word the page never chose into the query.
        _ => element.attr("value").unwrap_or("").to_string(),
    }
}

fn label_of(node: &Node, element: &ElementData, kind: Kind) -> String {
    if !matches!(kind, Kind::Submit | Kind::Push) {
        return String::new();
    }
    if let Some(value) = element.attr("value").map(str::trim).filter(|v| !v.is_empty()) {
        return value.to_string();
    }
    let text = super::collapse(&node.text_content());
    if !text.is_empty() {
        return text;
    }
    // What a browser writes on a submit button that never said what it was for.
    match kind {
        Kind::Submit => "Submit".to_string(),
        _ => "Button".to_string(),
    }
}

/// A count written as a bare number, the way HTML writes `size` and `rows`.
fn count_attr(value: Option<&str>) -> Option<usize> {
    value?.trim().parse::<usize>().ok().filter(|n| *n > 0)
}

// ── Self-test ───────────────────────────────────────────────────────────────

pub fn selftest(r: &mut crate::selftest::Report) {
    let page = super::render(
        "<body><form action=\"/find\">\
           <input type=hidden name=src value=\"os 101\">\
           <input name=q size=30 value=\"cat\">\
           <input name=blank>\
           <input value=\"no name\">\
           <input type=password name=pw>\
           <input type=checkbox name=safe value=on>\
           <input name=off value=1 disabled>\
           <input type=submit name=go value=\"Search\">\
           <input type=submit value=\"Also search\">\
         </form>\
         <input name=loose value=x></body>",
        super::Viewport { width: 480.0, height: 320.0, char_w: 8.0, line_h: 16.0 },
        super::Metrics { char_w: 8.0, line_h: 16.0 },
    );

    let names: Vec<&str> = page.forms.iter().map(|c| c.name.as_str()).collect();
    r.check(
        "controls found in document order",
        names == ["src", "q", "blank", "", "pw", "safe", "off", "go", "", "loose"],
    );

    let text = page.forms.iter().find(|c| c.name == "q");
    r.check("a field keeps its value attribute", text.map(|c| c.value.as_str()) == Some("cat"));
    r.check("a field starts unfocused", page.forms.focused().is_none());
    r.check(
        "a password field is one",
        page.forms.iter().any(|c| c.name == "pw" && c.kind.masked()),
    );
    r.check(
        "an unknown type is not editable",
        page.forms
            .iter()
            .any(|c| c.name == "safe" && !c.kind.editable() && c.disabled),
    );
    r.check(
        "a disabled field is still a text field",
        page.forms
            .iter()
            .any(|c| c.name == "off" && c.kind == Kind::Text && c.disabled),
    );
    r.check(
        "a control outside a form has no form",
        page.forms.iter().any(|c| c.name == "loose" && c.form == NO_NODE),
    );

    // The whole point: the query a press on the Search button sends.
    let form = text.map(|c| c.form).unwrap_or(NO_NODE);
    let button = page.forms.iter().find(|c| c.name == "go").map(|c| c.node);
    r.check(
        "query built from the successful controls",
        page.forms.query(form, button) == "src=os%20101&q=cat&blank=&pw=&go=Search",
    );
    r.check(
        "an unpressed button contributes nothing",
        page.forms.query(form, None) == "src=os%20101&q=cat&blank=&pw=",
    );
    r.check(
        "a loose control is not in the form's query",
        !page.forms.query(form, button).contains("loose"),
    );
    r.check(
        "a disabled control contributes nothing",
        !page.forms.query(form, button).contains("off"),
    );

    // Values go through the same percent encoding an address does, or a query
    // with an ampersand in it would look like two.
    let mut typed = super::render(
        "<body><form action=\"s\"><input name=\"a b\"><input name=q></form></body>",
        super::Viewport { width: 480.0, height: 320.0, char_w: 8.0, line_h: 16.0 },
        super::Metrics { char_w: 8.0, line_h: 16.0 },
    );
    let (first, second) = {
        let mut ids = typed.forms.iter().map(|c| c.node);
        (ids.next().unwrap_or(NO_NODE), ids.next().unwrap_or(NO_NODE))
    };
    let form = typed.forms.get(first).map(|c| c.form).unwrap_or(NO_NODE);
    if let Some(control) = typed.forms.get_mut(first) {
        control.set_value("a&b=c");
    }
    if let Some(control) = typed.forms.get_mut(second) {
        control.set_value("réd panda");
    }
    r.check(
        "names and values are encoded",
        typed.forms.query(form, None) == "a%20b=a%26b%3Dc&q=r%C3%A9d%20panda",
    );

    // Typing must survive the relayout that a picture arriving causes.
    typed.relayout();
    r.check(
        "typed text survives a relayout",
        typed.forms.get(first).map(|c| c.value.as_str()) == Some("a&b=c"),
    );

    // Caret editing, which is all of the keyboard handling that is pure.
    if let Some(control) = typed.forms.get_mut(second) {
        control.set_value("cat");
        r.check("caret follows an assignment", control.caret == 3);
        control.left();
        control.left();
        control.insert('h');
        r.check("insert lands at the caret", control.value == "chat");
        r.check("caret advances", control.caret == 2);
        control.home();
        control.right();
        control.backspace();
        r.check("backspace deletes before the caret", control.value == "hat");
        r.check("caret follows the deletion", control.caret == 0);
        control.backspace();
        r.check("backspace at the start does nothing", control.value == "hat");
        control.end();
        r.check("end goes past the last character", control.caret == 3);
        control.right();
        r.check("the caret stops at the end", control.caret == 3);
        control.insert('\n');
        r.check("a line break is not typed into a one-line field", control.value == "hat");
        control.set_value(&"x".repeat(MAX_VALUE_CHARS + 40));
        control.insert('y');
        r.check("a field's value is bounded", control.length() == MAX_VALUE_CHARS);
    }

    // Where a submission goes, which is the part a wrong answer sends
    // somewhere else entirely.
    let base = "http://example.com/a/b/page.html";
    r.check(
        "an empty action means this page",
        action_url(base, "", "q=cat") == "http://example.com/a/b/page.html?q=cat",
    );
    r.check(
        "a relative action resolves against the page",
        action_url(base, "find", "q=cat") == "http://example.com/a/b/find?q=cat",
    );
    r.check(
        "a root-relative action keeps the host",
        action_url(base, "/find", "q=cat") == "http://example.com/find?q=cat",
    );
    r.check(
        "an absolute action is taken as written",
        action_url(base, "https://duckduckgo.com/lite", "q=cat")
            == "https://duckduckgo.com/lite?q=cat",
    );
    r.check(
        "the action's own query is replaced",
        action_url(base, "/s?old=1#top", "q=cat") == "http://example.com/s?q=cat",
    );
    r.check(
        "an empty query leaves a bare address",
        action_url(base, "/s", "") == "http://example.com/s",
    );

    // And the two together, from the button that was pressed.
    let submitted = page
        .forms
        .iter()
        .find(|c| c.name == "go")
        .and_then(|c| page.forms.submission(&page.dom, c.node, base));
    r.check(
        "a press submits to the action",
        submitted.as_deref()
            == Some("http://example.com/find?src=os%20101&q=cat&blank=&pw=&go=Search"),
    );
    let submitted_from = |name: &str| {
        page.forms
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| page.forms.submission(&page.dom, c.node, base))
    };
    r.check(
        "a field sends its form without contributing a button",
        submitted_from("q").as_deref()
            == Some("http://example.com/find?src=os%20101&q=cat&blank=&pw="),
    );
    r.check("a control in no form submits nowhere", submitted_from("loose").is_none());
    r.check("a control this browser cannot offer submits nothing", submitted_from("safe").is_none());
    r.check("a disabled field submits nothing", submitted_from("off").is_none());

    // Focus only goes where it can be seen.
    let mut focus = super::render(
        "<body><form><input name=a><input type=hidden name=b><input type=submit>\
         <input type=button name=js value=Run></form></body>",
        super::Viewport { width: 480.0, height: 320.0, char_w: 8.0, line_h: 16.0 },
        super::Metrics { char_w: 8.0, line_h: 16.0 },
    );
    let ids: Vec<NodeId> = focus.forms.iter().map(|c| c.node).collect();
    r.check("a field can be focused", focus.forms.focus_at(ids[0], 0));
    r.check("a hidden control cannot", !focus.forms.focus_at(ids[1], 0));
    r.check("a button cannot", !focus.forms.focus_at(ids[2], 0));
    r.check("focus stays where it was put", focus.forms.focused() == Some(ids[0]));
    r.check(
        "a button the page drives itself submits nothing",
        focus.forms.submission(&focus.dom, ids[3], base).is_none(),
    );
    focus.relayout();
    r.check("focus survives a relayout", focus.forms.focused() == Some(ids[0]));
    focus.forms.blur();
    r.check("blur drops it", focus.forms.focused().is_none());

    // What a field shows when its value is longer than its box.
    let long = view("abcdefghij", 10, 4, 1, false);
    r.check(
        "a one-line field scrolls to the caret",
        long.rows == ["ghij"] && long.caret_col == 4,
    );
    let masked = view("secret", 6, 10, 1, true);
    r.check("a password shows nothing of itself", masked.rows == ["******"]);
    let wrapped = view("abcdefgh", 8, 3, 2, false);
    r.check(
        "a textarea wraps and scrolls down",
        wrapped.rows == ["def", "gh"] && wrapped.caret_row == 1 && wrapped.caret_col == 2,
    );
    let empty = view("", 0, 0, 0, false);
    r.check("an empty field in an empty box is drawable", empty.rows.len() == 1);
}
