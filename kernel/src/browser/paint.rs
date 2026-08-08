//! Turning a box tree into a flat display list.
//!
//! Painting is separated from drawing: this module produces owned commands in
//! page coordinates, and the window code blits them with a scroll offset. That
//! keeps the expensive parse/style/layout work out of the per-frame path — a
//! page is laid out once and repainted from the list on every scroll.

use alloc::string::String;
use alloc::vec::Vec;

use crate::color::Color;
use crate::framebuffer::TextSize;

use super::dom::{NodeId, MAX_DEPTH, NO_NODE};
use super::forms::{self, Kind};
use super::images::PLACEHOLDER_INSET;
use super::layout::{BoxKind, Fragment, InlineBox, LayoutBox, Rect};

pub enum DisplayCommand {
    SolidRect {
        rect: Rect,
        color: Color,
    },
    Text {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        text: String,
        color: Color,
        size: TextSize,
        bold: bool,
        underline: bool,
        strike: bool,
    },
    Image {
        rect: Rect,
        /// The `src` attribute, so the window layer can find the decoded pixels
        /// in the page's `ImageStore`.
        src: String,
    },
    Field(FieldBox),
}

/// The box kept for a form control.
///
/// What is in it is deliberately absent: a keystroke has to appear without the
/// page being laid out again, so the window layer looks the value up in the
/// page's [`super::Forms`] table by node — the same arrangement that lets a
/// picture arrive without a relayout.
#[derive(Clone, Copy)]
pub struct FieldBox {
    pub rect: Rect,
    pub node: NodeId,
    pub kind: Kind,
    /// The face the control inherited, which is what its contents are measured
    /// and drawn with.
    pub size: TextSize,
}

/// A region of the page a click can land on.
pub struct HitRegion {
    pub rect: Rect,
    /// Index into the page's link target table, if this region is a link.
    pub target: Option<usize>,
    /// The element to dispatch a click event against.
    pub node: NodeId,
}

#[derive(Default)]
pub struct DisplayList {
    pub commands: Vec<DisplayCommand>,
    pub hits: Vec<HitRegion>,
    /// Where each node ended up, in page coordinates, so that a script asking
    /// for `getBoundingClientRect` can be answered.
    ///
    /// Not the same thing as [`Self::hits`], which only carries the inline
    /// fragments a click can land on: a `<div>` has a box and no fragments, and
    /// a paragraph has one entry per line. A node may appear more than once and
    /// the caller unions what it finds.
    pub geometry: Vec<(NodeId, Rect)>,
    /// Total page height in pixels.
    pub height: f32,
}

/// Flatten a laid-out box tree into drawing commands.
pub fn build(root: &LayoutBox) -> DisplayList {
    let mut list = DisplayList::default();
    render_box(root, &mut list, 0);
    list.height = root.dimensions.border_box().height.max(0.0);
    list
}

fn render_box(root: &LayoutBox, list: &mut DisplayList, depth: usize) {
    if depth >= MAX_DEPTH {
        return;
    }

    match &root.kind {
        BoxKind::Block(styled) => {
            if styled.node.id != NO_NODE {
                list.geometry.push((styled.node.id, root.dimensions.border_box()));
            }
            render_background(root, list);
            render_borders(root, list);
        }
        BoxKind::Inline(inline) => {
            render_inline(root, inline, list);
            return;
        }
    }

    for child in &root.children {
        render_box(child, list, depth + 1);
    }
}

fn render_background(root: &LayoutBox, list: &mut DisplayList) {
    let Some(style) = block_style(root) else { return };
    let Some(color) = style.color("background-color") else { return };
    list.commands.push(DisplayCommand::SolidRect {
        rect: root.dimensions.border_box(),
        color,
    });
}

fn render_borders(root: &LayoutBox, list: &mut DisplayList) {
    let Some(style) = block_style(root) else { return };
    // Without an explicit colour a border still needs to be visible; a light
    // grey reads as a rule or a box edge on both themes.
    let color = style
        .color("border-color")
        .unwrap_or(Color::hex(0xCBD5E1));

    let d = &root.dimensions;
    let border_box = d.border_box();
    let edges = [
        // top
        (d.border.top, Rect { height: d.border.top, ..border_box }),
        // bottom
        (
            d.border.bottom,
            Rect {
                y: border_box.y + border_box.height - d.border.bottom,
                height: d.border.bottom,
                ..border_box
            },
        ),
        // left
        (d.border.left, Rect { width: d.border.left, ..border_box }),
        // right
        (
            d.border.right,
            Rect {
                x: border_box.x + border_box.width - d.border.right,
                width: d.border.right,
                ..border_box
            },
        ),
    ];

    for (width, rect) in edges {
        if width > 0.0 {
            list.commands.push(DisplayCommand::SolidRect { rect, color });
        }
    }
}

