//! Box layout.
//!
//! Block-level boxes stack vertically and fill their container's width;
//! inline content inside a block is gathered into an anonymous box and wrapped
//! into lines. Floats and grids are not implemented, so a page laid out here
//! is a simplified but recognisable version of what a real browser produces.
//!
//! Three details are worth knowing about:
//!
//! * The inline box keeps its **word list**, not just the wrapped result.
//!   Width is not known when the tree is built, so lines are rewrapped once it
//!   is; rewrapping from the words rather than from the previous fragments is
//!   what lets `<pre>` keep its runs of spaces intact.
//! * Adjacent vertical margins **collapse**, as they do in CSS. Without this
//!   every paragraph gap comes out twice as large as the page intended.
//! * An `<img>` is **replaced content**: it joins the line like a very wide
//!   word, but its size comes from the page's declarations or from the picture
//!   itself rather than from the font. Because the picture usually arrives
//!   after the first layout, the inputs to that size are carried in the word
//!   and only resolved when the line is wrapped — see [`image_size`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::color::Color;
use crate::framebuffer::TextSize;

use super::css::Value;
use super::dom::{ElementData, NodeId, NodeKind, MAX_DEPTH, NO_NODE};
use super::forms::{self, Kind};
use super::images::{dimension_attr, ImageStore, MAX_DIMENSION, PLACEHOLDER_INSET};
use super::style::{Display, StyledNode, TextAlign};

/// Metrics of the body font, used as the default and as the minimum line box.
#[derive(Clone, Copy)]
pub struct Metrics {
    pub char_w: f32,
    pub line_h: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct EdgeSizes {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}

impl Dimensions {
    /// The content box grown by padding and border.
    pub fn border_box(&self) -> Rect {
        Rect {
            x: self.content.x - self.padding.left - self.border.left,
            y: self.content.y - self.padding.top - self.border.top,
            width: self.content.width + self.padding.left + self.padding.right
                + self.border.left + self.border.right,
            height: self.content.height + self.padding.top + self.padding.bottom
                + self.border.top + self.border.bottom,
        }
    }
}

/// How a run of text should be drawn.
#[derive(Clone, PartialEq)]
pub struct TextStyle {
    pub color: Color,
    pub size: TextSize,
    pub bold: bool,
    pub underline: bool,
    pub strike: bool,
    /// Painted behind the text, for `<mark>` and inline `background-color`.
    pub background: Option<Color>,
    /// Index into the page's link target table, if this text is a link.
    pub link: Option<usize>,
    /// The element this text came from, for click dispatch.
    pub node: NodeId,
}

/// A positioned run of text, in coordinates relative to its inline box.
pub struct Fragment {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub text: String,
    pub style: TextStyle,
    /// Set when this fragment is a picture rather than a run of glyphs, in
    /// which case `text` holds the alt text and is only drawn if the picture
    /// itself cannot be.
    pub image: Option<ImagePaint>,
    /// Set when this fragment is a form control, in which case `text` holds a
    /// button's label and a field's contents are looked up when it is drawn.
    pub field: Option<Kind>,
}

/// What a painter needs in order to draw a picture, or the space kept for one.
pub struct ImagePaint {
    /// The `src` attribute, which is how the window layer finds the pixels.
    pub src: String,
    /// True when the pixels are in the page's store. False means the box is a
    /// placeholder, whether because the picture has not arrived yet or because
    /// it never will.
    pub ready: bool,
}

/// One word of inline content, before it is placed on a line.
struct Word {
    text: String,
    style: TextStyle,
    /// Starts a new line, for `<br>` and for each line of preformatted text.
    break_before: bool,
    /// Ends the current line if it has anything on it. Marks the edges of a
    /// block that had to be laid out inline, which needs a line to itself but
    /// not a blank one either side.
    fence: bool,
    /// Preformatted: keep the text exactly as-is and never soft-wrap it.
    literal: bool,
    /// Set for a picture, which occupies a fixed box on the line rather than a
    /// run of characters.
    image: Option<ImageWord>,
    /// Set for a form control, which occupies a box in the same way.
    field: Option<FieldWord>,
}

/// A form control waiting to be given a box.
///
/// Kept unresolved for the same reason a picture's size is: a width from the
/// cascade may be a percentage, and the share it is a percentage of is not
/// known until the line is wrapped.
struct FieldWord {
    kind: Kind,
    /// Width in characters, from the attributes.
    columns: usize,
    rows: usize,
    /// `width` from the cascade, which outranks the attributes.
    css: Option<Value>,
}

/// An `<img>` waiting to be given a box.
///
/// Every source of size is kept rather than being reduced to one on the spot:
/// a percentage is a share of the line's width, and the picture's own size may
/// not have arrived yet, so the decision cannot be made until the wrap.
struct ImageWord {
    src: String,
    /// `width` and `height` from the cascade, which outrank the attributes.
    css: (Option<Value>, Option<Value>),
    /// The `width` and `height` attributes, which HTML writes as bare pixels.
    attrs: (Option<f32>, Option<f32>),
    /// The picture's own size, once it has arrived.
    intrinsic: Option<(f32, f32)>,
    /// Whether the picture itself can be drawn. False both for one that has
    /// not arrived and for one that failed, because the fallback is the same.
    ready: bool,
}

/// A run of inline content belonging to the enclosing block.
pub struct InlineBox {
    /// The source of truth. Lines are derived from this every time the width
    /// changes, never from the previously placed fragments.
    words: Vec<Word>,
    align: TextAlign,
    pub fragments: Vec<Fragment>,
    height: f32,
}

pub enum BoxKind<'a> {
    Block(&'a StyledNode<'a>),
    Inline(InlineBox),
}

pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    pub kind: BoxKind<'a>,
    pub children: Vec<LayoutBox<'a>>,
    /// On a table row, each cell's share of the row width. Empty everywhere
    /// else, and empty on a row until the table has measured its columns.
    columns: Vec<f32>,
}

impl<'a> LayoutBox<'a> {
    fn block(node: &'a StyledNode<'a>) -> Self {
        LayoutBox {
            dimensions: Dimensions::default(),
            kind: BoxKind::Block(node),
            children: Vec::new(),
            columns: Vec::new(),
        }
    }

    fn inline(words: Vec<Word>, align: TextAlign) -> Self {
        LayoutBox {
            dimensions: Dimensions::default(),
            kind: BoxKind::Inline(InlineBox {
                words,
                align,
                fragments: Vec::new(),
                height: 0.0,
            }),
            children: Vec::new(),
            columns: Vec::new(),
        }
    }

    fn styled(&self) -> Option<&'a StyledNode<'a>> {
        match self.kind {
            BoxKind::Block(n) => Some(n),
            BoxKind::Inline(_) => None,
        }
    }
}

/// Lay out a style tree into a box tree of the given width.
///
/// Returns the root box and the table of link targets discovered while walking
/// inline content. `images` supplies the natural sizes of any pictures the page
/// has already been given; on the first pass over a document it is empty, and
/// every `<img>` falls back to a placeholder.
pub fn layout_document<'a>(
    root: &'a StyledNode<'a>,
    viewport_width: f32,
    metrics: Metrics,
    images: &ImageStore,
) -> (LayoutBox<'a>, Vec<String>) {
    let mut links = Vec::new();
    let mut root_box = build_box(root, metrics, images, &mut links, 0);

    let containing = Dimensions {
        content: Rect { x: 0.0, y: 0.0, width: viewport_width, height: 0.0 },
        ..Dimensions::default()
    };
    resolve_edges(&mut root_box, containing.content.width);
    root_box.dimensions.content.x = root_box.dimensions.margin.left
        + root_box.dimensions.border.left
        + root_box.dimensions.padding.left;
    root_box.dimensions.content.y = root_box.dimensions.margin.top
        + root_box.dimensions.border.top
        + root_box.dimensions.padding.top;
    layout_children(&mut root_box, metrics, 0);
    apply_explicit_height(&mut root_box);

    (root_box, links)
}

// ── Tree construction ───────────────────────────────────────────────────────

/// Build a block box for `node`, folding its inline children into anonymous
/// inline boxes.
fn build_box<'a>(
    node: &'a StyledNode<'a>,
    metrics: Metrics,
    images: &ImageStore,
    links: &mut Vec<String>,
    depth: usize,
) -> LayoutBox<'a> {
    let mut root = LayoutBox::block(node);
    if depth >= MAX_DEPTH {
        return root;
    }

    let align = node.text_align();
    let mut pending: Vec<Word> = Vec::new();

    for child in &node.children {
        match child.display() {
            Display::None => continue,
            Display::Block | Display::ListItem | Display::TableRow | Display::TableCell => {
                if !pending.is_empty() {
                    root.children
                        .push(LayoutBox::inline(core::mem::take(&mut pending), align));
                }
                let mut block = build_box(child, metrics, images, links, depth + 1);
                if child.display() == Display::ListItem {
                    prepend_marker(&mut block, child, align);
                }
                // A picture is replaced content: a block-level `<img>` has no
                // children to give its box a height, so the box has to be put
                // inside it by hand or the page shows a gap of nothing.
                if let Some(word) = block_image(child, images) {
                    block
                        .children
                        .push(LayoutBox::inline(alloc::vec![word], child.text_align()));
                }
                // A control the page has made block-level is replaced content
                // too, so whatever was built from its children is thrown away:
                // a textarea's contents are its value, not lines of the page.
                if let Some(word) = block_field(child) {
                    block.children.clear();
                    block
                        .children
                        .push(LayoutBox::inline(alloc::vec![word], child.text_align()));
                }
                root.children.push(block);
            }
            Display::Inline => {
                collect_inline(child, &mut pending, images, links, depth + 1, None, node.owner());
            }
        }
    }

    if !pending.is_empty() {
        root.children.push(LayoutBox::inline(pending, align));
    }

    if node.keyword("display") == Some("table") {
        measure_columns(&mut root);
    }

    root
}

