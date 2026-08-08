//! Boot-time checks for the DOM binding.
//!
//! [`crate::quickjs::selftest`] proves the engine; this proves the bridge. Every
//! check here runs a real page through a real engine and then looks at the
//! rendered result rather than at the value the script returned, because the
//! thing that matters is whether a script changing the document changes what a
//! reader sees.
//!
//! The three at the end are the ones that decide whether a page can be trusted
//! with the machine: a script that throws, a script that never returns, and a
//! script that recurses until the stack is gone. All three have to leave the
//! browser usable.

use alloc::string::String;

use crate::selftest::Report;

use super::super::{DisplayCommand, Metrics, Page, Viewport};
use super::{NodeId, Session, NO_NODE};

/// Eight-pixel characters over sixteen-pixel rows, matching the GUI font.
const METRICS: Metrics = Metrics { char_w: 8.0, line_h: 16.0 };
const VIEWPORT: Viewport = Viewport { width: 480.0, height: 320.0, char_w: 8.0, line_h: 16.0 };

const PAGE_URL: &str = "http://example.test/dir/page.html?q=1#frag";
/// The origin [`PAGE_URL`] is on, which is what `localStorage` is keyed by.
const TEST_ORIGIN: &str = "http://example.test";

/// Numbers worth reporting that no single check can express.
struct Findings {
    micros: u64,
    engine_micros: u64,
    prelude_bytes: usize,
    natives: usize,
    interrupt_micros: u64,
    overflow: String,
}

pub fn run() -> Report {
    let started = crate::clock::micros();
    let mut report = Report::new();
    let mut found = Findings {
        micros: 0,
        engine_micros: 0,
        prelude_bytes: super::DOM_PRELUDE.len(),
        natives: 0,
        interrupt_micros: 0,
        overflow: String::new(),
    };

    // What one page costs to set up, which is the price of every navigation.
    let before = crate::clock::micros();
    let mut blank = session("<body><p>hello</p></body>");
    found.engine_micros = crate::clock::micros().saturating_sub(before);
    found.natives = crate::quickjs::registered_natives();
    report.check("a page gets an engine", blank.eval("1 + 1").as_deref() == Ok("2"));
    report.check("the bindings are installed", found.natives > 40);
    report.check(
        "the prelude built a document",
        blank.eval("typeof document + typeof window.location").as_deref() == Ok("objectobject"),
    );
    report.check(
        "a page cannot reach the natives",
        blank.eval("typeof __h_get_attr").as_deref() == Ok("undefined"),
    );
    drop(blank);

    querying(&mut report);
    mutation(&mut report);
    events(&mut report);
    timers(&mut report);
    window_object(&mut report);
    storage(&mut report);
    containment(&mut report, &mut found);

    found.micros = crate::clock::micros().saturating_sub(started);
    announce(&found, &report);
    report
}

fn announce(found: &Findings, report: &Report) {
    crate::serial_println!(
        "script bindings: {} checks in {}.{:03} s",
        report.passed + report.failed,
        found.micros / 1_000_000,
        (found.micros / 1_000) % 1_000,
    );
    crate::serial_println!(
        "script bindings: a page costs {} ms to give an engine to — {} natives and {} bytes \
         of prelude",
        found.engine_micros / 1_000,
        found.natives,
        found.prelude_bytes,
    );
    crate::serial_println!(
        "script bindings: a runaway loop was stopped after {} ms against a {} ms budget; \
         runaway recursion gave \"{}\"",
        found.interrupt_micros / 1_000,
        super::SCRIPT_BUDGET_MICROS / 1_000,
        found.overflow,
    );
}

/// A loaded page with its scripts run, at a fixed address so `location` has
/// something to report.
fn session(html: &str) -> Session {
    let page = super::super::render(html, VIEWPORT, METRICS);
    let mut session = Session::new(page, PAGE_URL);
    session.run_scripts();
    session
}

/// Everything the page would draw, as one string. What a reader would see.
fn text_of(page: &Page) -> String {
    let mut out = String::new();
    for command in &page.display.commands {
        if let DisplayCommand::Text { text, .. } = command {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(text);
        }
    }
    out
}

/// Evaluate and compare, which most of these are.
fn eq(report: &mut Report, session: &mut Session, name: &'static str, source: &str, want: &str) {
    match session.eval(source) {
        Ok(got) => {
            if got != want {
                crate::serial_println!(
                    "script bindings: {} — wanted {:?}, got {:?}",
                    name, want, got
                );
            }
            report.check(name, got == want);
        }
        Err(why) => {
            crate::serial_println!("script bindings: {} — failed: {}", name, why);
            report.check(name, false);
        }
    }
}