fn render_inline(root: &LayoutBox, inline: &InlineBox, list: &mut DisplayList) {
    let origin = root.dimensions.content;

    for frag in &inline.fragments {
        let x = origin.x + frag.x;
        let y = origin.y + frag.y;
        let rect = Rect { x, y, width: frag.width, height: frag.height };

        // An inline background is painted before the text so the glyphs land
        // on top of it.
        if let Some(color) = frag.style.background {
            list.commands.push(DisplayCommand::SolidRect { rect, color });
        }

        if frag.style.link.is_some() || frag.style.node != NO_NODE {
            list.hits.push(HitRegion { rect, target: frag.style.link, node: frag.style.node });
        }
        if frag.style.node != NO_NODE {
            list.geometry.push((frag.style.node, rect));
        }

        if let Some(kind) = frag.field {
            render_field(rect, kind, frag, list);
            continue;
        }

        // A picture is a box of pixels rather than a run of glyphs, and the
        // pixels themselves live outside the display list: the window layer
        // looks them up by src, which is what lets one arrive without the page
        // having to be laid out again.
        if let Some(image) = &frag.image {
            if image.ready {
                list.commands.push(DisplayCommand::Image { rect, src: image.src.clone() });
            } else {
                render_placeholder(rect, frag, list);
            }
            continue;
        }

        list.commands.push(DisplayCommand::Text {
            x,
            y,
            width: frag.width,
            height: frag.height,
            text: frag.text.clone(),
            color: frag.style.color,
            size: frag.style.size,
            bold: frag.style.bold,
            underline: frag.style.underline,
            strike: frag.style.strike,
        });
    }
}

/// Draw a form control: the box, and a button's label inside it.
///
/// A field's contents are not here at all — see [`FieldBox`] — but a label is,
/// because it comes from the document and changes only when the document does.
fn render_field(rect: Rect, kind: Kind, frag: &Fragment, list: &mut DisplayList) {
    list.commands.push(DisplayCommand::Field(FieldBox {
        rect,
        node: frag.style.node,
        kind,
        size: frag.style.size,
    }));

    if frag.text.is_empty() {
        return;
    }
    let row_h = frag.style.size.row_h() as f32;
    let text_w = frag.text.chars().count() as f32 * frag.style.size.char_w() as f32;
    list.commands.push(DisplayCommand::Text {
        // Centred, the way a button's label is everywhere else in the desktop.
        x: rect.x + ((rect.width - text_w) / 2.0).max(forms::INSET),
        y: rect.y + ((rect.height - row_h) / 2.0).max(0.0),
        width: text_w,
        height: row_h.min(rect.height),
        text: frag.text.clone(),
        color: frag.style.color,
        size: frag.style.size,
        bold: frag.style.bold,
        // A label is not a link however it got here, and an underline through
        // the middle of a button reads as a mistake.
        underline: false,
        strike: false,
    });
}

/// Draw the space kept for a picture that is not there: an empty frame with as
/// much of the alt text as was made to fit inside it.
///
/// The frame matters as much as the text. A page whose pictures are still
/// arriving looks unfinished rather than broken, and one whose pictures failed
/// still says what they were of.
fn render_placeholder(rect: Rect, frag: &Fragment, list: &mut DisplayList) {
    // A mid grey reads as an edge on a white page and on the dark ones the
    // image search results use, which a border taken from the text colour
    // would not.
    const EDGE: Color = Color::hex(0x94A3B8);
    /// Smallest box worth framing. Old pages are held together with one-pixel
    /// spacers and rules, and a grey dot in place of each would be far worse
    /// than the nothing they were meant to be — the space is still kept.
    const MIN_FRAMED: f32 = 16.0;

    if rect.width >= MIN_FRAMED && rect.height >= MIN_FRAMED {
        let edges = [
            Rect { height: 1.0, ..rect },
            Rect { y: rect.y + rect.height - 1.0, height: 1.0, ..rect },
            Rect { width: 1.0, ..rect },
            Rect { x: rect.x + rect.width - 1.0, width: 1.0, ..rect },
        ];
        for edge in edges {
            list.commands.push(DisplayCommand::SolidRect { rect: edge, color: EDGE });
        }
    }

    if frag.text.is_empty() {
        return;
    }
    list.commands.push(DisplayCommand::Text {
        x: rect.x + PLACEHOLDER_INSET,
        y: rect.y + PLACEHOLDER_INSET,
        width: (rect.width - 2.0 * PLACEHOLDER_INSET).max(0.0),
        // One row, clipped to the frame: the alt text was already cut to the
        // width, and a frame shorter than a line of text has no room for it.
        height: (frag.style.size.row_h() as f32).min(rect.height),
        text: frag.text.clone(),
        color: frag.style.color,
        size: frag.style.size,
        bold: frag.style.bold,
        underline: frag.style.underline,
        strike: frag.style.strike,
    });
}

fn block_style<'a>(root: &'a LayoutBox<'a>) -> Option<&'a super::style::StyledNode<'a>> {
    match root.kind {
        BoxKind::Block(node) => Some(node),
        BoxKind::Inline(_) => None,
    }
}