// ── Tables ──────────────────────────────────────────────────────────────────

/// Longest cell text a column is allowed to claim when sharing out the width.
///
/// Without a cap one paragraph-sized cell takes the whole row and squeezes
/// every other column down to nothing.
const MAX_COLUMN_CHARS: f32 = 40.0;

/// Work out how a table's rows should share their width, and tell each row.
///
/// Columns are weighted by their widest cell, so a table of dates and titles
/// gives the titles the room. Every row in the table gets the same weights,
/// which is what makes the columns line up.
fn measure_columns(table: &mut LayoutBox) {
    let mut widths: Vec<f32> = Vec::new();
    collect_column_widths(table, &mut widths, 0);
    if widths.is_empty() {
        return;
    }

    let total: f32 = widths.iter().sum();
    if total <= 0.0 {
        return;
    }
    let shares: Vec<f32> = widths.iter().map(|w| w / total).collect();
    assign_columns(table, &shares, 0);
}

fn collect_column_widths(root: &LayoutBox, widths: &mut Vec<f32>, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    if is_row(root) {
        for (i, cell) in root.children.iter().enumerate() {
            let chars = cell
                .styled()
                .map(|s| cell_chars(s))
                .unwrap_or(1.0)
                .clamp(1.0, MAX_COLUMN_CHARS);
            if i == widths.len() {
                widths.push(chars);
            } else if chars > widths[i] {
                widths[i] = chars;
            }
        }
        return;
    }
    for child in &root.children {
        collect_column_widths(child, widths, depth + 1);
    }
}

fn assign_columns(root: &mut LayoutBox, shares: &[f32], depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }
    if is_row(root) {
        root.columns = shares.to_vec();
        return;
    }
    for child in root.children.iter_mut() {
        assign_columns(child, shares, depth + 1);
    }
}

fn is_row(root: &LayoutBox) -> bool {
    matches!(root.styled().map(|s| s.display()), Some(Display::TableRow))
}

/// How wide a cell would like to be, in characters of its own font.
fn cell_chars(cell: &StyledNode) -> f32 {
    let text = cell.node.text_content();
    let mut chars = 0usize;
    for word in text.split_whitespace() {
        chars += word.chars().count() + 1;
    }
    chars.saturating_sub(1).max(1) as f32
}

/// Put a bullet at the start of a list item's first line.
fn prepend_marker<'a>(item: &mut LayoutBox<'a>, node: &'a StyledNode<'a>, align: TextAlign) {
    // An asterisk rather than a bullet: the bitmap font only carries basic
    // Latin, so U+2022 would draw as a blank.
    let marker = Word {
        text: "*".to_string(),
        style: node.text_style(None, node.owner()),
        break_before: false,
        fence: false,
        literal: false,
        image: None,
        field: None,
    };

    for child in item.children.iter_mut() {
        if let BoxKind::Inline(inline) = &mut child.kind {
            inline.words.insert(0, marker);
            return;
        }
    }

    // A list item whose content is all blocks still gets its bullet.
    item.children.insert(0, LayoutBox::inline(alloc::vec![marker], align));
}