// ── reading the document ────────────────────────────────────────────────────

fn querying(report: &mut Report) {
    let mut page = session(
        "<body><div id='box' class='panel wide' data-role='main'>\
           <p class='line'>first</p><p class='line'>second</p>\
           <a href='/next'>go</a>\
         </div><input id='field' value='typed'></body>",
    );

    eq(report, &mut page, "getElementById", "document.getElementById('box').tagName", "DIV");
    eq(report, &mut page, "missing element is null", "String(document.getElementById('ghost'))", "null");
    eq(report, &mut page, "querySelector by class", "document.querySelector('.line').textContent", "first");
    eq(report, &mut page, "querySelectorAll counts", "document.querySelectorAll('p.line').length", "2");
    eq(
        report,
        &mut page,
        "querySelectorAll is a real array",
        "document.querySelectorAll('.line').map(p => p.textContent).join('|')",
        "first|second",
    );
    eq(
        report,
        &mut page,
        "a descendant selector needs the ancestor",
        "document.querySelectorAll('#box a').length + ',' + document.querySelectorAll('#nope a').length",
        "1,0",
    );
    eq(report, &mut page, "getElementsByTagName", "document.getElementsByTagName('p').length", "2");
    eq(report, &mut page, "getElementsByClassName", "document.getElementsByClassName('line').length", "2");
    eq(
        report,
        &mut page,
        "an element does not match its own querySelector",
        "String(document.getElementById('box').querySelector('div'))",
        "null",
    );

    // The wrapper cache: the same element twice has to be the same object, which
    // the previous engine could not do and which pages test.
    eq(
        report,
        &mut page,
        "two references to one element are equal",
        "document.getElementById('box') === document.querySelector('#box')",
        "true",
    );

    eq(report, &mut page, "className", "document.getElementById('box').className", "panel wide");
    eq(report, &mut page, "classList reads", "document.getElementById('box').classList.contains('wide')", "true");
    eq(report, &mut page, "classList length", "document.getElementById('box').classList.length", "2");
    eq(report, &mut page, "dataset reads", "document.getElementById('box').dataset.role", "main");
    eq(
        report,
        &mut page,
        "getAttribute is null when absent",
        "String(document.getElementById('box').getAttribute('nope'))",
        "null",
    );
    eq(report, &mut page, "hasAttribute", "document.getElementById('box').hasAttribute('data-role')", "true");
    eq(report, &mut page, "a form control's value", "document.getElementById('field').value", "typed");

    eq(
        report,
        &mut page,
        "the tree is navigable",
        "var b = document.getElementById('box');\
         [b.children.length, b.firstElementChild.textContent, b.children[0].nextElementSibling.textContent,\
          b.children[1].parentNode.id].join('|')",
        "3|first|second|box",
    );
    eq(
        report,
        &mut page,
        "childNodes sees text and children does not",
        "var p = document.querySelector('.line');\
         p.childNodes.length + ',' + p.children.length + ',' + p.childNodes[0].nodeType",
        "1,0,3",
    );
    eq(report, &mut page, "innerHTML reads back", "document.querySelector('.line').innerHTML", "first");
    eq(
        report,
        &mut page,
        "outerHTML includes the element",
        "document.querySelector('.line').outerHTML",
        "<p class=\"line\">first</p>",
    );
    eq(report, &mut page, "matches", "document.querySelector('a').matches('#box a')", "true");
    eq(report, &mut page, "closest", "document.querySelector('a').closest('div').id", "box");

    // Geometry, from the layout tree the page was actually laid out into.
    eq(
        report,
        &mut page,
        "getBoundingClientRect has a real width",
        "document.getElementById('box').getBoundingClientRect().width > 100",
        "true",
    );
    eq(
        report,
        &mut page,
        "the second paragraph is below the first",
        "var ps = document.querySelectorAll('.line');\
         ps[1].getBoundingClientRect().top > ps[0].getBoundingClientRect().top",
        "true",
    );
    eq(
        report,
        &mut page,
        "an element with no box is all zeroes",
        "var d = document.createElement('div');\
         var r = d.getBoundingClientRect(); [r.x, r.y, r.width, r.height].join(',')",
        "0,0,0,0",
    );
}

// ── changing it ─────────────────────────────────────────────────────────────

