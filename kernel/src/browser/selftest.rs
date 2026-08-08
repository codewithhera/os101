//! Boot-time checks for the rendering engine.
//!
//! Every stage is exercised on a document written to trip the failures that
//! matter: unclosed tags, entities, `<script>` leaking into the page, cascade
//! ordering, text wrapping at a known width, and pictures that have not
//! arrived. All of it is pure computation, so it runs at boot with nothing
//! attached — the pictures are made up on the spot rather than decoded.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::color::Color;
use crate::image::Image;
use crate::selftest::Report;

use super::forms::Kind;
use super::images::{dimension_attr, ImageStore};
use super::{css, dom, entities, forms, htmlparse, layout, style, DisplayCommand, Metrics, Viewport};

/// Eight-pixel characters over sixteen-pixel rows, matching the GUI font.
const METRICS: Metrics = Metrics { char_w: 8.0, line_h: 16.0 };
const VIEWPORT: Viewport =
    Viewport { width: 480.0, height: 320.0, char_w: 8.0, line_h: 16.0 };

fn sheet(source: &str) -> css::Stylesheet {
    css::parse(source, VIEWPORT)
}

fn value(source: &str) -> Option<css::Value> {
    css::parse_value(source, VIEWPORT)
}

/// Unit conversion multiplies by a scale that is not exact in binary, so
/// lengths are compared with a tolerance rather than for equality.
fn px_near(source: &str, expected: f32) -> bool {
    match value(source).and_then(|v| v.to_px(0.0)) {
        Some(actual) => {
            let diff = actual - expected;
            diff > -0.01 && diff < 0.01
        }
        None => false,
    }
}

pub fn run() -> Report {
    let mut r = Report::new();

    parsing(&mut r);
    entity_decoding(&mut r);
    stylesheets(&mut r);
    cascade(&mut r);
    boxes(&mut r);
    pictures(&mut r);
    super::forms::selftest(&mut r);
    controls(&mut r);
    full_page(&mut r);
    scripting(&mut r);
    urls(&mut r);

    r
}

/// Where script meets the rendering pipeline.
///
/// The binding itself has its own boot self-test — [`super::script::selftest`],
/// which is where the DOM surface, the event phases and the time budget are
/// checked. What is here is the part that belongs to the engine rather than to
/// the bridge: that a script's change to the document really does go back through
/// style, layout and paint, and that a page whose script fails is still a page.
fn scripting(r: &mut Report) {
    // A script that runs at load and rewrites an element's text, which has to
    // reach the display list before the page is ever shown.
    let loaded = session(
        "<body><p id='out'>before</p>\
         <script>document.getElementById('out').textContent = 'after'</script></body>",
    );
    r.check("script ran at load", page_text(&loaded.page).contains("after"));
    r.check("old text replaced", !page_text(&loaded.page).contains("before"));

    // An inline style written from script has to go through the cascade, which
    // means the colour the painter uses comes out of the declaration.
    let mut styled = session(
        "<body><p id='p' style='margin: 0'>x</p>\
         <script>document.getElementById('p').style.color = '#ff0000'</script></body>",
    );
    r.check(
        "style reaches the painter",
        find_text(&styled.page, "x").map(|(c, _, _)| c) == Some(Color::rgb(255, 0, 0)),
    );
    r.check(
        "existing declarations are kept",
        styled.eval("document.getElementById('p').style.margin").as_deref() == Ok("0"),
    );

    // A `<style>` element a script appends has to be picked up too, since the
    // author stylesheet is collected from the document on every layout.
    let injected = session(
        "<body><p>styled</p>\
         <script>var s = document.createElement('style');\
           s.textContent = 'p { color: #00ff00 }';\
           document.head.appendChild(s)</script></body>",
    );
    r.check(
        "a stylesheet added from script is applied",
        find_text(&injected.page, "styled").map(|(c, _, _)| c) == Some(Color::rgb(0, 255, 0)),
    );

    // A new element has to be laid out and painted like any other.
    let built = session(
        "<body><ul id='list'></ul>\
         <script>var li = document.createElement('li'); li.textContent = 'added';\
           document.getElementById('list').appendChild(li)</script></body>",
    );
    r.check("an appended node renders", page_text(&built.page).contains("added"));

    // A click handler that rewrites the page relayouts once per dispatch.
    let mut live = session(
        "<body><p id='t'>zero</p><button id='go'>Go</button>\
         <script>var n = 0;\
           document.getElementById('go').addEventListener('click', function () {\
             n += 1; document.getElementById('t').textContent = 'count ' + n });\
         </script></body>",
    );
    let button = node_with_text(&live.page, "Go");
    r.check("a button is hit-testable", button.is_some());
    if let Some(id) = button {
        let before = live.page.layouts();
        r.check("the click was handled", live.dispatch_click(id).handled);
        r.check("the change was repainted", page_text(&live.page).contains("count 1"));
        r.check("and it cost one layout", live.page.layouts() == before + 1);
    }

    // A page whose script is broken is still a page, and one whose script never
    // returns must not take the machine with it.
    let broken = session("<body><p>intact</p><script>this is not javascript(</script></body>");
    r.check("a broken script survives", page_text(&broken.page).contains("intact"));

    // console.log and alert are captured rather than lost: the browser shows the
    // newest on its status line.
    let talking = session("<body><script>console.log('hello', 1); alert('hi')</script></body>");
    r.check("console captured", talking.log.iter().any(|m| m.contains("hello 1")));
    r.check("alert captured", talking.log.iter().any(|m| m == "hi"));
}

fn parsing(r: &mut Report) {
    let tree = htmlparse::parse("<div><p>one<p>two</div>");
    // `<p>` closes an open `<p>`, so both paragraphs are siblings in the div.
    let div = tree.find_tag("div");
    r.check("parse finds div", div.is_some());
    let paragraphs = div
        .map(|d| d.children.iter().filter(|c| c.tag() == "p").count())
        .unwrap_or(0);
    r.check("implicit paragraph close", paragraphs == 2);

    // Void elements must not swallow the rest of the document.
    let tree = htmlparse::parse("<p>a<br>b<img src=x>c</p>");
    r.check("void elements", tree.text_content().contains("abc"));

    // An unclosed tag at end of input must still terminate.
    let tree = htmlparse::parse("<div><span>text");
    r.check("unclosed tags recovered", tree.text_content().contains("text"));

    // Comments and doctypes are not content.
    let tree = htmlparse::parse("<!DOCTYPE html><!-- hidden --><p>shown</p>");
    let text = tree.text_content();
    r.check("comment dropped", !text.contains("hidden"));
    r.check("doctype dropped", !text.contains("DOCTYPE"));
    r.check("content kept", text.contains("shown"));

    // Attributes, quoted and bare.
    let tree = htmlparse::parse("<a href='/x' class=\"a b\" data-n=3>link</a>");
    let anchor = tree.find_tag("a").and_then(|n| n.as_element());
    r.check("single-quoted attribute", anchor.and_then(|e| e.attr("href")) == Some("/x"));
    r.check("bare attribute", anchor.and_then(|e| e.attr("data-n")) == Some("3"));
    r.check(
        "class list",
        anchor.map(|e| e.classes().count()) == Some(2),
    );
}

