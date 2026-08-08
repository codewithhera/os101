//! A small web rendering engine.
//!
//! The pipeline is the conventional one, cut down to what a kernel with one
//! monospace font and no GPU can honour:
//!
//! ```text
//! HTML ──parse──▶ DOM ──┐
//!                       ├──style──▶ style tree ──layout──▶ box tree ──paint──▶ display list
//! CSS  ──parse──▶ rules ┘
//! ```
//!
//! Everything is recomputed on navigation and on resize; scrolling only
//! re-walks the display list, which is why it stays smooth.

pub mod css;
pub mod dom;
pub mod entities;
pub mod forms;
pub mod htmlparse;
pub mod images;
pub mod layout;
pub mod paint;
pub mod script;
pub mod search;
pub mod selftest;
pub mod style;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::color::Color;

pub use css::Viewport;
pub use dom::{Node, NodeId};
pub use forms::Forms;
pub use images::ImageStore;
pub use layout::Metrics;
pub use paint::{DisplayCommand, DisplayList, FieldBox, HitRegion};

/// Refuse documents larger than this. A page that big would spend longer in
/// layout than the user is willing to wait, and the heap is not large.
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// The user-agent stylesheet: the default look of HTML.
///
/// Vertical rhythm is expressed in whole line heights (16px) so that block
/// spacing lines up with text rows.
const UA_CSS: &str = "
html, body, div, p, section, article, header, footer, main, nav, aside,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dt, dd, pre, blockquote, form,
thead, tbody, tfoot, figure, figcaption, hr, address,
details, summary, fieldset, legend, center, dir, menu, output {
    display: block;
}

head, script, style, title, meta, link, base, noscript, template,
iframe, object, embed, param, source, track, svg, canvas, audio, video,
select, option, optgroup {
    display: none;
}

/* A field is replaced content, not a run of text: layout measures its box from
   the `size` attribute and the window layer draws whatever is in it. Hiding
   them, which this sheet used to do, left every form on the web unusable —
   including the search box on most of the pages anyone would want to reach. */
input, textarea {
    display: inline;
}

li { display: list-item; }

/* A table lays its rows out itself: `display: table` is what tells the layout
   code to measure the columns so they line up down the page. */
table { display: table; }
tr { display: table-row; }
td, th { display: table-cell; }

/* Buttons stay visible: they are the thing scripts most often attach a
   handler to, and a page whose buttons are invisible cannot be used. */
button {
    display: inline;
    background-color: #E2E8F0;
    color: #0F172A;
    font-weight: bold;
}

body {
    color: #1E293B;
    margin: 0;
    padding: 8px;
}

p, ul, ol, dl, blockquote, pre, table, figure, form, address {
    margin-top: 16px;
    margin-bottom: 16px;
}

h1, h2, h3, h4, h5, h6 {
    font-weight: bold;
    margin-top: 16px;
    margin-bottom: 16px;
}