fn mutation(report: &mut Report) {
    // A script that runs at load and rewrites the page before it is ever shown.
    let loaded = session(
        "<body><p id='out'>before</p>\
         <script>document.getElementById('out').textContent = 'after'</script></body>",
    );
    report.check("a load-time script ran", text_of(&loaded.page).contains("after"));
    report.check("and the old text is gone", !text_of(&loaded.page).contains("before"));

    let mut page = session("<body><ul id='list'></ul><p id='p' style='margin: 0'>x</p></body>");

    eq(
        report,
        &mut page,
        "createElement and appendChild",
        "var li = document.createElement('li'); li.textContent = 'added';\
         document.getElementById('list').appendChild(li);\
         document.getElementById('list').children.length",
        "1",
    );
    report.check("the appended node renders", text_of(&page.page).contains("added"));

    eq(
        report,
        &mut page,
        "a fresh element has no parent",
        "String(document.createElement('span').parentNode)",
        "null",
    );
    // The previous engine parked created nodes in <head>, where getElementById
    // could find them before they were attached. This is the check that they are
    // genuinely outside the document.
    eq(
        report,
        &mut page,
        "a detached element is not in the document",
        "var d = document.createElement('div'); d.id = 'detached';\
         String(document.getElementById('detached')) + ',' + d.isConnected",
        "null,false",
    );
    eq(
        report,
        &mut page,
        "a removed node can be put back",
        "var li = document.querySelector('li'); var list = document.getElementById('list');\
         var taken = list.removeChild(li);\
         var gone = list.children.length; list.appendChild(taken);\
         gone + ',' + list.children.length + ',' + (taken === li)",
        "0,1,true",
    );
    eq(
        report,
        &mut page,
        "insertBefore puts it in front",
        "var list = document.getElementById('list');\
         var first = document.createElement('li'); first.textContent = 'top';\
         list.insertBefore(first, list.firstChild);\
         list.children.map(c => c.textContent).join(',')",
        "top,added",
    );
    eq(
        report,
        &mut page,
        "replaceChild swaps one for the other",
        "var list = document.getElementById('list');\
         var fresh = document.createElement('li'); fresh.textContent = 'new';\
         list.replaceChild(fresh, list.firstChild);\
         list.children.map(c => c.textContent).join(',')",
        "new,added",
    );
    eq(
        report,
        &mut page,
        "cloneNode deep copies",
        "var list = document.getElementById('list');\
         var copy = list.cloneNode(true);\
         copy.children.length + ',' + (copy === list) + ',' + copy.children[0].textContent",
        "2,false,new",
    );
    eq(
        report,
        &mut page,
        "remove takes it out of the tree",
        "var list = document.getElementById('list');\
         list.firstChild.remove(); list.children.length",
        "1",
    );

    // A fragment has to put its children in and not itself: a page that builds
    // its rows in one and appends it to a table would otherwise end up with a
    // `<div>` between the table and its rows, and the table would come apart.
    eq(
        report,
        &mut page,
        "a document fragment moves its children in",
        "var list = document.getElementById('list');\
         var frag = document.createDocumentFragment();\
         for (var i = 0; i < 3; i++) { var li = document.createElement('li');\
           li.textContent = 'f' + i; frag.appendChild(li) }\
         var was = list.children.length;\
         list.appendChild(frag);\
         (list.children.length - was) + ',' + list.children.map(c => c.tagName).join('')",
        "3,LILILILI",
    );

    eq(
        report,
        &mut page,
        "innerHTML writes reparse",
        "var list = document.getElementById('list');\
         list.innerHTML = '<li class=\"x\">one</li><li>two</li>';\
         list.children.length + ',' + document.querySelectorAll('#list .x').length",
        "2,1",
    );
    report.check("reparsed markup renders", text_of(&page.page).contains("one"));

    // Attributes, classes and styles all land in the document, which is what
    // makes the cascade pick them up on the next layout.
    eq(
        report,
        &mut page,
        "setAttribute then read",
        "var e = document.getElementById('p'); e.setAttribute('data-x', '7'); e.getAttribute('data-x')",
        "7",
    );
    eq(
        report,
        &mut page,
        "removeAttribute",
        "var e = document.getElementById('p'); e.removeAttribute('data-x'); String(e.getAttribute('data-x'))",
        "null",
    );
    eq(
        report,
        &mut page,
        "classList add, toggle and remove",
        "var e = document.getElementById('p');\
         e.classList.add('lit'); var a = e.className;\
         e.classList.toggle('lit'); var b = e.className;\
         e.classList.toggle('lit'); [a, b, e.className].join('|')",
        "lit||lit",
    );
    eq(
        report,
        &mut page,
        "dataset writes an attribute",
        "var e = document.getElementById('p'); e.dataset.userName = 'ada';\
         e.getAttribute('data-user-name')",
        "ada",
    );
    eq(
        report,
        &mut page,
        "style keeps the declarations already there",
        "var e = document.getElementById('p'); e.style.color = 'rgb(255, 0, 0)';\
         e.style.margin + '|' + e.style.color",
        "0|rgb(255, 0, 0)",
    );
    eq(
        report,
        &mut page,
        "camelCase becomes a css name",
        "var e = document.getElementById('p'); e.style.backgroundColor = 'red';\
         e.getAttribute('style').indexOf('background-color') >= 0",
        "true",
    );
    // The point of writing to `style` at all: it has to reach the painter.
    let red = page.page.display.commands.iter().any(|command| match command {
        DisplayCommand::Text { text, color, .. } => {
            text.contains('x') && *color == crate::color::Color::rgb(255, 0, 0)
        }
        _ => false,
    });
    report.check("an inline style reaches the painter", red);

    // document.write, which for a load-time script means "put this where the
    // parser would have been" — after the script element. Two writes have to come
    // out in the order they were made, and after the markup that preceded them.
    let written = session(
        "<body><p>before</p>\
         <script>document.write('<p>one</p>'); document.write('<p>two</p>')</script>\
         <p>after</p></body>",
    );
    report.check(
        "document.write lands where the parser was",
        text_of(&written.page) == "before one two after",
    );
    // And outside a load-time script it must do nothing rather than replace the
    // page, which is what a real browser would do and what nobody wants.
    let mut late = session("<body><p>kept</p></body>");
    let _ = late.eval("document.write('<p>lost</p>')");
    report.check("a later write is refused", text_of(&late.page) == "kept");
    report.check(
        "and says so",
        late.log.iter().any(|line| line.contains("document.write outside")),
    );

    // Batching. A hundred appends is one layout, which is the whole reason the
    // dirty flag exists.
    let mut batched = session("<body><ul id='big'></ul></body>");
    let before = layouts_of(&batched);
    eq(
        report,
        &mut batched,
        "a hundred appends in a loop",
        "var list = document.getElementById('big');\
         for (var i = 0; i < 100; i++) { var li = document.createElement('li');\
           li.textContent = 'row ' + i; list.appendChild(li) }\
         list.children.length",
        "100",
    );
    report.check("all hundred rows rendered", text_of(&batched.page).contains("row 99"));
    report.check("and they cost one layout, not a hundred", layouts_of(&batched) == before + 1);
}