/// Walk an inline subtree, emitting its words.
///
/// `owner` is the element the words should be attributed to for hit-testing;
/// it follows the nearest enclosing element, since a text node is not
/// something a click can land on.
fn collect_inline<'a>(
    node: &'a StyledNode<'a>,
    words: &mut Vec<Word>,
    images: &ImageStore,
    links: &mut Vec<String>,
    depth: usize,
    inherited_link: Option<usize>,
    owner: NodeId,
) {
    if depth >= MAX_DEPTH || node.display() == Display::None {
        return;
    }

    let owner = match node.owner() {
        NO_NODE => owner,
        id => id,
    };

    // An <a href> opens a link scope for everything inside it.
    let mut link = inherited_link;
    if let Some(element) = node.node.as_element() {
        if element.tag.eq_ignore_ascii_case("a") {
            if let Some(href) = element.attr("href") {
                let href = href.trim();
                if !href.is_empty() && links.len() < 4096 {
                    links.push(href.to_string());
                    link = Some(links.len() - 1);
                }
            }
        }
        if element.tag.eq_ignore_ascii_case("br") {
            words.push(Word {
                text: String::new(),
                style: node.text_style(link, owner),
                break_before: true,
                fence: false,
                literal: false,
                image: None,
                field: None,
            });
            return;
        }
        // A picture joins the line as one wide word. It may not be here yet,
        // in which case the word is a placeholder holding its alt text.
        if element.tag.eq_ignore_ascii_case("img") {
            if let Some(word) = image_word(node, element, images, link, owner) {
                words.push(word);
            }
            return;
        }

        // A control joins the line as one wide word, and nothing inside it is
        // page text: a button's children are its label and a textarea's are its
        // value, both of which belong to the control rather than to the line.
        if forms::is_control(&element.tag) {
            if let Some(word) = field_word(node, link, owner) {
                words.push(word);
            }
            return;
        }
    }

    match &node.node.kind {
        NodeKind::Text(text) => {
            let preformatted = matches!(node.keyword("white-space"), Some("pre" | "pre-wrap"));
            push_text(text, node.text_style(link, owner), preformatted, words);
        }
        NodeKind::Element(_) => {
            for child in &node.children {
                match child.display() {
                    Display::None => continue,
                    // A block inside inline content cannot be laid out as a
                    // block here, so its text joins the run on its own lines.
                    // It is fenced by a break on each side: without the
                    // trailing one, whatever follows the block carries on
                    // beside it, which is how an unclosed tag earlier in the
                    // document turns a heading into a run-on line.
                    Display::Block | Display::ListItem
                    | Display::TableRow | Display::TableCell => {
                        let edge = || Word {
                            text: String::new(),
                            style: child.text_style(link, owner),
                            break_before: false,
                            fence: true,
                            literal: false,
                            image: None,
                            field: None,
                        };
                        words.push(edge());
                        if child.display() == Display::ListItem {
                            words.push(Word {
                                text: "*".to_string(),
                                style: child.text_style(link, owner),
                                break_before: false,
                                fence: false,
                                literal: false,
                                image: None,
                                field: None,
                            });
                        }
                        collect_inline(child, words, images, links, depth + 1, link, owner);
                        words.push(edge());
                    }
                    Display::Inline => {
                        collect_inline(child, words, images, links, depth + 1, link, owner)
                    }
                }
            }
        }
    }
}

fn push_text(text: &str, style: TextStyle, preformatted: bool, words: &mut Vec<Word>) {
    if preformatted {
        // Each source line becomes one unbreakable word so its internal runs
        // of spaces survive; tabs become spaces because the font has no glyph
        // for them.
        for (i, line) in text.split('\n').enumerate() {
            words.push(Word {
                text: line.replace('\t', "    "),
                style: style.clone(),
                break_before: i > 0,
                fence: false,
                literal: true,
                image: None,
                field: None,
            });
        }
        return;
    }

    for word in text.split_whitespace() {
        words.push(Word {
            text: word.to_string(),
            style: style.clone(),
            break_before: false,
            fence: false,
            literal: false,
            image: None,
            field: None,
        });
    }
}

// ── Pictures ────────────────────────────────────────────────────────────────

/// The box kept for a picture that has not arrived but has something to say in
/// its place. Sixteen by nine at a size that reads as a picture without pushing
/// the rest of the page off the screen.
const PLACEHOLDER_WIDTH: f32 = 240.0;
const PLACEHOLDER_HEIGHT: f32 = 135.0;

/// The shape a picture is assumed to have when the page names one dimension
/// and nothing else is known. Four by three is the least surprising guess, and
/// it is only ever reached before the bytes arrive.
const FALLBACK_RATIO: f32 = 4.0 / 3.0;

/// The word for an `<img>`, or nothing at all when there is neither a picture
/// nor anything to say in its place.
fn image_word(
    node: &StyledNode,
    element: &ElementData,
    images: &ImageStore,
    link: Option<usize>,
    owner: NodeId,
) -> Option<Word> {
    let src = element.attr("src").unwrap_or("").trim();
    let alt = element
        .attr("alt")
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // A zero-sided picture would divide by nothing when its aspect is needed,
    // and no decoder here produces one, so treat it as not having arrived.
    let intrinsic = images
        .size(src)
        .map(|(w, h)| (w as f32, h as f32))
        .filter(|(w, h)| *w > 0.0 && *h > 0.0);

    let css = (
        node.length_value("width").cloned(),
        node.length_value("height").cloned(),
    );
    let attrs = (
        dimension_attr(element.attr("width").unwrap_or("")),
        dimension_attr(element.attr("height").unwrap_or("")),
    );

    // A picture that is not here, says nothing and asks for no room leaves
    // nothing behind. An empty frame on every page that decorates itself with
    // spacers and rules would be worse than the gap it fills.
    let silent = alt.is_empty()
        && css.0.is_none()
        && css.1.is_none()
        && attrs.0.is_none()
        && attrs.1.is_none();
    if intrinsic.is_none() && silent {
        return None;
    }

    Some(Word {
        text: alt,
        style: node.text_style(link, owner),
        break_before: false,
        fence: false,
        literal: false,
        image: Some(ImageWord {
            src: src.to_string(),
            css,
            attrs,
            intrinsic,
            ready: intrinsic.is_some(),
        }),
        field: None,
    })
}