/* Only four faces exist, so these round to 32, 24, 20 and 16 pixels. */
h1 { color: #0F172A; font-size: 32px; }
h2 { color: #0F172A; font-size: 24px; }
h3 { color: #1E293B; font-size: 20px; }
h4, h5, h6 { color: #1E293B; font-size: 16px; }

center, caption { text-align: center; }
th { text-align: center; }
mark { background-color: #FEF08A; color: #0F172A; }
s, del, strike { text-decoration: line-through; }
i, em, cite, var, dfn { font-style: italic; }

ul, ol, dd { padding-left: 24px; }
blockquote {
    padding-left: 16px;
    border-left-width: 3px;
    border-color: #94A3B8;
    color: #475569;
}

pre {
    white-space: pre;
    background-color: #F1F5F9;
    padding: 8px;
    color: #0F172A;
}
code, kbd, samp { color: #B91C1C; }

hr {
    margin-top: 16px;
    margin-bottom: 16px;
    border-top-width: 1px;
    border-color: #CBD5E1;
}

b, strong, th { font-weight: bold; }
u, ins { text-decoration: underline; }
a { color: #1D4ED8; text-decoration: underline; }

td, th { padding: 2px; }
table { border-color: #E2E8F0; }
";

/// A loaded document.
///
/// The page keeps its DOM rather than discarding it after the first render.
/// Style, layout and paint are cheap enough to redo and are all derived state,
/// so a script that changes the tree just calls [`Page::relayout`] — which is
/// exactly how a real browser handles a DOM mutation, if rather less
/// incrementally.
pub struct Page {
    pub dom: Node,
    pub title: String,
    pub display: DisplayList,
    /// URLs referenced by [`paint::HitRegion::target`].
    pub link_targets: Vec<String>,
    /// Page background, from `html` or `body`, if the document asked for one.
    pub background: Option<Color>,
    /// The document's `<script>` elements, in document order. Running them is
    /// [`script::Session`]'s job, not the pipeline's.
    pub scripts: Vec<dom::ScriptRef>,
    /// Pictures for this document. The window layer fills this in after the
    /// HTML arrives and calls [`Page::relayout`], which is why a picture can
    /// appear a moment after the text around it.
    pub images: ImageStore,
    /// The form controls in this document, and what has been typed into them.
    /// Rebuilt from the DOM on every relayout, keyed by node so that nothing a
    /// user typed is lost when one happens.
    pub forms: Forms,
    viewport: Viewport,
    metrics: Metrics,
    /// Where ids for script-created nodes carry on from.
    next_id: NodeId,
    /// How many times this page has been laid out.
    ///
    /// Only the self-tests read it, and what they read it for is the one thing
    /// about the binding that no output can show: that a script appending a
    /// hundred nodes in a loop causes one layout rather than a hundred.
    layouts: usize,
}

impl Page {
    pub fn height(&self) -> f32 {
        self.display.height
    }

    /// What the page was laid out against, which is what `window.innerWidth`
    /// reports to a script.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// Would a reader see anything on this page?
    ///
    /// A document that fetched cleanly and then paints nothing but background
    /// is the signature of a page whose content is assembled by JavaScript we
    /// cannot run — which is most of what a modern site ships. Left alone it
    /// looks exactly like a broken browser, so the window layer uses this to
    /// say what happened instead of showing a blank rectangle.
    pub fn has_visible_content(&self) -> bool {
        self.display.commands.iter().any(|command| match command {
            DisplayCommand::Text { text, .. } => !text.trim().is_empty(),
            DisplayCommand::Image { .. } => true,
            // A page that is nothing but a search box is still a usable page.
            DisplayCommand::Field(_) => true,
            // A rectangle alone is not content: a page's background and its
            // empty layout boxes are both drawn as rectangles.
            DisplayCommand::SolidRect { .. } => false,
        })
    }

    /// How many times [`Page::relayout`] has run.
    pub fn layouts(&self) -> usize {
        self.layouts
    }

    /// Redo style, layout and paint from the current DOM.
    pub fn relayout(&mut self) {
        self.layouts += 1;
        self.forms.rebuild(&self.dom);

        let mut author_css = String::new();
        self.dom.collect_styles(&mut author_css);

        let ua = css::parse(UA_CSS, self.viewport);
        let author = css::parse(&author_css, self.viewport);
        let styled = style::build(&self.dom, &ua, &author, self.viewport);

        self.background = find_background(&styled);
        self.title = self
            .dom
            .find_tag("title")
            .map(|n| collapse(&n.text_content()))
            .filter(|t| !t.is_empty())
            .unwrap_or_default();

        let (box_tree, link_targets) =
            layout::layout_document(&styled, self.viewport.width, self.metrics, &self.images);
        self.display = paint::build(&box_tree);
        self.link_targets = link_targets;
    }

    /// Give ids to any nodes a script has just created.
    pub fn refresh_ids(&mut self) {
        let mut next = self.next_id;
        self.dom.assign_ids(&mut next);
        self.next_id = next;
    }

    /// Number a subtree that is not in the document yet, so a script can hold
    /// a reference to it before it is attached.
    pub fn adopt_ids(&mut self, node: &mut Node) {
        let mut next = self.next_id;
        node.assign_ids(&mut next);
        self.next_id = next;
    }

    /// The topmost region at a page-coordinate point.
    ///
    /// Later regions win, since they are painted on top.
    pub fn hit(&self, x: f32, y: f32) -> Option<&HitRegion> {
        self.display.hits.iter().rev().find(|h| {
            x >= h.rect.x
                && x < h.rect.x + h.rect.width
                && y >= h.rect.y
                && y < h.rect.y + h.rect.height
        })
    }

    /// Every `<img src>` in the document, in document order and without
    /// repeats.
    ///
    /// The values come back exactly as the page wrote them: resolving one
    /// against an address is the caller's job, since only the caller knows
    /// which address this document arrived from. A repeated src — a bullet or a
    /// spacer used down the page — is listed once, because it only has to be
    /// fetched once.
    pub fn image_sources(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for node in self.dom.descendants() {
            if !node.tag().eq_ignore_ascii_case("img") {
                continue;
            }
            let Some(src) = node.as_element().and_then(|e| e.attr("src")) else { continue };
            let src = src.trim();
            if src.is_empty() || out.iter().any(|seen| seen == src) {
                continue;
            }
            if out.len() >= images::MAX_PICTURES {
                break;
            }
            out.push(src.to_string());
        }
        out
    }

    /// The `src` of the picture at a page-coordinate point.
    ///
    /// Only pictures that arrived are reported. This feeds a context menu that
    /// saves a picture or makes it the wallpaper, and neither means anything
    /// for a frame with alt text in it.
    pub fn image_at(&self, x: f32, y: f32) -> Option<&str> {
        // Later commands are painted over earlier ones, so the last match wins.
        self.display.commands.iter().rev().find_map(|command| match command {
            DisplayCommand::Image { rect, src }
                if x >= rect.x
                    && x < rect.x + rect.width
                    && y >= rect.y
                    && y < rect.y + rect.height =>
            {
                Some(src.as_str())
            }
            _ => None,
        })
    }

    /// The form control at a page-coordinate point.
    ///
    /// Read from the display list rather than from the control table, because
    /// only the display list knows where a control ended up on the page — and
    /// the box is what a click has to be measured against to place the caret.
    pub fn field_at(&self, x: f32, y: f32) -> Option<&FieldBox> {
        self.display.commands.iter().rev().find_map(|command| match command {
            DisplayCommand::Field(field)
                if x >= field.rect.x
                    && x < field.rect.x + field.rect.width
                    && y >= field.rect.y
                    && y < field.rect.y + field.rect.height =>
            {
                Some(field)
            }
            _ => None,
        })
    }

    /// The URL of the link at a page-coordinate point, if any.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        self.display
            .hits
            .iter()
            .rev()
            .filter(|h| {
                x >= h.rect.x
                    && x < h.rect.x + h.rect.width
                    && y >= h.rect.y
                    && y < h.rect.y + h.rect.height
            })
            .find_map(|h| h.target)
            .and_then(|t| self.link_targets.get(t))
            .map(|s| s.as_str())
    }
}

/// Run the full pipeline over a document.
///
/// `viewport` sizes the page and resolves `vw`/`vh` units; `metrics` describes
/// the font it will be drawn with. Scripts are collected but not run — the
/// caller decides whether to execute them.
pub fn render(html: &str, viewport: Viewport, metrics: Metrics) -> Page {
    let source = if html.len() > MAX_DOCUMENT_BYTES {
        &html[..floor_char_boundary(html, MAX_DOCUMENT_BYTES)]
    } else {
        html
    };

    let mut dom = htmlparse::parse(source);
    let mut next_id = 0;
    dom.assign_ids(&mut next_id);
    let scripts = dom.collect_scripts();

    let mut page = Page {
        dom,
        title: String::new(),
        display: DisplayList::default(),
        link_targets: Vec::new(),
        background: None,
        scripts,
        // Empty: the window layer has not had a chance to fetch anything yet,
        // so this first layout is the one that draws placeholders.
        images: ImageStore::new(),
        forms: Forms::default(),
        viewport,
        metrics,
        next_id,
        layouts: 0,
    };
    page.relayout();
    page
}

/// The `background-color` of `html` or `body`, which paints the whole viewport
/// rather than just that element's box.
fn find_background(root: &style::StyledNode) -> Option<Color> {
    let mut found = None;
    let mut stack = alloc::vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth >= dom::MAX_DEPTH {
            continue;
        }
        let tag = node.node.tag();
        if tag.eq_ignore_ascii_case("html") || tag.eq_ignore_ascii_case("body") {
            // Body wins over html when both declare one.
            if let Some(color) = node.color("background-color") {
                found = Some(color);
                if tag.eq_ignore_ascii_case("body") {
                    return found;
                }
            }
        }
        stack.extend(node.children.iter().map(|c| (c, depth + 1)));
    }
    found
}

/// Collapse runs of whitespace into single spaces, the way HTML does.
pub(crate) fn collapse(s: &str) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Largest index no greater than `at` that lands on a char boundary.
fn floor_char_boundary(s: &str, at: usize) -> usize {
    let mut i = at.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A minimal error page, so failures look like pages rather than blank space.
pub fn error_document(title: &str, detail: &str) -> String {
    alloc::format!(
        "<html><head><title>{}</title></head><body>\
         <h1>{}</h1><p>{}</p></body></html>",
        escape(title),
        escape(title),
        escape(detail)
    )
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

/// Resolve `href` against the page it appeared on.
///
/// Handles absolute URLs, protocol-relative URLs, root-relative paths, and
/// plain relative paths. Anything unrecognised is returned unchanged so the
/// HTTP layer can produce the error message.
pub fn resolve_url(base: &str, href: &str) -> String {
    let href = href.trim();

    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Some(rest) = href.strip_prefix("//") {
        return alloc::format!("http://{}", rest);
    }

    // Split the base into scheme://host and path.
    let after_scheme = base.find("://").map(|i| i + 3).unwrap_or(0);
    let (origin, path) = match base[after_scheme..].find('/') {
        Some(i) => base.split_at(after_scheme + i),
        None => (base, "/"),
    };
    let path = if path.is_empty() { "/" } else { path };

    if href.starts_with('/') {
        return alloc::format!("{}{}", origin, href);
    }
    if href.is_empty() {
        return base.to_string();
    }

    // Relative to the current directory.
    let dir = match path.rfind('/') {
        Some(i) => &path[..=i],
        None => "/",
    };
    normalise(&alloc::format!("{}{}{}", origin, dir, href))
}

/// Collapse `.` and `..` segments in a URL path.
fn normalise(url: &str) -> String {
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    let Some(slash) = url[after_scheme..].find('/') else {
        return url.to_string();
    };
    let (origin, path) = url.split_at(after_scheme + slash);

    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }

    let mut out = String::from(origin);
    for part in &parts {
        out.push('/');
        out.push_str(part);
    }
    if out.len() == origin.len() || path.ends_with('/') {
        out.push('/');
    }
    out
}