/// How many times this page has been laid out, counted from the display list's
/// own generation rather than from a hook — see [`Page::layouts`].
fn layouts_of(session: &Session) -> usize {
    session.page.layouts()
}

// ── events ──────────────────────────────────────────────────────────────────

fn events(report: &mut Report) {
    // Both phases, in order, on a three-deep tree.
    let mut phases = session(
        "<body><div id='outer'><div id='inner'><button id='b'>Press</button></div></div>\
         <script>\
           window.trail = '';\
           function note(name) { return function (e) { trail += name + e.eventPhase + ' ' } }\
           document.getElementById('outer').addEventListener('click', note('outerC'), true);\
           document.getElementById('inner').addEventListener('click', note('innerC'), true);\
           document.getElementById('b').addEventListener('click', note('targetC'), true);\
           document.getElementById('b').addEventListener('click', note('target'));\
           document.getElementById('inner').addEventListener('click', note('innerB'));\
           document.getElementById('outer').addEventListener('click', note('outerB'));\
           document.addEventListener('click', note('doc'));\
         </script></body>",
    );
    let button = node_with_text(&phases.page, "Press");
    report.check("the button is hit-testable", button.is_some());
    if let Some(id) = button {
        let outcome = phases.dispatch_click(id);
        report.check("the click was handled", outcome.handled);
        report.check("and nothing cancelled it", outcome.allows_default());
        eq(
            report,
            &mut phases,
            "capture ran outside in, then bubble inside out",
            "trail.trim()",
            "outerC1 innerC1 targetC2 target2 innerB3 outerB3 doc3",
        );
    }

    // preventDefault is what decides whether the browser follows the link, which
    // is the behaviour the previous engine could not express at all.
    let mut cancelled = session(
        "<body><a id='a' href='/elsewhere'>link</a>\
         <script>document.getElementById('a').addEventListener('click', function (e) {\
             e.preventDefault(); window.saw = e.type + ':' + e.target.id + ':' + (e.clientX >= 0);\
           });</script></body>",
    );
    if let Some(id) = node_with_text(&cancelled.page, "link") {
        let outcome = cancelled.dispatch_click_at(id, 12.0, 34.0);
        report.check("a cancelled click is reported as cancelled", !outcome.allows_default());
        eq(report, &mut cancelled, "the event carried its own details", "saw", "click:a:true");
    }

    // A listener that does nothing must leave the link alone, which is the case
    // the old engine got backwards: any handler running consumed the click.
    let mut passive = session(
        "<body><a id='a' href='/elsewhere'>link</a>\
         <script>document.addEventListener('click', function () { window.counted = 1 });</script></body>",
    );
    if let Some(id) = node_with_text(&passive.page, "link") {
        let outcome = passive.dispatch_click(id);
        report.check("a passive document listener does not eat the click", outcome.allows_default());
        eq(report, &mut passive, "but it did run", "counted", "1");
    }

    // stopPropagation, once, and removeEventListener.
    let mut control = session(
        "<body><div id='d'><span id='s'>x</span></div>\
         <script>\
           window.log = '';\
           var once = function () { log += 'once' };\
           var gone = function () { log += 'gone' };\
           document.getElementById('s').addEventListener('click', once, { once: true });\
           document.getElementById('s').addEventListener('click', gone);\
           document.getElementById('s').removeEventListener('click', gone);\
           document.getElementById('s').addEventListener('click', function (e) { e.stopPropagation() });\
           document.getElementById('d').addEventListener('click', function () { log += 'parent' });\
         </script></body>",
    );
    if let Some(id) = node_with_text(&control.page, "x") {
        control.dispatch_click(id);
        control.dispatch_click(id);
        eq(report, &mut control, "once fires once, removed never, stopped does not bubble", "log", "once");
    }

    // The property form and the attribute form, both of which are script.
    let mut inline = session(
        "<body><p id='o'>no</p>\
         <button id='attr' onclick=\"document.getElementById('o').textContent = 'yes'\">a</button>\
         <button id='prop'>b</button>\
         <script>document.getElementById('prop').onclick = function () {\
             document.getElementById('o').textContent = 'prop' };</script></body>",
    );
    if let Some(id) = node_with_text(&inline.page, "a") {
        inline.dispatch_click(id);
        report.check("an onclick attribute ran", text_of(&inline.page).contains("yes"));
    }
    if let Some(id) = node_with_text(&inline.page, "b") {
        inline.dispatch_click(id);
        report.check("an onclick property ran", text_of(&inline.page).contains("prop"));
    }

    // A handler that rewrites the page has to cause a relayout.
    let mut live = session(
        "<body><p id='t'>zero</p><button id='go'>Go</button>\
         <script>var n = 0;\
           document.getElementById('go').addEventListener('click', function () {\
             n += 1; document.getElementById('t').textContent = 'count ' + n });\
         </script></body>",
    );
    if let Some(id) = node_with_text(&live.page, "Go") {
        live.dispatch_click(id);
        report.check("the document change was painted", text_of(&live.page).contains("count 1"));
        live.dispatch_click(id);
        report.check("and again on the second click", text_of(&live.page).contains("count 2"));
    }

    // The load events, in order, and readyState alongside them.
    let ordered = session(
        "<body><script>\
           window.order = document.readyState;\
           document.addEventListener('DOMContentLoaded', function () { order += '|dcl:' + document.readyState });\
           window.addEventListener('load', function () { order += '|load:' + document.readyState });\
         </script></body>",
    );
    let mut ordered = ordered;
    eq(
        report,
        &mut ordered,
        "DOMContentLoaded then load, with readyState following",
        "order",
        "loading|dcl:interactive|load:complete",
    );

    // A synthetic event a page makes itself.
    let mut synthetic = session(
        "<body><div id='d'><span id='s'>x</span></div>\
         <script>window.seen = '';\
           document.getElementById('d').addEventListener('tea', function (e) { seen = e.type + ':' + e.detail.cups });\
         </script></body>",
    );
    eq(
        report,
        &mut synthetic,
        "dispatchEvent bubbles a page's own event",
        "document.getElementById('s').dispatchEvent(new CustomEvent('tea', { bubbles: true, detail: { cups: 2 } }));\
         seen",
        "tea:2",
    );
    eq(
        report,
        &mut synthetic,
        "element.click goes through the listeners",
        "window.clicked = 0;\
         document.getElementById('s').addEventListener('click', function () { clicked++ });\
         document.getElementById('s').click(); clicked",
        "1",
    );
}