/// The word for an `<img>` the page has made block-level, which reaches layout
/// as a box of its own rather than as part of a run of inline content.
fn block_image(node: &StyledNode, images: &ImageStore) -> Option<Word> {
    let element = node.node.as_element()?;
    if !element.tag.eq_ignore_ascii_case("img") {
        return None;
    }
    // No link scope: an `<a>` is inline, so a block image inside one is
    // reached through the inline path above and keeps its target there.
    image_word(node, element, images, None, node.owner())
}

/// The box a picture occupies on a line `available` pixels wide.
///
/// The order is the one the spec asks for and the one pages assume: the
/// cascade, then the HTML attributes, then the picture's own size. Anything
/// still missing is derived from the aspect ratio, so a page that gives only a
/// width does not get a square.
fn image_size(image: &ImageWord, available: f32) -> (f32, f32) {
    let width = image.css.0.as_ref().and_then(|v| v.to_px(available)).or(image.attrs.0);
    let height = image.css.1.as_ref().and_then(|v| v.to_px(available)).or(image.attrs.1);
    let ratio = image
        .intrinsic
        .map(|(w, h)| w / h)
        .unwrap_or(FALLBACK_RATIO);

    let (mut w, mut h) = match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w / ratio),
        (None, Some(h)) => (h * ratio, h),
        (None, None) => image
            .intrinsic
            .unwrap_or((PLACEHOLDER_WIDTH, PLACEHOLDER_HEIGHT)),
    };

    // Percentages and ratios are arithmetic on numbers that came off the
    // internet, and a box of a size that is not a number would poison every
    // line it touched.
    if !w.is_finite() || !h.is_finite() {
        return (0.0, 0.0);
    }
    w = w.clamp(0.0, MAX_DIMENSION);
    h = h.clamp(0.0, MAX_DIMENSION);

    // A photograph straight off a camera is wider than any window here, and
    // there is no horizontal scrolling to reach the rest of it. Scaling the
    // height to match keeps the picture in proportion rather than squashing it.
    if w > available && available > 0.0 {
        h *= available / w;
        w = available;
    }

    (w, h)
}

/// As much of the alt text as fits across a placeholder.
///
/// The frame is the size the picture would have been and the painter does not
/// clip, so a long description has to be cut here or it runs across whatever
/// stands beside it.
fn fit_alt(alt: &str, width: f32, char_w: f32) -> String {
    if alt.is_empty() || char_w <= 0.0 {
        return String::new();
    }
    // A negative float casts to zero rather than wrapping, so a frame narrower
    // than its own insets simply holds no text.
    let room = ((width - 2.0 * PLACEHOLDER_INSET) / char_w) as usize;
    alt.chars().take(room).collect()
}

// ── Form controls ───────────────────────────────────────────────────────────

/// The word for a control, or nothing when it takes up no room — a hidden
/// input, which still carries a value on submission but has nothing to draw.
fn field_word(node: &StyledNode, link: Option<usize>, owner: NodeId) -> Option<Word> {
    let shape = forms::shape_of(node.node)?;
    Some(Word {
        text: shape.label,
        style: node.text_style(link, owner),
        break_before: false,
        fence: false,
        literal: false,
        image: None,
        field: Some(FieldWord {
            kind: shape.kind,
            columns: shape.columns,
            rows: shape.rows,
            css: node.length_value("width").cloned(),
        }),
    })
}

/// The word for a control the page has made block-level, which reaches layout
/// as a box of its own rather than as part of a run of inline content.
fn block_field(node: &StyledNode) -> Option<Word> {
    if !forms::is_control(node.node.tag()) {
        return None;
    }
    field_word(node, None, node.owner())
}

/// The box a control occupies on a line `available` pixels wide.
fn field_size(field: &FieldWord, char_w: f32, row_h: f32, available: f32) -> (f32, f32) {
    let width = field
        .css
        .as_ref()
        .and_then(|v| v.to_px(available))
        .unwrap_or(field.columns as f32 * char_w + 2.0 * forms::INSET);
    let height = field.rows as f32 * row_h + 2.0 * forms::INSET;

    // A percentage width is arithmetic on a number that came off the internet,
    // and a box of a size that is not a number would poison the line it sat on.
    if !width.is_finite() || !height.is_finite() {
        return (0.0, 0.0);
    }

    // There is no horizontal scrolling to reach a field wider than the line it
    // sits on, so it is cut down to the room there is — but never below one
    // character, since a box too narrow to hold anything is not a field the
    // user can tell is there, and a page can nest a control inside a column
    // narrower than that.
    let floor = char_w + 2.0 * forms::INSET;
    let ceiling = if available > 0.0 { available.max(floor) } else { MAX_DIMENSION };
    (
        width.clamp(floor, ceiling),
        height.clamp(0.0, MAX_DIMENSION),
    )
}