fn entity_decoding(r: &mut Report) {
    r.check("named entity", entities::decode_entities("a&amp;b") == "a&b");
    r.check("numeric entity", entities::decode_entities("A&#66;C") == "ABC");
    r.check("hex entity", entities::decode_entities("&#x41;") == "A");
    r.check("angle brackets", entities::decode_entities("&lt;p&gt;") == "<p>");
    r.check(
        "unknown entity left alone",
        entities::decode_entities("a&bogus;b") == "a&bogus;b",
    );
    // A stray `&` followed by non-ASCII text used to panic the kernel: the
    // scan for the closing `;` cut the string at a fixed byte offset, which
    // fell inside a multi-byte character.
    r.check(
        "ampersand before non-ascii",
        entities::decode_entities("a & für Grüße") == "a & für Grüße",
    );
    r.check(
        "entity after non-ascii",
        entities::decode_entities("Grüße &amp; more") == "Grüße & more",
    );
    r.check(
        "multibyte at the byte limit",
        entities::decode_entities("&xxxxxxxxxxüe;") == "&xxxxxxxxxxüe;",
    );
}

fn stylesheets(r: &mut Report) {
    let basic = sheet("p, .lead { color: #ff0000; margin: 4px 8px } /* c */ #x { color: blue }");
    r.check("rule count", basic.rules.len() == 2);
    r.check(
        "selector list",
        basic.rules.first().map(|r| r.selectors.len()) == Some(2),
    );

    // `margin: 4px 8px` becomes four longhands.
    let margins = basic.rules[0]
        .declarations
        .iter()
        .filter(|d| d.name.starts_with("margin-"))
        .count();
    r.check("margin shorthand expands", margins == 4);

    r.check("hex colour", css::parse_color("#ff0000") == Some(Color::rgb(255, 0, 0)));
    r.check("short hex colour", css::parse_color("#f00") == Some(Color::rgb(255, 0, 0)));
    r.check("rgb() colour", css::parse_color("rgb(1, 2, 3)") == Some(Color::rgb(1, 2, 3)));
    r.check("named colour", css::parse_color("navy") == Some(Color::hex(0x000080)));
    r.check("nonsense colour rejected", css::parse_color("notacolour").is_none());

    // Lengths, including a fractional value, without `str::parse::<f32>`.
    r.check(
        "px length",
        value("12px").and_then(|v| v.to_px(0.0)) == Some(12.0),
    );
    r.check(
        "fractional length",
        value("1.5px").and_then(|v| v.to_px(0.0)) == Some(1.5),
    );
    r.check(
        "percentage resolves",
        value("50%").and_then(|v| v.to_px(200.0)) == Some(100.0),
    );
    // Keywords that happen to end in a unit are keywords, not broken lengths.
    r.check(
        "keyword ending in a unit",
        value("list-item").and_then(|v| v.keyword().map(|k| k == "list-item"))
            == Some(true),
    );
    r.check(
        "bare number is a length",
        value("3").and_then(|v| v.to_px(0.0)) == Some(3.0),
    );

    // Viewport units resolve against the viewport, not a fixed guess.
    r.check("vw resolves", px_near("50vw", VIEWPORT.width / 2.0));
    r.check("vh resolves", px_near("10vh", VIEWPORT.height / 10.0));
    r.check("vmin resolves", px_near("100vmin", VIEWPORT.height));
    r.check("rem is not read as em", px_near("2rem", 2.0 * VIEWPORT.line_h));

    // An at-rule and its block must be skipped without eating what follows.
    let after_at_rule = sheet("@media screen { p { color: red } } h1 { color: lime }");
    r.check("at-rule skipped", after_at_rule.rules.len() == 1);
    r.check(
        "rule after at-rule",
        subject_tag(&after_at_rule, 0).as_deref() == Some("h1"),
    );

    // Combinators are kept rather than reduced to the rightmost compound.
    let combinators = sheet("nav ul > li.active { color: red }");
    let selector = combinators.rules.first().and_then(|r| r.selectors.first());
    r.check("combinator parts", selector.map(|s| s.parts.len()) == Some(3));
    r.check(
        "child combinator kept",
        selector.map(|s| s.combinators.clone())
            == Some(alloc::vec![css::Combinator::Descendant, css::Combinator::Child]),
    );
    r.check(
        "combinator specificity sums",
        selector.map(|s| s.specificity()) == Some((0, 1, 3)),
    );

    // Attribute selectors are parsed, and their contents are not mistaken
    // for a class or an id.
    let attrs = sheet("a[href][target=\"_blank\"].ext { color: red }");
    let subject = attrs.rules.first().and_then(|r| r.selectors.first()).and_then(|s| s.subject());
    r.check("attribute selector count", subject.map(|c| c.attrs.len()) == Some(2));
    r.check("attribute tag survives", subject.and_then(|c| c.tag.clone()).as_deref() == Some("a"));
    r.check("attribute class survives", subject.map(|c| c.classes.len()) == Some(1));
    r.check(
        "attribute value parsed",
        subject.and_then(|c| c.attrs.get(1).cloned())
            == Some(("target".into(), Some("_blank".into()))),
    );
}