// ── timers and promises ─────────────────────────────────────────────────────

fn timers(report: &mut Report) {
    // A zero-delay timer set at load time has had its turn by the time the page
    // is first shown.
    let immediate = session(
        "<body><p id='d'>waiting</p>\
         <script>setTimeout(function () {\
           document.getElementById('d').textContent = 'fired' }, 0)</script></body>",
    );
    report.check("a zero-delay timer fired before the page was shown", text_of(&immediate.page).contains("fired"));

    // One with a real delay has not, and must fire once the clock has moved —
    // which is what the browser's idle path is for.
    let mut later = session(
        "<body><p id='d'>waiting</p>\
         <script>setTimeout(function () {\
           document.getElementById('d').textContent = 'late' }, 60)</script></body>",
    );
    report.check("a 60 ms timer has not fired yet", !text_of(&later.page).contains("late"));
    report.check("and the session says it is still waiting", later.has_pending_work());
    spin_for(80_000);
    later.pump();
    report.check("it fires once the clock has moved", text_of(&later.page).contains("late"));
    report.check("and then nothing is waiting", !later.has_pending_work());

    // clearTimeout has to actually stop one.
    let mut cleared = session(
        "<body><p id='d'>waiting</p>\
         <script>var t = setTimeout(function () {\
           document.getElementById('d').textContent = 'should not happen' }, 10);\
           clearTimeout(t)</script></body>",
    );
    spin_for(30_000);
    cleared.pump();
    report.check("a cleared timer never runs", !text_of(&cleared.page).contains("should not"));

    // setInterval, which has to repeat and then be stoppable.
    let mut repeating = session(
        "<body><p id='d'>0</p>\
         <script>window.n = 0;\
           window.handle = setInterval(function () { n++;\
             document.getElementById('d').textContent = String(n);\
             if (n >= 3) clearInterval(handle) }, 5)</script></body>",
    );
    for _ in 0..6 {
        spin_for(10_000);
        repeating.pump();
    }
    eq(report, &mut repeating, "setInterval repeated and then stopped", "n", "3");
    report.check("the repeat reached the page", text_of(&repeating.page).contains('3'));

    // requestAnimationFrame maps onto the same pump, which is the repaint.
    let mut framed = session(
        "<body><p id='d'>still</p>\
         <script>requestAnimationFrame(function (t) {\
           document.getElementById('d').textContent = 'framed ' + (t > 0) })</script></body>",
    );
    framed.pump();
    report.check("requestAnimationFrame ran with a timestamp", text_of(&framed.page).contains("framed true"));

    // Promises. Nothing in QuickJS runs these without the embedder draining the
    // job queue, so this is the check that the drain is wired in.
    let resolved = session(
        "<body><p id='d'>pending</p>\
         <script>Promise.resolve('ready').then(function (v) {\
           document.getElementById('d').textContent = v })</script></body>",
    );
    report.check("a promise chain ran", text_of(&resolved.page).contains("ready"));

    let awaited = session(
        "<body><p id='d'>pending</p>\
         <script>(async function () {\
           var v = await Promise.resolve('awaited');\
           document.getElementById('d').textContent = v })()</script></body>",
    );
    report.check("an async function resumed", text_of(&awaited.page).contains("awaited"));

    // A promise that settles from a timer needs both mechanisms at once.
    let mut both = session(
        "<body><p id='d'>pending</p>\
         <script>new Promise(function (done) { setTimeout(function () { done('both') }, 20) })\
           .then(function (v) { document.getElementById('d').textContent = v })</script></body>",
    );
    spin_for(40_000);
    both.pump();
    // The timer resolves the promise; the job that runs the `then` is queued by
    // that, and drained by the same pump.
    report.check("a promise resolved from a timer continues", text_of(&both.page).contains("both"));
}