/// Add a control to the current line, which is already known to have room.
///
/// Its label is cut to the box, as a placeholder's alt text is: the painter
/// does not clip, so a long label would otherwise run over whatever stands
/// beside it.
fn place_field(
    line: &mut Line,
    word: &Word,
    field: &FieldWord,
    width: f32,
    height: f32,
    char_w: f32,
) {
    let space_w = if line.fragments.is_empty() { 0.0 } else { char_w };
    let room = ((width - 2.0 * forms::INSET) / char_w.max(1.0)) as usize;

    line.fragments.push(Fragment {
        x: line.width + space_w,
        y: 0.0,
        width,
        height,
        text: word.text.chars().take(room).collect(),
        style: word.style.clone(),
        image: None,
        field: Some(field.kind),
    });

    line.width += space_w + width;
    if height > line.height {
        line.height = height;
    }
}

// ── Line breaking ───────────────────────────────────────────────────────────

/// A line under construction.
struct Line {
    fragments: Vec<Fragment>,
    width: f32,
    height: f32,
}

impl Line {
    fn new(min_height: f32) -> Self {
        Line { fragments: Vec::new(), width: 0.0, height: min_height }
    }
}

/// Break `words` into fragments no wider than `max_width`.
///
/// Returns the fragments in inline-box coordinates and the total height.
fn wrap(
    words: &[Word],
    max_width: f32,
    metrics: Metrics,
    align: TextAlign,
) -> (Vec<Fragment>, f32) {
    let mut out: Vec<Fragment> = Vec::new();
    let mut line = Line::new(metrics.line_h);
    let mut y = 0.0f32;

    for word in words {
        if word.break_before || (word.fence && !line.fragments.is_empty()) {
            finish_line(&mut out, &mut line, &mut y, max_width, align, metrics);
        }

        // Like a picture, a control is measured before the empty-text check
        // below: an empty field is still a box, and the box is what matters.
        if let Some(field) = &word.field {
            let char_w = word.style.size.char_w() as f32;
            let row_h = word.style.size.row_h() as f32;
            let (width, height) = field_size(field, char_w, row_h, max_width);
            if width > 0.0 && height > 0.0 {
                if !line.fragments.is_empty() && line.width + char_w + width > max_width {
                    finish_line(&mut out, &mut line, &mut y, max_width, align, metrics);
                }
                place_field(&mut line, word, field, width, height, char_w);
            }
            continue;
        }

        // A picture is measured before the empty-text check below: its alt text
        // is allowed to be empty, and the box is what matters.
        if let Some(image) = &word.image {
            let (width, height) = image_size(image, max_width);
            // Nothing to reserve: the page asked for a picture of no size, or
            // for one whose size did not come out as a number.
            if width > 0.0 && height > 0.0 {
                let char_w = word.style.size.char_w() as f32;
                // It wraps like a very wide word rather than being cut in half,
                // because half a picture is worth nothing.
                if !line.fragments.is_empty() && line.width + char_w + width > max_width {
                    finish_line(&mut out, &mut line, &mut y, max_width, align, metrics);
                }
                place_image(&mut line, word, image, width, height, char_w);
            }
            continue;
        }

        if word.text.is_empty() {
            continue;
        }

        let char_w = word.style.size.char_w() as f32;
        let row_h = word.style.size.row_h() as f32;

        if word.literal {
            // Preformatted text is placed as-is and allowed to overflow;
            // rewrapping it would destroy the layout the author drew by hand.
            place(&mut line, &word.text, char_w, row_h, &word.style, false);
            continue;
        }

        for chunk in break_long(&word.text, char_w, max_width) {
            let chunk_w = chunk.chars().count() as f32 * char_w;
            let space_w = if line.fragments.is_empty() { 0.0 } else { char_w };
            if !line.fragments.is_empty() && line.width + space_w + chunk_w > max_width {
                finish_line(&mut out, &mut line, &mut y, max_width, align, metrics);
            }
            let leading_space = !line.fragments.is_empty();
            place(&mut line, chunk, char_w, row_h, &word.style, leading_space);
        }
    }

    if !line.fragments.is_empty() {
        finish_line(&mut out, &mut line, &mut y, max_width, align, metrics);
    }

    (out, y)
}