fn cascade(r: &mut Report) {
    let document = htmlparse::parse(
        "<body><p class='hi' id='one' style='color:#00ff00'>a</p><p>b</p></body>",
    );
    let ua = sheet("p { color: #000000; display: block }");
    let author = sheet("p { color: #ff0000 } .hi { color: #0000ff }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);

    let paragraphs = collect_tag(&styled, "p");
    r.check("two paragraphs styled", paragraphs.len() == 2);

    // The inline attribute beats every rule.
    r.check(
        "inline style wins",
        paragraphs.first().and_then(|n| n.color("color")) == Some(Color::rgb(0, 255, 0)),
    );
    // With no inline style, the author sheet beats the user agent sheet.
    r.check(
        "author beats user agent",
        paragraphs.get(1).and_then(|n| n.color("color")) == Some(Color::rgb(255, 0, 0)),
    );
    r.check(
        "user agent display applies",
        paragraphs.first().map(|n| n.display()) == Some(style::Display::Block),
    );

    // A class selector outranks a bare tag selector.
    let author = sheet(".hi { color: #0000ff } p { color: #ff0000 }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let with_class = collect_tag(&styled, "p");
    r.check(
        "specificity respected",
        with_class
            .first()
            .and_then(|n| n.color("color"))
            // Still green: the inline style outranks both.
            == Some(Color::rgb(0, 255, 0)),
    );

    // Colour inherits into descendants that set nothing of their own. The
    // user-agent sheet must not name a colour here, or `p` would stop the
    // inherited one at the paragraph.
    let document = htmlparse::parse("<body><p><em>deep</em></p></body>");
    let ua = sheet("body, p { display: block }");
    let author = sheet("body { color: #123456 }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let em = collect_tag(&styled, "em");
    r.check(
        "colour inherits",
        em.first().and_then(|n| n.color("color")) == Some(Color::hex(0x123456)),
    );

    // Descendant and child combinators have to consult the ancestor chain,
    // not just the element in front of them.
    let document = htmlparse::parse(
        "<body><nav><ul><li><b>in</b></li></ul></nav><ul><li><b>out</b></li></ul></body>",
    );
    let ua = sheet("body, nav, ul, li { display: block }");
    let author = sheet("nav b { color: #ff0000 }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let bolds = collect_tag(&styled, "b");
    r.check("two candidates", bolds.len() == 2);
    r.check(
        "descendant matches inside",
        bolds.first().and_then(|n| n.color("color")) == Some(Color::rgb(255, 0, 0)),
    );
    r.check(
        "descendant misses outside",
        bolds.get(1).and_then(|n| n.color("color")).is_none(),
    );

    // `nav > b` must not match a `b` that is a grandchild of the nav.
    let author = sheet("nav > b { color: #00ff00 }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    r.check(
        "child combinator is strict",
        collect_tag(&styled, "b")
            .first()
            .and_then(|n| n.color("color"))
            .is_none(),
    );

    // Attribute selectors match on presence and on an exact value.
    let document = htmlparse::parse("<body><a href='#' target='_blank'>x</a><a>y</a></body>");
    let ua = sheet("body { display: block }");
    let author = sheet("a[target=\"_blank\"] { color: #0000ff }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let anchors = collect_tag(&styled, "a");
    r.check(
        "attribute selector matches",
        anchors.first().and_then(|n| n.color("color")) == Some(Color::rgb(0, 0, 255)),
    );
    r.check(
        "attribute selector rejects",
        anchors.get(1).and_then(|n| n.color("color")).is_none(),
    );

    // Font size is inherited and rounds to a face that exists.
    let document = htmlparse::parse("<body><h1>big</h1><p>normal</p></body>");
    let styled = style::build(
        &document,
        &sheet("body, h1, p { display: block } h1 { font-size: 32px }"),
        &sheet(""),
        VIEWPORT,
    );
    r.check(
        "heading font size",
        collect_tag(&styled, "h1").first().map(|n| n.font_size())
            == Some(crate::framebuffer::TextSize::Huge),
    );
    r.check(
        "body font size",
        collect_tag(&styled, "p").first().map(|n| n.font_size())
            == Some(crate::framebuffer::TextSize::Normal),
    );
}

fn boxes(r: &mut Report) {
    // Ten characters at 8px each need two 40px lines.
    let document = htmlparse::parse("<body><p>aaaa bbbb cc</p></body>");
    let ua = sheet("body, p { display: block }");
    let author = sheet("");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (tree, _) = layout::layout_document(&styled, 40.0, METRICS, &no_images());

    let lines = count_lines(&tree);
    r.check("text wraps to width", lines >= 3);

    // No fragment may overflow the content width.
    r.check("fragments fit", fragments_fit(&tree, 40.0));

    // Padding pushes content inwards and adds to the height.
    let ua = sheet("body, p { display: block } p { padding: 10px }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (padded, _) = layout::layout_document(&styled, 200.0, METRICS, &no_images());
    let p_box = find_block(&padded, "p");
    r.check(
        "padding offsets content",
        p_box.map(|d| d.content.x >= 10.0).unwrap_or(false),
    );
    r.check(
        "padding widens border box",
        p_box
            .map(|d| d.border_box().width - d.content.width == 20.0)
            .unwrap_or(false),
    );

    // Siblings stack rather than overlap.
    let document = htmlparse::parse("<body><p>one</p><p>two</p></body>");
    let ua = sheet("body, p { display: block }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (stacked, _) = layout::layout_document(&styled, 200.0, METRICS, &no_images());
    let tops = block_tops(&stacked, "p");
    r.check("siblings stack", tops.len() == 2 && tops[1] > tops[0]);

    // Adjacent margins collapse: 20px below one paragraph meeting 20px above
    // the next leaves a 20px gap, not 40px.
    let ua = sheet("body, p { display: block } p { margin-top: 20px; margin-bottom: 20px }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (collapsed, _) = layout::layout_document(&styled, 200.0, METRICS, &no_images());
    let tops = block_tops(&collapsed, "p");
    r.check(
        "adjacent margins collapse",
        tops.len() == 2 && (tops[1] - tops[0] - (METRICS.line_h + 20.0)).abs() < 0.5,
    );

    // A word longer than the line is broken rather than left to overflow.
    let document = htmlparse::parse("<body><p>aaaaaaaaaaaaaaaaaaaaaaaa</p></body>");
    let ua = sheet("body, p { display: block }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (broken, _) = layout::layout_document(&styled, 40.0, METRICS, &no_images());
    r.check("long word broken", fragments_fit(&broken, 40.0));
    r.check("long word kept whole", collected_text(&broken).replace('\n', "").len() == 24);

    // Preformatted text keeps its spacing and is not rewrapped.
    let document = htmlparse::parse("<body><pre>a    b\nc</pre></body>");
    let ua = sheet("body { display: block } pre { display: block; white-space: pre }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (pre, _) = layout::layout_document(&styled, 400.0, METRICS, &no_images());
    let text = collected_text(&pre);
    r.check("pre keeps runs of spaces", text.contains("a    b"));
    r.check("pre keeps its own line breaks", text.contains("a    b\nc"));

    // An unclosed tag can leave a heading inside inline content. It still
    // needs a line to itself, and so does whatever follows it.
    let document = htmlparse::parse("<body><span>before<p>middle</p>after</span></body>");
    let ua = sheet("body, p { display: block } span { display: inline }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (fenced, _) = layout::layout_document(&styled, 400.0, METRICS, &no_images());
    r.check(
        "block in inline is fenced",
        collected_text(&fenced) == "before\nmiddle\nafter",
    );

    // A list that ended up inline still reads as a list.
    let document = htmlparse::parse("<body><span><ul><li>one</li><li>two</li></ul></span></body>");
    let ua = sheet("body, ul { display: block } li { display: list-item } span { display: inline }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (inline_list, _) = layout::layout_document(&styled, 400.0, METRICS, &no_images());
    r.check(
        "inline list keeps its markers",
        collected_text(&inline_list) == "* one\n* two",
    );

    // Centring shifts a short line rightwards; left alignment does not.
    let document = htmlparse::parse("<body><p>hi</p></body>");
    let ua = sheet("body, p { display: block }");
    let centred = style::build(&document, &ua, &sheet("p { text-align: center }"), VIEWPORT);
    let (centred, _) = layout::layout_document(&centred, 200.0, METRICS, &no_images());
    let plain = style::build(&document, &ua, &author, VIEWPORT);
    let (plain, _) = layout::layout_document(&plain, 200.0, METRICS, &no_images());
    r.check(
        "text-align centres",
        first_fragment_x(&centred).unwrap_or(0.0) > first_fragment_x(&plain).unwrap_or(0.0),
    );

    // A larger face produces a taller line.
    let document = htmlparse::parse("<body><h1>big</h1></body>");
    let ua = sheet("body, h1 { display: block } h1 { font-size: 32px }");
    let styled = style::build(&document, &ua, &author, VIEWPORT);
    let (large, _) = layout::layout_document(&styled, 400.0, METRICS, &no_images());
    r.check(
        "large text is taller",
        find_block(&large, "h1").map(|d| d.content.height > METRICS.line_h) == Some(true),
    );

    // The column idiom every real page uses: a capped width with auto margins.
    let document = htmlparse::parse("<body><div>column</div></body>");
    let ua = sheet("body, div { display: block }");
    let narrow = sheet("div { max-width: 200px; margin-left: auto; margin-right: auto }");
    let styled = style::build(&document, &ua, &narrow, VIEWPORT);
    let (column, _) = layout::layout_document(&styled, 600.0, METRICS, &no_images());
    let div = find_block(&column, "div");
    r.check(
        "max-width caps the box",
        div.map(|d| (d.content.width - 200.0).abs() < 0.5) == Some(true),
    );
    r.check(
        "auto margins centre the box",
        div.map(|d| (d.content.x - 200.0).abs() < 0.5) == Some(true),
    );

    // min-width keeps a box from collapsing below a floor.
    let wide = sheet("div { width: 10px; min-width: 120px }");
    let styled = style::build(&document, &ua, &wide, VIEWPORT);
    let (floored, _) = layout::layout_document(&styled, 600.0, METRICS, &no_images());
    r.check(
        "min-width raises the box",
        find_block(&floored, "div").map(|d| (d.content.width - 120.0).abs() < 0.5) == Some(true),
    );

    tables(r);
}

/// Table rows put their cells side by side, in columns that line up down the
/// page. Without this a table reads as one cell per line.
fn tables(r: &mut Report) {
    let document = htmlparse::parse(
        "<body><table><tr><td>a</td><td>bbbbbbbbbb</td></tr>\
         <tr><td>c</td><td>d</td></tr></table></body>",
    );
    let ua = sheet(
        "body { display: block } table { display: table } \
         tr { display: table-row } td { display: table-cell }",
    );
    let styled = style::build(&document, &ua, &sheet(""), VIEWPORT);
    let (tree, _) = layout::layout_document(&styled, 400.0, METRICS, &no_images());

    let cells = block_lefts(&tree, "td");
    r.check("all cells laid out", cells.len() == 4);
    r.check(
        "cells sit side by side",
        cells.len() == 4 && cells[1] > cells[0] && cells[3] > cells[2],
    );
    r.check(
        "columns line up between rows",
        cells.len() == 4 && (cells[0] - cells[2]).abs() < 0.5 && (cells[1] - cells[3]).abs() < 0.5,
    );

    // The wide column gets the room, not an equal half.
    let widths = block_widths(&tree, "td");
    r.check(
        "wide column takes more width",
        widths.len() == 4 && widths[1] > widths[0] * 2.0,
    );

    // Rows still stack.
    let rows = block_tops(&tree, "tr");
    r.check("rows stack", rows.len() == 2 && rows[1] > rows[0]);
}

// ── Pictures ────────────────────────────────────────────────────────────────

/// An `<img>` is the one thing on a page whose size is not known when the page
/// is first laid out, so each part is checked separately: the store the window
/// layer fills in, the box a picture is given, how it breaks a line, and what a
/// finished page reports about it.
fn pictures(r: &mut Report) {
    picture_store(r);
    picture_sizing(r);
    picture_lines(r);
    picture_pages(r);
}

/// The store, and the attribute parsing that feeds it.
///
/// The distinction that matters is between a picture that has not arrived and
/// one that failed: layout can only fall back to alt text if it is told.
fn picture_store(r: &mut Report) {
    let mut store = ImageStore::new();
    r.check("store starts empty", store.is_empty() && store.len() == 0);
    r.check("an unfetched src is unknown", !store.known("a.png"));
    r.check("an unfetched src has no size", store.size("a.png").is_none());

    store.insert("a.png", picture(8, 4));
    r.check("a picture is remembered", store.known("a.png"));
    r.check("a picture keeps its size", store.size("a.png") == Some((8, 4)));
    r.check("a picture is handed back", store.get("a.png").is_some());
    r.check("one picture counts as one", store.len() == 1 && !store.is_empty());

    store.fail("b.png");
    r.check("a failure is known", store.known("b.png"));
    r.check("a failure has no size", store.size("b.png").is_none());
    r.check("a failure has no pixels", store.get("b.png").is_none());

    // A reload has to be able to replace what the last fetch found.
    store.insert("a.png", picture(2, 2));
    r.check("a picture can be replaced", store.size("a.png") == Some((2, 2)));

    // An `<img>` with no src names no picture at all.
    store.insert("", picture(1, 1));
    r.check("an empty src is not stored", !store.known("") && store.len() == 2);

    // The fetch and the layout only ever meet through this key, so they have to
    // agree about a src that was written with room around it.
    store.insert("  c.png  ", picture(3, 3));
    r.check("whitespace is not part of a src", store.size("c.png") == Some((3, 3)));

    r.check("bare pixel count", dimension_attr("288") == Some(288.0));
    r.check("dimension with a unit", dimension_attr("288px") == Some(288.0));
    r.check("dimension with spaces", dimension_attr(" 96 ") == Some(96.0));
    r.check("percentage is not a pixel count", dimension_attr("50%").is_none());
    r.check("negative dimension refused", dimension_attr("-5").is_none());
    r.check("word is not a dimension", dimension_attr("wide").is_none());
    r.check("empty dimension refused", dimension_attr("").is_none());
    r.check("absurd dimension capped", dimension_attr("99999999") == Some(8192.0));
}

/// The used size of one picture, in the order it is resolved: the cascade, the
/// attributes, the picture itself, then the aspect ratio.
fn picture_sizing(r: &mut Report) {
    let none = ImageStore::new();
    let mut ready = ImageStore::new();
    ready.insert("p.png", picture(120, 90));

    let declared = "<body><p><img src='p.png' width='100' height='50'></p></body>";
    let boxes = picture_layout(declared, "", 400.0, &none);
    r.check(
        "attributes size the picture",
        boxes.len() == 1 && boxes[0].is(100.0, 50.0),
    );
    let boxes = picture_layout(declared, "img { width: 64px; height: 32px }", 400.0, &none);
    r.check("css size beats the attributes", boxes.len() == 1 && boxes[0].is(64.0, 32.0));
    let boxes = picture_layout(declared, "img { width: 64px }", 400.0, &none);
    r.check(
        "css and attributes can each give one side",
        boxes.len() == 1 && boxes[0].is(64.0, 50.0),
    );
    let boxes = picture_layout(declared, "img { width: 50% }", 400.0, &none);
    r.check(
        "a percentage width is a share of the line",
        boxes.len() == 1 && boxes[0].is(200.0, 50.0),
    );

    // With nothing declared the picture's own size is used, and it is drawn
    // rather than stood in for.
    let bare = "<body><p><img src='p.png' alt='Sunset'></p></body>";
    let boxes = picture_layout(bare, "", 400.0, &ready);
    r.check("natural size used when nothing is declared", boxes.len() == 1 && boxes[0].is(120.0, 90.0));
    r.check("an arrived picture is drawn", boxes.len() == 1 && boxes[0].ready);

    // One declared side and the picture's own shape give the other.
    let boxes = picture_layout("<body><p><img src='p.png' width='60'></p></body>", "", 400.0, &ready);
    r.check("aspect kept from the width", boxes.len() == 1 && boxes[0].is(60.0, 45.0));
    let boxes = picture_layout("<body><p><img src='p.png' height='45'></p></body>", "", 400.0, &ready);
    r.check("aspect kept from the height", boxes.len() == 1 && boxes[0].is(60.0, 45.0));

    // Without the picture there is no shape to keep, so four by three is
    // assumed rather than a square.
    let boxes = picture_layout("<body><p><img src='p.png' width='120'></p></body>", "", 400.0, &none);
    r.check("four by three from the width", boxes.len() == 1 && boxes[0].is(120.0, 90.0));
    let boxes = picture_layout("<body><p><img src='p.png' height='90'></p></body>", "", 400.0, &none);
    r.check("four by three from the height", boxes.len() == 1 && boxes[0].is(120.0, 90.0));

    // Nothing known at all, but something to say: keep a modest box.
    let boxes = picture_layout(bare, "", 400.0, &none);
    r.check("a placeholder box is kept", boxes.len() == 1 && boxes[0].is(240.0, 135.0));
    r.check("a placeholder is not a picture", boxes.len() == 1 && !boxes[0].ready);
    r.check("a placeholder holds the alt text", boxes.len() == 1 && boxes[0].alt == "Sunset");

    // A picture that failed is in the same position, and its alt text is the
    // whole point of the attribute.
    let mut failed = ImageStore::new();
    failed.fail("p.png");
    let boxes = picture_layout(bare, "", 400.0, &failed);
    r.check("a failed picture is not drawn", boxes.len() == 1 && !boxes[0].ready);
    r.check("a failed picture keeps its alt text", boxes.len() == 1 && boxes[0].alt == "Sunset");

    // Nothing to draw and nothing to say: an empty frame would be worse than
    // the gap it fills.
    let boxes = picture_layout("<body><p><img src='p.png'></p></body>", "", 400.0, &none);
    r.check("a silent picture reserves nothing", boxes.is_empty());
    let boxes = picture_layout("<body><p><img src='p.png'></p></body>", "img { width: 0px }", 400.0, &none);
    r.check("a picture asked for no width reserves nothing", boxes.is_empty());

    // A missing src cannot name a picture, but the alt text still stands.
    let boxes = picture_layout("<body><p><img alt='Sunset'></p></body>", "", 400.0, &none);
    r.check("a missing src is not a picture", boxes.len() == 1 && !boxes[0].ready);
    r.check("a missing src names nothing", boxes.len() == 1 && boxes[0].src.is_empty());

    // A photograph off a camera has to be brought back inside the page.
    let boxes = picture_layout(
        "<body><p><img src='p.png' width='4000' height='2000'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check("a wide picture is clamped to the page", boxes.len() == 1 && boxes[0].is(400.0, 200.0));
    let boxes = picture_layout(
        "<body><p><img src='p.png' width='99999999' height='99999999'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check(
        "absurd dimensions are bounded",
        boxes.len() == 1 && boxes[0].width <= 400.5 && boxes[0].height <= 400.5,
    );

    // A description longer than the frame is cut, since nothing clips it later.
    let long = alloc::format!(
        "<body><p><img src='p.png' alt='{}'></p></body>",
        "x".repeat(400)
    );
    let boxes = picture_layout(&long, "", 400.0, &none);
    let room = ((240.0 - 8.0) / glyph_w()) as usize;
    r.check(
        "alt text is cut to the frame",
        boxes.len() == 1 && boxes[0].alt.chars().count() == room,
    );

    // A page is free to make a picture block-level, and it still has a box.
    let boxes = picture_layout(declared, "img { display: block }", 400.0, &none);
    r.check("a block picture keeps its box", boxes.len() == 1 && boxes[0].is(100.0, 50.0));
    let boxes = picture_layout(
        "<body><img src='a' width='100' height='50'><img src='b' width='100' height='50'></body>",
        "img { display: block }",
        400.0,
        &none,
    );
    r.check(
        "block pictures stack",
        boxes.len() == 2 && boxes[1].y >= boxes[0].y + 50.0,
    );

    // And `img` is inline until something says otherwise.
    let document = htmlparse::parse("<body><img src='a'></body>");
    let styled = style::build(&document, &sheet("body { display: block }"), &sheet(""), VIEWPORT);
    r.check(
        "img is inline by default",
        collect_tag(&styled, "img").first().map(|n| n.display()) == Some(style::Display::Inline),
    );
}

/// A picture takes part in line breaking like a very wide word.
fn picture_lines(r: &mut Report) {
    let none = ImageStore::new();

    let pair = picture_layout(
        "<body><p><img src='a' width='180' height='40'>\
         <img src='b' width='180' height='40'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check(
        "two pictures share a line",
        pair.len() == 2 && near(pair[0].y, pair[1].y) && pair[1].x > pair[0].x,
    );
    r.check(
        "a space stands between two pictures",
        pair.len() == 2 && near(pair[1].x, pair[0].x + 180.0 + glyph_w()),
    );

    let trio = picture_layout(
        "<body><p><img src='a' width='180' height='40'>\
         <img src='b' width='180' height='40'>\
         <img src='c' width='180' height='40'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check("a third picture wraps", trio.len() == 3 && trio[2].y > trio[1].y);
    r.check(
        "the wrapped picture starts the line",
        trio.len() == 3 && near(trio[2].x, trio[0].x),
    );

    // The line grows to the picture, so the next one clears it rather than
    // being drawn over it.
    let tall = picture_layout(
        "<body><p><img src='a' width='300' height='200'>\
         <img src='b' width='300' height='200'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check(
        "a line is as tall as the picture on it",
        tall.len() == 2 && tall[1].y >= tall[0].y + 200.0,
    );

    let beside = picture_layout(
        "<body><p>Look<img src='a' width='40' height='20'></p></body>",
        "",
        400.0,
        &none,
    );
    // Four characters and the space that follows them, then the picture.
    r.check(
        "text and a picture share a line",
        beside.len() == 1 && near(beside[0].x, 5.0 * glyph_w()) && near(beside[0].y, 0.0),
    );

    let alone = picture_layout(
        "<body><p>Look<img src='a' width='400' height='100'></p></body>",
        "",
        400.0,
        &none,
    );
    r.check(
        "a picture too wide to share takes its own line",
        alone.len() == 1 && alone[0].y >= METRICS.line_h,
    );
}

/// What a finished page reports: the commands it would blit, the regions a
/// click can land on, and the list of pictures it is still waiting for.
fn picture_pages(r: &mut Report) {
    let document = "<body><p>Before</p>\
                    <a href='/full'><img src='p.png' width='120' height='60' alt='A cat'></a>\
                    <p>After</p></body>";

    // The first pass over any page finds an empty store, which is why the
    // window layer has to lay the page out a second time.
    let waiting = super::render(document, VIEWPORT, METRICS);
    r.check("nothing is blitted before the bytes arrive", painted(&waiting).is_empty());
    r.check("alt text stands in meanwhile", page_text(&waiting).contains("A cat"));
    r.check(
        "the page asks for the picture",
        waiting.image_sources() == alloc::vec![String::from("p.png")],
    );

    let page = page_with(document, &[("p.png", 240, 120)], &[]);
    let drawn = painted(&page);
    r.check("the picture is blitted once it arrives", drawn.len() == 1);
    r.check(
        "the picture is named by its src",
        drawn.first().map(|(src, _)| src.as_str()) == Some("p.png"),
    );
    r.check(
        "the declared box is used, not the natural one",
        drawn.first().map(|(_, rect)| near(rect.width, 120.0) && near(rect.height, 60.0))
            == Some(true),
    );
    r.check("alt text gives way to the picture", !page_text(&page).contains("A cat"));
    r.check(
        "the text around it is untouched",
        page_text(&page).contains("Before") && page_text(&page).contains("After"),
    );

    let centre = drawn
        .first()
        .map(|(_, rect)| (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0));
    r.check(
        "a picture in a link carries the link",
        centre.and_then(|(x, y)| page.link_at(x, y)) == Some("/full"),
    );
    r.check(
        "a picture is a hit region of its own",
        centre.and_then(|(x, y)| page.hit(x, y)).map(|h| h.node)
            == page.dom.find_tag("img").map(|n| n.id),
    );
    r.check(
        "image_at finds the picture",
        centre.and_then(|(x, y)| page.image_at(x, y)) == Some("p.png"),
    );
    r.check(
        "image_at misses beside the picture",
        centre.map(|(_, y)| page.image_at(9000.0, y).is_none()) == Some(true),
    );
    r.check("image_at misses below the page", page.image_at(20.0, 9000.0).is_none());

    // A picture that will never arrive falls back for good, and stays clickable
    // so the link around it can still be followed.
    let broken = page_with(document, &[], &["p.png"]);
    r.check("a failed picture is not blitted", painted(&broken).is_empty());
    r.check("a failed picture shows its alt text", page_text(&broken).contains("A cat"));
    r.check(
        "a failed picture is framed",
        broken.display.commands.iter().any(|c| matches!(
            c,
            DisplayCommand::SolidRect { rect, .. }
                if near(rect.width, 120.0) && near(rect.height, 1.0)
        )),
    );
    r.check(
        "a failed picture is still a link",
        broken.display.hits.iter().any(|h| h.target.is_some()),
    );
    r.check("image_at ignores a failed picture", broken.image_at(20.0, 30.0).is_none());

    // Old pages are held together with one-pixel spacers. The space is kept for
    // them, but a grey dot in place of each would be worse than nothing.
    let spacer = super::render(
        "<body><img src='s.gif' width='1' height='20'></body>",
        VIEWPORT,
        METRICS,
    );
    r.check(
        "a spacer is not framed",
        !spacer
            .display
            .commands
            .iter()
            .any(|c| matches!(c, DisplayCommand::SolidRect { .. })),
    );
    r.check(
        "a spacer still takes its space",
        spacer.image_sources() == alloc::vec![String::from("s.gif")] && spacer.height() >= 20.0,
    );

    // The user-agent sheet must neither hide pictures nor make them blocks.
    let two = page_with(
        "<body><img src='a.png' width='60' height='40'>\
         <img src='b.png' width='60' height='40'></body>",
        &[("a.png", 60, 40), ("b.png", 60, 40)],
        &[],
    );
    let drawn = painted(&two);
    r.check("the user-agent sheet does not hide pictures", drawn.len() == 2);
    r.check(
        "pictures are inline",
        drawn.len() == 2
            && near(drawn[0].1.y, drawn[1].1.y)
            && drawn[1].1.x > drawn[0].1.x,
    );

    // Text after a picture is text, not part of the picture's fragment.
    let mixed = page_with(
        "<body><p><img src='p.png' width='40' height='30' alt='Pic'>beside</p></body>",
        &[("p.png", 40, 30)],
        &[],
    );
    r.check("text after a picture survives", page_text(&mixed).contains("beside"));
    r.check("text after a picture is not swallowed", painted(&mixed).len() == 1);

    // Sources are what the window layer fetches: in order, once each.
    let listed = super::render(
        "<body><img src='one.png'><img src='two.png'><img src='one.png'>\
         <img alt='no src'><img src='  one.png  '></body>",
        VIEWPORT,
        METRICS,
    );
    let sources = listed.image_sources();
    r.check(
        "sources are in document order",
        sources.first().map(|s| s.as_str()) == Some("one.png")
            && sources.get(1).map(|s| s.as_str()) == Some("two.png"),
    );
    r.check("sources are deduplicated", sources.len() == 2);
    r.check("a missing src is not a source", !sources.iter().any(|s| s.is_empty()));

    // A page can be crowded with pictures without falling over, and cannot ask
    // for an unbounded number of fetches.
    let mut many = String::from("<body>");
    for i in 0..(super::images::MAX_PICTURES + 40) {
        many.push_str(&alloc::format!("<img src='p{}.png' width='40' height='30' alt='n'>", i));
    }
    many.push_str("</body>");
    let crowded = super::render(&many, VIEWPORT, METRICS);
    r.check("a crowded page still has height", crowded.height() > 30.0);
    r.check(
        "sources are capped",
        crowded.image_sources().len() == super::images::MAX_PICTURES,
    );

    let hundred = {
        let mut html = String::from("<body>");
        for i in 0..100 {
            html.push_str(&alloc::format!("<img src='p{}.png' width='40' height='30'>", i));
        }
        html.push_str("</body>");
        html
    };
    r.check(
        "a hundred pictures are all laid out",
        picture_layout(&hundred, "", 464.0, &no_images()).len() == 100,
    );

    // And a page full of nonsense must not take the machine with it.
    let junk = super::render(
        "<body><img><img src><img src='' alt=''><img width height alt>\
         <img src='x' width='-1' height='abc'>\
         <img src='y' width='99999999' height='99999999'></body>",
        VIEWPORT,
        METRICS,
    );
    r.check("malformed pictures survive", junk.height() >= 0.0);
    r.check("malformed pictures are not blitted", painted(&junk).is_empty());
}

/// Form controls as the page sees them: the box each one is given, and the
/// click that has to find it.
fn controls(r: &mut Report) {
    let page = super::render(
        "<body><form action=\"/s\">\
           <input name=q size=10>\
           <input type=hidden name=h value=1>\
           <input type=checkbox name=c>\
           <input type=submit value=\"Go\">\
         </form></body>",
        VIEWPORT,
        METRICS,
    );

    let boxes = fields(&page);
    let field = boxes.first().copied();
    let row_h = crate::framebuffer::TextSize::Normal.row_h() as f32;
    let frame = 2.0 * forms::INSET;

    r.check("every visible control is given a box", boxes.len() == 3);
    r.check(
        "size sets a field's width",
        field.map(|f| near(f.rect.width, 10.0 * glyph_w() + frame)) == Some(true),
    );
    r.check(
        "a one-line field is one row deep",
        field.map(|f| near(f.rect.height, row_h + frame)) == Some(true),
    );
    r.check(
        "a hidden control takes up no room",
        !boxes.iter().any(|f| f.kind == Kind::Hidden),
    );
    r.check(
        "a hidden control is still a control",
        page.forms.iter().any(|c| c.name == "h" && c.value == "1"),
    );
    r.check(
        "an unsupported type keeps its box",
        boxes.iter().any(|f| f.kind == Kind::Unsupported),
    );
    r.check("a button's label is drawn", page_text(&page).contains("Go"));
    r.check("a page that is only a form has content", page.has_visible_content());

    // Hit-testing is how a click finds the field to put the caret in.
    let inside = field.and_then(|f| page.field_at(f.rect.x + 1.0, f.rect.y + 1.0));
    r.check(
        "a field is hit-testable",
        inside.map(|f| f.node) == field.map(|f| f.node),
    );
    r.check("a miss finds no field", page.field_at(9000.0, 9000.0).is_none());

    // The cascade outranks the attribute, exactly as it does for a picture.
    let styled = super::render(
        "<body><style>input{width:160px}</style>\
         <form><input name=q size=4></form></body>",
        VIEWPORT,
        METRICS,
    );
    r.check(
        "a declared width beats the size attribute",
        fields(&styled).first().map(|f| near(f.rect.width, 160.0)) == Some(true),
    );

    // A textarea's contents belong to the control, not to the page.
    let area = super::render(
        "<body><form><textarea name=t rows=3 cols=8>hello</textarea></form></body>",
        VIEWPORT,
        METRICS,
    );
    r.check(
        "a textarea is as deep as its rows",
        fields(&area).first().map(|f| near(f.rect.height, 3.0 * row_h + frame)) == Some(true),
    );
    r.check("a textarea's contents are not page text", !page_text(&area).contains("hello"));
    r.check(
        "a textarea's contents are its value",
        area.forms.iter().any(|c| c.name == "t" && c.value == "hello"),
    );

    // A control the page has made block-level is still one box, not a box plus
    // whatever was laid out from its children.
    let block = super::render(
        "<body><style>textarea{display:block}</style>\
         <form><textarea name=t>typed</textarea></form></body>",
        VIEWPORT,
        METRICS,
    );
    r.check("a block-level control gets one box", fields(&block).len() == 1);
    r.check("and nothing else", !page_text(&block).contains("typed"));

    // A field joins the line it was written on.
    let inline = super::render(
        "<body><form><p>Find <input name=q size=4> now</p></form></body>",
        VIEWPORT,
        METRICS,
    );
    r.check(
        "a field follows the words before it",
        fields(&inline).first().map(|f| f.rect.x > 8.0) == Some(true),
    );
    r.check("words after a field are kept", page_text(&inline).contains("now"));

    // Nonsense in the attributes must produce a page rather than a panic.
    let hostile = super::render(
        "<body><form><input size=0><input size=-4><input size=abc>\
         <textarea rows=99999 cols=99999></textarea>\
         <input type=file><input type=></form></body>",
        VIEWPORT,
        METRICS,
    );
    r.check(
        "nonsense controls survive",
        hostile.height() >= 0.0 && fields(&hostile).len() == 6,
    );
    r.check(
        "an outsized control is bounded by the page",
        fields(&hostile).iter().all(|f| f.rect.width <= VIEWPORT.width + 0.5),
    );

    // Reading and writing `input.value` from script, which is how a page fills
    // its own form in.
    let mut scripted = session(
        "<body><form><input id=q name=q value=cat></form>\
         <script>document.getElementById('q').value = 'dog'</script></body>",
    );
    r.check(
        "script writes a field's value",
        scripted.page.forms.iter().any(|c| c.name == "q" && c.value == "dog"),
    );
    r.check(
        "script reads it back",
        scripted.eval("document.getElementById('q').value").as_deref() == Ok("dog"),
    );
}

fn full_page(r: &mut Report) {
    let document = "<html><head><title>Test Page</title>\
                    <style>h1{color:#ff8800}</style></head>\
                    <body><h1>Heading</h1><p>Hello &amp; welcome to \
                    <a href=\"/next\">the next page</a>.</p>\
                    <script>alert('no')</script>\
                    <ul><li>First</li><li>Second</li></ul></body></html>";

    let page = super::render(document, VIEWPORT, METRICS);

    r.check("page title", page.title == "Test Page");
    r.check(
        "link target recorded",
        page.link_targets.first().map(|s| s.as_str()) == Some("/next"),
    );
    r.check(
        "link region recorded",
        page.display.hits.iter().any(|h| h.target.is_some()),
    );

    let text = page_text(&page);
    r.check("heading kept", text.contains("Heading"));
    r.check("entity decoded", text.contains("Hello & welcome"));
    r.check("list items kept", text.contains("First") && text.contains("Second"));
    r.check("list marker added", text.contains("* First"));
    // Script, style and title text must never reach the page.
    r.check("script stripped", !text.contains("alert"));
    r.check("style stripped", !text.contains("ff8800"));
    r.check("title not in body", !text.contains("Test Page"));

    // The author sheet coloured the heading, and it is drawn bold by the
    // user-agent sheet.
    let heading = find_text(&page, "Heading");
    r.check(
        "author colour applied",
        heading.map(|(c, _, _)| c) == Some(Color::hex(0xFF8800)),
    );
    r.check("heading is bold", heading.map(|(_, bold, _)| bold) == Some(true));

    // Links are underlined without the page asking.
    let link = find_text(&page, "the next page");
    r.check("link underlined", link.map(|(_, _, u)| u) == Some(true));

    r.check("page has height", page.height() > 0.0);

    // Hit-testing must land on the link's own box and nowhere else.
    let hit = page
        .display
        .hits
        .iter()
        .find(|h| h.target.is_some())
        .map(|h| (h.rect.x + 1.0, h.rect.y + 1.0));
    r.check(
        "link hit-test",
        hit.and_then(|(x, y)| page.link_at(x, y)) == Some("/next"),
    );
    r.check("miss returns nothing", page.link_at(9000.0, 9000.0).is_none());

    // A document that is nothing but junk must still produce a page.
    let junk = super::render("<<<>>&&; <p", VIEWPORT, METRICS);
    r.check("malformed input survives", junk.height() >= 0.0);
}

fn urls(r: &mut Report) {
    let base = "http://example.com/a/b/page.html";
    r.check(
        "absolute href",
        super::resolve_url(base, "http://other.org/x") == "http://other.org/x",
    );
    r.check(
        "root-relative href",
        super::resolve_url(base, "/top") == "http://example.com/top",
    );
    r.check(
        "relative href",
        super::resolve_url(base, "next.html") == "http://example.com/a/b/next.html",
    );
    r.check(
        "parent-relative href",
        super::resolve_url(base, "../up.html") == "http://example.com/a/up.html",
    );
    r.check(
        "protocol-relative href",
        super::resolve_url(base, "//cdn.example.com/x") == "http://cdn.example.com/x",
    );
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn collect_tag<'a>(node: &'a style::StyledNode<'a>, tag: &str) -> Vec<&'a style::StyledNode<'a>> {
    let mut out = Vec::new();
    collect_tag_inner(node, tag, &mut out, 0);
    out
}

fn collect_tag_inner<'a>(
    node: &'a style::StyledNode<'a>,
    tag: &str,
    out: &mut Vec<&'a style::StyledNode<'a>>,
    depth: usize,
) {
    if depth >= dom::MAX_DEPTH {
        return;
    }
    if node.node.tag().eq_ignore_ascii_case(tag) {
        out.push(node);
    }
    for child in &node.children {
        collect_tag_inner(child, tag, out, depth + 1);
    }
}

/// Distinct vertical positions among the fragments, which is the number of
/// lines the text was broken into.
fn count_lines(root: &layout::LayoutBox) -> usize {
    match &root.kind {
        layout::BoxKind::Inline(inline) => {
            let mut tops: Vec<f32> = Vec::new();
            for frag in &inline.fragments {
                if !tops.iter().any(|t| (t - frag.y).abs() < 0.5) {
                    tops.push(frag.y);
                }
            }
            tops.len()
        }
        layout::BoxKind::Block(_) => root.children.iter().map(count_lines).sum(),
    }
}

fn fragments_fit(root: &layout::LayoutBox, width: f32) -> bool {
    match &root.kind {
        layout::BoxKind::Inline(inline) => inline
            .fragments
            .iter()
            .all(|f| f.x + f.width <= width + 0.5),
        layout::BoxKind::Block(_) => root.children.iter().all(|c| fragments_fit(c, width)),
    }
}

/// Fragment text in layout order, with a newline between lines.
fn collected_text(root: &layout::LayoutBox) -> String {
    let mut out = String::new();
    let mut last_y: Option<f32> = None;
    collected_text_inner(root, &mut out, &mut last_y);
    out
}

fn collected_text_inner(root: &layout::LayoutBox, out: &mut String, last_y: &mut Option<f32>) {
    match &root.kind {
        layout::BoxKind::Inline(inline) => {
            for frag in &inline.fragments {
                if last_y.map(|y| (y - frag.y).abs() > 0.5).unwrap_or(false) {
                    out.push('\n');
                }
                out.push_str(&frag.text);
                *last_y = Some(frag.y);
            }
        }
        layout::BoxKind::Block(_) => {
            for child in &root.children {
                collected_text_inner(child, out, last_y);
            }
        }
    }
}

fn first_fragment_x(root: &layout::LayoutBox) -> Option<f32> {
    match &root.kind {
        layout::BoxKind::Inline(inline) => inline.fragments.first().map(|f| f.x),
        layout::BoxKind::Block(_) => root.children.iter().find_map(first_fragment_x),
    }
}

fn subject_tag(sheet: &css::Stylesheet, index: usize) -> Option<String> {
    sheet
        .rules
        .get(index)?
        .selectors
        .first()?
        .subject()?
        .tag
        .clone()
}

fn find_block(root: &layout::LayoutBox, tag: &str) -> Option<layout::Dimensions> {
    if let layout::BoxKind::Block(node) = root.kind {
        if node.node.tag().eq_ignore_ascii_case(tag) {
            return Some(root.dimensions);
        }
    }
    root.children.iter().find_map(|c| find_block(c, tag))
}

/// Every box laid out for `tag`, in document order.
fn blocks(root: &layout::LayoutBox, tag: &str) -> Vec<layout::Dimensions> {
    let mut out = Vec::new();
    blocks_inner(root, tag, &mut out);
    out
}

fn blocks_inner(root: &layout::LayoutBox, tag: &str, out: &mut Vec<layout::Dimensions>) {
    if let layout::BoxKind::Block(node) = root.kind {
        if node.node.tag().eq_ignore_ascii_case(tag) {
            out.push(root.dimensions);
        }
    }
    for child in &root.children {
        blocks_inner(child, tag, out);
    }
}

fn block_tops(root: &layout::LayoutBox, tag: &str) -> Vec<f32> {
    blocks(root, tag).iter().map(|d| d.content.y).collect()
}

fn block_lefts(root: &layout::LayoutBox, tag: &str) -> Vec<f32> {
    blocks(root, tag).iter().map(|d| d.content.x).collect()
}

fn block_widths(root: &layout::LayoutBox, tag: &str) -> Vec<f32> {
    blocks(root, tag).iter().map(|d| d.content.width).collect()
}

/// A page with its scripts already run, ready to be poked at.
fn session(html: &str) -> super::script::Session {
    let mut session =
        super::script::Session::new(super::render(html, VIEWPORT, METRICS), "http://example.test/");
    session.run_scripts();
    session
}

/// The element behind the first painted fragment containing `needle`.
fn node_with_text(page: &super::Page, needle: &str) -> Option<dom::NodeId> {
    let target = page.display.commands.iter().find_map(|c| match c {
        DisplayCommand::Text { text, x, y, .. } if text.contains(needle) => Some((*x, *y)),
        _ => None,
    })?;
    page.hit(target.0 + 1.0, target.1 + 1.0).map(|h| h.node)
}

/// Everything the page would draw, one fragment per line.
fn page_text(page: &super::Page) -> String {
    let mut out = String::new();
    for command in &page.display.commands {
        if let DisplayCommand::Text { text, .. } = command {
            out.push_str(text);
            out.push('\n');
        }
    }
    out
}

// ── Picture helpers ─────────────────────────────────────────────────────────

/// A page whose pictures have not arrived, which is every page on its first
/// layout.
fn no_images() -> ImageStore {
    ImageStore::new()
}

/// A picture of a known size and no interesting content.
///
/// Layout only ever consults the dimensions, so nothing is decoded here and the
/// sizes stay small enough not to matter to the heap.
fn picture(width: usize, height: usize) -> Arc<Image> {
    Arc::new(Image {
        width,
        height,
        pixels: alloc::vec![Color::rgb(1, 2, 3); width * height],
    })
}

/// One picture as it was laid out, copied out of the box tree so the tree it
/// came from can be dropped.
struct Placed {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    src: String,
    /// True when the picture itself is drawn rather than a frame around its alt
    /// text.
    ready: bool,
    alt: String,
}

impl Placed {
    fn is(&self, width: f32, height: f32) -> bool {
        near(self.width, width) && near(self.height, height)
    }
}

/// Percentages and aspect ratios do not land on exact pixels, so sizes are
/// compared with a tolerance, as the length checks above are.
fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.5
}

/// The width layout actually measures text with.
///
/// [`METRICS`] carries the default line box, not the font: a picture is spaced
/// from its neighbours by one character of the face it inherited, so the checks
/// that care have to ask the face.
fn glyph_w() -> f32 {
    crate::framebuffer::TextSize::Normal.char_w() as f32
}

/// Lay out a document and report the pictures in it, in page coordinates.
///
/// The user-agent side is cut down to the blocks these documents use and says
/// nothing at all about `img`, so what comes out is the engine's own default
/// treatment of a picture rather than a rule written for the occasion.
fn picture_layout(html: &str, css: &str, width: f32, images: &ImageStore) -> Vec<Placed> {
    let document = htmlparse::parse(html);
    let styled = style::build(
        &document,
        &sheet("body, p, div { display: block }"),
        &sheet(css),
        VIEWPORT,
    );
    let (tree, _) = layout::layout_document(&styled, width, METRICS, images);
    let mut out = Vec::new();
    collect_placed(&tree, &mut out);
    out
}

fn collect_placed(root: &layout::LayoutBox, out: &mut Vec<Placed>) {
    match &root.kind {
        layout::BoxKind::Inline(inline) => {
            let origin = root.dimensions.content;
            for frag in &inline.fragments {
                let Some(image) = &frag.image else { continue };
                out.push(Placed {
                    x: origin.x + frag.x,
                    y: origin.y + frag.y,
                    width: frag.width,
                    height: frag.height,
                    src: image.src.clone(),
                    ready: image.ready,
                    alt: frag.text.clone(),
                });
            }
        }
        layout::BoxKind::Block(_) => {
            for child in &root.children {
                collect_placed(child, out);
            }
        }
    }
}

/// A page laid out twice, which is what the window layer does: once to find out
/// which pictures it needs, and again once it has them.
fn page_with(html: &str, ready: &[(&str, usize, usize)], failed: &[&str]) -> super::Page {
    let mut page = super::render(html, VIEWPORT, METRICS);
    for (src, width, height) in ready {
        page.images.insert(src, picture(*width, *height));
    }
    for src in failed {
        page.images.fail(src);
    }
    page.relayout();
    page
}

/// The pictures a page would blit, with the boxes they go in.
fn painted(page: &super::Page) -> Vec<(String, layout::Rect)> {
    page.display
        .commands
        .iter()
        .filter_map(|command| match command {
            DisplayCommand::Image { rect, src } => Some((src.clone(), *rect)),
            _ => None,
        })
        .collect()
}

/// The boxes a page would draw for its form controls, in document order.
fn fields(page: &super::Page) -> Vec<super::FieldBox> {
    page.display
        .commands
        .iter()
        .filter_map(|command| match command {
            DisplayCommand::Field(field) => Some(*field),
            _ => None,
        })
        .collect()
}

/// Colour, weight and underline of the first fragment containing `needle`.
fn find_text(page: &super::Page, needle: &str) -> Option<(Color, bool, bool)> {
    page.display.commands.iter().find_map(|c| match c {
        DisplayCommand::Text { text, color, bold, underline, .. } if text.contains(needle) => {
            Some((*color, *bold, *underline))
        }
        _ => None,
    })
}