/// Burn `micros` of the monotonic clock.
///
/// The timers run on [`crate::clock`], which advances from the CPU's own counter
/// rather than from the timer IRQ — so it moves here even though this is a boot
/// self-test with nothing else running.
fn spin_for(micros: u64) {
    let until = crate::clock::micros().saturating_add(micros);
    while crate::clock::micros() < until {
        core::hint::spin_loop();
    }
}

// ── the window ──────────────────────────────────────────────────────────────

fn window_object(report: &mut Report) {
    let mut page = session("<body><p>x</p><form id='f' action='/go'><input name='q' value='hi'></form></body>");

    eq(report, &mut page, "window is the global", "window === globalThis && self === window", "true");
    eq(report, &mut page, "document.title is writable", "document.title = 'Set'; document.title", "Set");
    eq(
        report,
        &mut page,
        "location is taken apart correctly",
        "[location.protocol, location.hostname, location.pathname, location.search, location.hash].join(' ')",
        "http: example.test /dir/page.html ?q=1 #frag",
    );
    eq(report, &mut page, "the viewport is reported", "window.innerWidth", "480");
    eq(report, &mut page, "navigator names this browser", "navigator.userAgent.indexOf('OS101') >= 0", "true");
    eq(report, &mut page, "document.cookie round-trips", "document.cookie = 'a=1'; document.cookie", "a=1");
    eq(
        report,
        &mut page,
        "getComputedStyle answers from the inline style",
        "var p = document.querySelector('p'); p.style.color = 'blue';\
         getComputedStyle(p).getPropertyValue('color')",
        "blue",
    );

    // console.log has to reach somewhere a reader can see it, with objects
    // formatted rather than turned into [object Object].
    let talking = session(
        "<body><script>console.log('hello', 1, { a: [1, 2], b: 'x' }, null);\
           console.warn('careful'); alert('hi')</script></body>",
    );
    report.check(
        "console.log formats its arguments",
        talking.log.iter().any(|line| line == "hello 1 { a: [1, 2], b: \"x\" } null"),
    );
    report.check(
        "console.warn is marked as a warning",
        talking.log.iter().any(|line| line == "warning: careful"),
    );
    report.check("alert is captured", talking.log.iter().any(|line| line == "hi"));

    // Assigning to location has to be recorded for the browser rather than
    // acted on inside the engine, which would deadlock against the window
    // manager's lock.
    let mut going = session("<body><script>location.href = '/next'</script></body>");
    report.check(
        "assigning to location asks the browser to navigate",
        going.take_pending_navigation().as_deref() == Some("http://example.test/next"),
    );
    let mut relative = session("<body><script>location = 'other.html'</script></body>");
    report.check(
        "and a relative address resolves against the page",
        relative.take_pending_navigation().as_deref() == Some("http://example.test/dir/other.html"),
    );

    // form.submit() goes the same way.
    let mut submitted = session("<body><form id='f' action='/go'><input name='q' value='hi'></form></body>");
    let _ = submitted.eval("document.getElementById('f').submit()");
    report.check(
        "form.submit asks the browser to navigate",
        matches!(submitted.take_pending_navigation(), Some(url) if url.contains("/go?q=hi")),
    );
}