/// Split a word that cannot fit on a line of its own into pieces that can.
///
/// Long unbroken strings — URLs, base64, a table of hyphens — would otherwise
/// run off the edge of the page and be unreadable.
fn break_long(text: &str, char_w: f32, max_width: f32) -> Vec<&str> {
    let width = text.chars().count() as f32 * char_w;
    if width <= max_width || char_w <= 0.0 {
        return alloc::vec![text];
    }
    let per_line = ((max_width / char_w) as usize).max(1);

    let mut pieces = Vec::new();
    let mut start = 0usize;
    let mut taken = 0usize;
    for (offset, _) in text.char_indices() {
        if taken == per_line {
            pieces.push(&text[start..offset]);
            start = offset;
            taken = 0;
        }
        taken += 1;
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

/// Add a picture to the current line, which is already known to have room.
///
/// The line grows to the picture's height, so a tall picture pushes the text
/// beside it down with it rather than being drawn over its neighbours.
fn place_image(
    line: &mut Line,
    word: &Word,
    image: &ImageWord,
    width: f32,
    height: f32,
    char_w: f32,
) {
    // A space stands between a picture and whatever shares its line, exactly as
    // one stands between two words.
    let space_w = if line.fragments.is_empty() { 0.0 } else { char_w };

    line.fragments.push(Fragment {
        x: line.width + space_w,
        y: 0.0,
        width,
        height,
        text: fit_alt(&word.text, width, char_w),
        style: word.style.clone(),
        image: Some(ImagePaint { src: image.src.clone(), ready: image.ready }),
        field: None,
    });

    line.width += space_w + width;
    if height > line.height {
        line.height = height;
    }
}

/// Append text to the current line, extending the previous fragment when the
/// style matches so a phrase is one draw call and one underline.
fn place(
    line: &mut Line,
    text: &str,
    char_w: f32,
    row_h: f32,
    style: &TextStyle,
    leading_space: bool,
) {
    let space_w = if leading_space { char_w } else { 0.0 };
    let text_w = text.chars().count() as f32 * char_w;

    match line.fragments.last_mut() {
        // A picture and a control are never extended, however well the style
        // matches: their text is a label and their width is a box, not a count
        // of characters.
        Some(last)
            if last.style == *style && last.image.is_none() && last.field.is_none() =>
        {
            if leading_space {
                last.text.push(' ');
            }
            last.text.push_str(text);
            last.width += space_w + text_w;
        }
        _ => line.fragments.push(Fragment {
            x: line.width + space_w,
            y: 0.0,
            width: text_w,
            height: row_h,
            text: text.to_string(),
            style: style.clone(),
            image: None,
            field: None,
        }),
    }

    line.width += space_w + text_w;
    if row_h > line.height {
        line.height = row_h;
    }
}

/// Position the finished line's fragments and start a new one.
fn finish_line(
    out: &mut Vec<Fragment>,
    line: &mut Line,
    y: &mut f32,
    max_width: f32,
    align: TextAlign,
    metrics: Metrics,
) {
    let shift = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => ((max_width - line.width) / 2.0).max(0.0),
        TextAlign::Right => (max_width - line.width).max(0.0),
    };

    for mut fragment in line.fragments.drain(..) {
        fragment.x += shift;
        // Mixed sizes on one line sit on a common bottom edge, which is a
        // reasonable stand-in for a real baseline.
        fragment.y = *y + (line.height - fragment.height);
        out.push(fragment);
    }

    *y += line.height;
    *line = Line::new(metrics.line_h);
}

// ── Layout ──────────────────────────────────────────────────────────────────

/// Resolve every edge of a box against its containing block's width.
///
/// CSS resolves vertical padding and margins against the *width* too, which is
/// surprising but correct.
fn resolve_edges(root: &mut LayoutBox, available: f32) {
    let Some(style) = root.styled() else {
        root.dimensions.content.width = available;
        return;
    };

    let d = &mut root.dimensions;
    d.margin.left = style.length("margin-left", available);
    d.margin.right = style.length("margin-right", available);
    d.margin.top = style.length("margin-top", available);
    d.margin.bottom = style.length("margin-bottom", available);
    d.padding.left = style.length("padding-left", available);
    d.padding.right = style.length("padding-right", available);
    d.padding.top = style.length("padding-top", available);
    d.padding.bottom = style.length("padding-bottom", available);
    d.border.left = style.length("border-left-width", available);
    d.border.right = style.length("border-right-width", available);
    d.border.top = style.length("border-top-width", available);
    d.border.bottom = style.length("border-bottom-width", available);

    let surround = d.margin.left + d.margin.right
        + d.padding.left + d.padding.right
        + d.border.left + d.border.right;

    let room = (available - surround).max(0.0);
    let explicit = match style.keyword("width") {
        Some("auto") => None,
        _ => style.value("width").and_then(|v| v.to_px(available)),
    };

    // A page asking for more width than there is gets clamped rather than
    // being allowed to run off the side of the window.
    // A keyword here is `none` or `auto`, neither of which constrains anything.
    let limit = |name: &str| match style.keyword(name) {
        Some(_) => None,
        None => style.value(name).and_then(|v| v.to_px(available)),
    };
    let mut width = explicit.unwrap_or(room);
    if let Some(max) = limit("max-width") {
        width = width.min(max);
    }
    if let Some(min) = limit("min-width") {
        width = width.max(min);
    }
    d.content.width = width.clamp(0.0, room);

    // `margin: 0 auto` is how most pages centre a column, and it only means
    // anything once the box is narrower than its container — which is exactly
    // what `max-width` above has just made it.
    let leftover = room - d.content.width;
    if leftover > 0.0 {
        let auto = |side| style.keyword(side) == Some("auto");
        match (auto("margin-left"), auto("margin-right")) {
            (true, true) => {
                d.margin.left += leftover / 2.0;
                d.margin.right += leftover / 2.0;
            }
            (true, false) => d.margin.left += leftover,
            (false, true) => d.margin.right += leftover,
            (false, false) => {}
        }
    }
}

fn layout_children(root: &mut LayoutBox, metrics: Metrics, depth: usize) {
    // An inline box has no children; it wraps to the width just resolved.
    if let BoxKind::Inline(inline) = &mut root.kind {
        let width = root.dimensions.content.width.max(metrics.char_w);
        let (fragments, height) = wrap(&inline.words, width, metrics, inline.align);
        inline.fragments = fragments;
        inline.height = height;
        root.dimensions.content.height = height;
        return;
    }

    if depth >= MAX_DEPTH {
        return;
    }

    if !root.columns.is_empty() {
        layout_row(root, metrics, depth);
        return;
    }

    let origin = root.dimensions.content;
    let mut cursor_y = origin.y;
    // Adjacent margins collapse: only the larger of the two gaps applies.
    let mut previous_margin = 0.0f32;

    for child in root.children.iter_mut() {
        resolve_edges(child, origin.width);

        let d = child.dimensions;
        let gap = (d.margin.top - previous_margin).max(0.0);
        cursor_y += gap;

        child.dimensions.content.x = origin.x + d.margin.left + d.border.left + d.padding.left;
        child.dimensions.content.y = cursor_y + d.border.top + d.padding.top;

        layout_children(child, metrics, depth + 1);
        apply_explicit_height(child);

        // The cursor carries the bottom margin, and the next sibling only adds
        // whatever its own top margin has over it. Between them the gap works
        // out as the larger of the two, which is what collapsing means.
        let laid = child.dimensions;
        cursor_y = laid.content.y
            + laid.content.height
            + laid.padding.bottom
            + laid.border.bottom
            + laid.margin.bottom;
        previous_margin = laid.margin.bottom;
    }

    root.dimensions.content.height = (cursor_y - origin.y).max(0.0);
}

/// Place a table row's cells side by side, each in its column's share of the
/// width. The row is as tall as its tallest cell.
fn layout_row(root: &mut LayoutBox, metrics: Metrics, depth: usize) {
    let origin = root.dimensions.content;
    let columns = core::mem::take(&mut root.columns);
    let fallback = 1.0 / (root.children.len().max(1) as f32);

    let mut x = origin.x;
    let mut tallest = 0.0f32;

    for (i, cell) in root.children.iter_mut().enumerate() {
        let share = (columns.get(i).copied().unwrap_or(fallback) * origin.width).max(metrics.char_w);

        resolve_edges(cell, share);
        let d = cell.dimensions;
        cell.dimensions.content.x = x + d.margin.left + d.border.left + d.padding.left;
        cell.dimensions.content.y = origin.y + d.margin.top + d.border.top + d.padding.top;

        layout_children(cell, metrics, depth + 1);
        apply_explicit_height(cell);

        let laid = cell.dimensions;
        let outer = laid.margin.top + laid.border.top + laid.padding.top
            + laid.content.height
            + laid.padding.bottom + laid.border.bottom + laid.margin.bottom;
        tallest = tallest.max(outer);
        x += share;
    }

    // Backgrounds and borders read as a row rather than as ragged boxes when
    // every cell is stretched to the tallest one.
    for cell in root.children.iter_mut() {
        let d = cell.dimensions;
        let vertical = d.margin.top + d.border.top + d.padding.top
            + d.padding.bottom + d.border.bottom + d.margin.bottom;
        cell.dimensions.content.height = (tallest - vertical).max(d.content.height);
    }

    root.dimensions.content.height = tallest;
    root.columns = columns;
}

fn apply_explicit_height(root: &mut LayoutBox) {
    let Some(style) = root.styled() else { return };
    if style.keyword("height") == Some("auto") {
        return;
    }
    if let Some(h) = style.value("height").and_then(|v| v.to_px(0.0)) {
        // Only ever grow: a declared height smaller than the content would
        // clip text, and there is no overflow handling to clip it with.
        if h > root.dimensions.content.height {
            root.dimensions.content.height = h;
        }
    }
}