// ── storage ─────────────────────────────────────────────────────────────────

fn storage(report: &mut Report) {
    // Cleared first and last, and only for this test's own origin: the store is a
    // real file on the user's disk, and wiping all of it every boot to keep a
    // self-test tidy would be a fine way to lose their data.
    super::storage::forget(TEST_ORIGIN);

    let mut page = session("<body></body>");
    eq(
        report,
        &mut page,
        "localStorage stores and reads back",
        "localStorage.setItem('k', 'v'); localStorage.getItem('k')",
        "v",
    );
    eq(
        report,
        &mut page,
        "a missing key is null",
        "String(localStorage.getItem('nope'))",
        "null",
    );
    eq(
        report,
        &mut page,
        "property access works too",
        "localStorage.token = 'abc'; localStorage.token + ',' + localStorage.length",
        "abc,2",
    );
    eq(
        report,
        &mut page,
        "removeItem and clear",
        "localStorage.removeItem('k'); var after = localStorage.length;\
         localStorage.clear(); after + ',' + localStorage.length",
        "1,0",
    );

    // The two stores are separate, and localStorage outlives the page.
    let mut first = session("<body></body>");
    let _ = first.eval("localStorage.setItem('kept', 'yes'); sessionStorage.setItem('lost', 'yes')");
    drop(first);
    let mut second = session("<body></body>");
    eq(
        report,
        &mut second,
        "localStorage survives the page and sessionStorage does not",
        "localStorage.getItem('kept') + ',' + String(sessionStorage.getItem('lost'))",
        "yes,null",
    );

    super::storage::forget(TEST_ORIGIN);
}

// ── a page that misbehaves ──────────────────────────────────────────────────

fn containment(report: &mut Report, found: &mut Findings) {
    // A broken script must not stop the page rendering, or the next script.
    let broken = session(
        "<body><p>intact</p><script>this is not javascript(</script>\
         <script>document.querySelector('p').className = 'ran'</script></body>",
    );
    report.check("a page with a broken script still renders", text_of(&broken.page).contains("intact"));
    report.check("the failure was reported", !broken.errors.is_empty());
    report.check(
        "and the next script still ran",
        broken.page.dom.find_tag("p").and_then(|p| p.as_element()).and_then(|e| e.attr("class"))
            == Some("ran"),
    );

    // One that throws at load time, likewise.
    let throwing = session("<body><p>alive</p><script>null.oops</script></body>");
    report.check("a throwing script leaves the page rendered", text_of(&throwing.page).contains("alive"));
    report.check(
        "and says what threw",
        throwing.errors.iter().any(|why| why.contains("TypeError")),
    );

    // A handler that throws must not stop the click, the relayout, or the next
    // handler.
    let mut hostile = session(
        "<body><div id='d'><button id='b'>Press</button></div>\
         <script>\
           document.getElementById('b').addEventListener('click', function () { throw new Error('boom') });\
           document.getElementById('d').addEventListener('click', function () {\
             document.getElementById('d').className = 'survived' });\
         </script></body>",
    );
    if let Some(id) = node_with_text(&hostile.page, "Press") {
        hostile.dispatch_click(id);
        eq(report, &mut hostile, "a throwing handler does not stop the next one", "document.getElementById('d').className", "survived");
        report.check(
            "and the throw was logged",
            hostile.log.iter().any(|line| line.contains("boom")),
        );
    }

    // The interrupt budget: the check that a page cannot wedge the machine, and
    // the measurement that says it is the budget we chose doing it.
    //
    // The loop is wrapped in a `try` on purpose, so that one evaluation proves
    // both halves of the guarantee — that a runaway loop is stopped, and that a
    // page cannot keep it alive by catching the interruption. QuickJS raises it
    // uncatchably, which is what makes the interrupt handler worth more than a
    // step counter. It costs the whole budget in boot time and is worth it.
    let mut runaway = session("<body><p>alive</p></body>");
    let started = crate::clock::micros();
    let outcome = runaway.eval("try { while (true) {} } catch (e) { 'caught it' }");
    found.interrupt_micros = crate::clock::micros().saturating_sub(started);
    report.check("an infinite loop is stopped", outcome.is_err());
    report.check(
        "a try/catch cannot keep it alive",
        !matches!(&outcome, Ok(text) if text.contains("caught")),
    );
    report.check(
        "and says it ran for too long rather than naming an internal error",
        matches!(&outcome, Err(why) if why.contains("too long")),
    );
    // Generously bracketed: the point is that it is the budget doing this and
    // not something else, not that the clock is precise.
    report.check(
        "it was stopped at about the budget",
        found.interrupt_micros >= super::SCRIPT_BUDGET_MICROS / 2
            && found.interrupt_micros < super::SCRIPT_BUDGET_MICROS * 3,
    );
    report.check("the page is usable afterwards", runaway.eval("1 + 1").as_deref() == Ok("2"));
    report.check("and still has its document", text_of(&runaway.page).contains("alive"));

    // Runaway recursion is the other way to lose the machine, and it comes back
    // as an ordinary exception a script can catch.
    let mut deep = session("<body><p>alive</p></body>");
    let overflow = deep.eval(
        "var reached = 0; function down(n) { reached = n; return down(n + 1) }\
         try { down(1) } catch (e) { e.constructor.name + ': ' + e.message } ",
    );
    found.overflow = overflow.clone().unwrap_or_default();
    report.check(
        "runaway recursion is caught, not fatal",
        matches!(&overflow, Ok(text) if text.contains("stack overflow")),
    );
    report.check(
        "and it got a useful depth first",
        matches!(deep.eval("reached"), Ok(text) if text.parse::<u32>().unwrap_or(0) > 100),
    );
    report.check("the page is usable after an overflow", deep.eval("2 * 21").as_deref() == Ok("42"));

    // A timer that reschedules itself for ever must not spin the pump.
    let mut spinning = session(
        "<body><script>window.n = 0;\
           (function again() { n++; setTimeout(again, 0) })()</script></body>",
    );
    let counted = spinning.eval("n").ok().and_then(|text| text.parse::<u32>().ok()).unwrap_or(0);
    report.check("a self-rescheduling timer is bounded per pump", counted > 0 && counted < 1_000);
    report.check("and it is still queued for the next one", spinning.has_pending_work());
}

/// The element behind the first painted fragment containing `needle`, found the
/// way a real click finds it: through the hit list.
fn node_with_text(page: &Page, needle: &str) -> Option<NodeId> {
    let target = page.display.commands.iter().find_map(|command| match command {
        DisplayCommand::Text { text, x, y, .. } if text.contains(needle) => Some((*x, *y)),
        _ => None,
    })?;
    page.hit(target.0 + 1.0, target.1 + 1.0)
        .map(|hit| hit.node)
        .filter(|id| *id != NO_NODE)
}
