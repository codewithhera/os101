//! Running a page's scripts, and binding the document to the engine.
//!
//! The engine is QuickJS ([`crate::quickjs`]). What it can reach outside its own
//! heap is the fifty-odd `__h_*` native functions registered here, each of which
//! takes and returns nothing but numbers, strings and booleans — that is the
//! entire width of the bridge. Everything with shape to it, from the element
//! wrappers to the event objects to the timer queue, is built on the other side
//! by `domjs.js`, which is evaluated into every page before the page's own
//! script runs.
//!
//! # How a native reaches the document
//!
//! A native is a plain `fn(&Args) -> Return` with nowhere to put a receiver, so
//! the session being scripted is published in [`ACTIVE`] for the duration of an
//! evaluation and taken down again afterwards. That is a raw pointer, and it is
//! sound for the same reasons the rest of the browser is: OS101 is uniprocessor,
//! a session is only ever touched from the GUI event loop, and no interrupt
//! handler goes anywhere near it. The one rule the code has to keep is that no
//! Rust reference into the session may be alive while the engine is running,
//! which is why [`Session::run`] moves the engine out into a local first.
//!
//! # Batching
//!
//! A mutation sets a flag; nothing lays the page out until the whole batch — a
//! script, an event dispatch, a round of timers — has finished. A loop appending
//! a hundred nodes therefore costs one layout, not a hundred. The exception is
//! geometry: `getBoundingClientRect` has to answer from a current layout, so it
//! flushes first, which is the same bargain a real browser strikes.
//!
//! # Time
//!
//! Every evaluation is given a deadline (see [`SCRIPT_BUDGET`]), enforced by
//! QuickJS's interrupt handler, and a page's whole load shares
//! [`PAGE_BUDGET`]. `while (true) {}` therefore ends with an error rather than a
//! dead machine, and it cannot be caught and retried: the engine raises the
//! interruption uncatchably.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::quickjs::{Arg, Args, Engine, Return};

use super::css;
use super::dom::{Node, NodeId, NodeKind, ScriptRef, NO_NODE};
use super::htmlparse;
use super::style;
use super::Page;

/// The JavaScript half of the binding.
const DOM_PRELUDE: &str = include_str!("domjs.js");

/// How long one evaluation may run before the engine interrupts it.
///
/// A second and a half is far longer than any honest page spends in a script tag
/// or a click handler — the pages this browser can render at all are measured in
/// single-digit milliseconds — and short enough that a runaway loop reads as a
/// hesitation rather than a hang. It is the same figure for a script tag, an
/// event handler and a timer callback, because a page cannot be trusted to have
/// put its expensive work in the one we were feeling generous about.
pub const SCRIPT_BUDGET_MICROS: u64 = 1_500_000;

/// How long *all* of one page's load-time scripts may take between them.
///
/// Without this a document with twenty script tags could spend twenty times
/// [`SCRIPT_BUDGET_MICROS`] before the first paint. Once it is gone the
/// remaining scripts are skipped and the page is reported as such, which is
/// better than a minute of frozen window.
pub const PAGE_BUDGET_MICROS: u64 = 5_000_000;

/// The stack the page's scripts get. Deliberately the engine's default: the
/// browser creates its engine from `browser_show_document`, which is four frames
/// below the main loop, so the budget the engine anchors there is very nearly all
/// of it.
const STACK_SIZE: usize = crate::quickjs::DEFAULT_STACK_SIZE;

/// How many messages from `console.log` and `alert` to keep.
const MAX_LOG: usize = 64;
/// How many detached subtrees a script may hold at once. A node a script has
/// removed stays alive so it can be put back, and this is where "stays alive"
/// stops.
const MAX_DETACHED: usize = 4_096;
/// How many external `<script src>` a page may pull in. Each is a blocking HTTP
/// request on the thread the GUI runs on, so this is the same kind of ceiling
/// the picture loader has.
pub const MAX_EXTERNAL_SCRIPTS: usize = 4;
/// Longest cookie jar and storage value a page may keep, in bytes.
const MAX_STORE_BYTES: usize = 64 * 1024;

/// The session the running engine is bound to.
///
/// Null whenever no script is running, which is what makes a stray native call
/// answer with its fallback instead of following a dangling pointer.
static ACTIVE: AtomicPtr<Session> = AtomicPtr::new(core::ptr::null_mut());

/// Ids the JavaScript side uses for the two event targets that are not nodes.
/// They are below -1 because -1 already means "no such node".
const JS_NONE: i64 = -1;
const JS_DOCUMENT: i64 = -2;
const JS_WINDOW: i64 = -3;

/// What a dispatch did, which is what tells the browser whether to carry on and
/// follow the link the click landed on.
#[derive(Clone, Copy, Default)]
pub struct Dispatch {
    /// At least one handler ran.
    pub handled: bool,
    /// A handler called `preventDefault`.
    pub prevented: bool,
}

impl Dispatch {
    /// Should the browser do what it was going to do anyway — follow the link,
    /// submit the form, insert the character?
    pub fn allows_default(self) -> bool {
        !self.prevented
    }
}

/// How far through the load a page is, which is what `document.readyState` says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ready {
    Loading,
    Interactive,
    Complete,
}

/// A loaded page together with the engine running against it.
pub struct Session {
    pub page: Page,
    /// Taken out of the session while the engine runs, so that a native can hold
    /// `&mut Session` without aliasing a live borrow. `None` only for the moment
    /// an evaluation is in progress, and for a session whose engine could not be
    /// built at all.
    engine: Option<Engine>,
    /// Messages from `console.*` and `alert`, newest last.
    pub log: Vec<String>,
    /// Everything that went wrong while running the page's scripts.
    pub errors: Vec<String>,
    /// The address this document came from, which is what `location` reports and
    /// what a relative `src` resolves against.
    url: String,
    /// Subtrees a script has made or removed but not attached. They keep their
    /// ids so a script can hold a reference, and they are deliberately not part
    /// of `page.dom` so that `getElementById` cannot find them.
    detached: Vec<Node>,
    /// Set when script has changed the document.
    dirty: bool,
    /// Where a script asked to go, for the browser to pick up once it is out of
    /// the window manager's lock — navigating from inside a native would
    /// deadlock against it.
    pending_navigation: Option<String>,
    /// `document.cookie`, which goes no further than this. Nothing puts it in a
    /// request and nothing writes it to disk.
    cookies: Vec<(String, String)>,
    /// `sessionStorage`, which lasts as long as this page does.
    session_store: Vec<(String, String)>,
    ready: Ready,
    /// Where `document.write` writes: the parser's position, which starts as the
    /// `<script>` element being evaluated and moves along behind what it writes.
    /// [`NO_NODE`] outside a load-time script, where a write means nothing.
    write_anchor: NodeId,
    /// What is left of [`PAGE_BUDGET_MICROS`].
    page_budget: u64,
    /// Whether a timer, an animation callback or a promise job is waiting. Kept
    /// as a flag because the browser's main loop reads it on every pass.
    pending: bool,
}

/// A session holds a QuickJS runtime, which is raw pointers, and the window
/// manager it lives inside is a `static` behind a spinlock that requires `Send`.
///
/// Asserting it is sound here because OS101 is uniprocessor — there is no
/// application-processor bring-up — and a session is only ever reached from the
/// GUI event loop. No interrupt handler touches the window manager. The engine's
/// own rule, that it must be used from the stack it was created on, is met by
/// the same argument.
unsafe impl Send for Session {}

impl Session {
    /// Wrap a page, build an engine for it and install the browser globals.
    ///
    /// `url` is the address the document came from. A session whose engine
    /// cannot be built is still a usable session: the page renders, and every
    /// evaluation reports that there is no engine.
    pub fn new(mut page: Page, url: &str) -> Session {
        ensure_head(&mut page);

        let mut session = Session {
            page,
            engine: None,
            log: Vec::new(),
            errors: Vec::new(),
            url: url.to_string(),
            detached: Vec::new(),
            dirty: false,
            pending_navigation: None,
            cookies: Vec::new(),
            session_store: Vec::new(),
            ready: Ready::Loading,
            write_anchor: NO_NODE,
            page_budget: PAGE_BUDGET_MICROS,
            pending: false,
        };

        match Engine::with_limits(STACK_SIZE, crate::quickjs::DEFAULT_MEMORY_LIMIT) {
            Ok(mut engine) => {
                if let Err(why) = register_natives(&mut engine) {
                    session.errors.push(alloc::format!("script bindings: {}", why));
                }
                session.engine = Some(engine);
                // The prelude is trusted code but it is still code, and a
                // failure here would leave every page script confused about why
                // `document` is missing — so it is reported like any other.
                if let Err(why) = session.eval_bounded(DOM_PRELUDE, "dom.js") {
                    session.errors.push(alloc::format!("the DOM binding failed to load: {}", why));
                }
            }
            Err(why) => session.errors.push(alloc::format!("no script engine: {}", why)),
        }
        session
    }

    /// Publish this session, hand the engine to `f`, and put both back.
    ///
    /// Everything that runs JavaScript goes through here. The engine is *moved*
    /// out of `self` first on purpose: a native reconstructs `&mut Session` from
    /// [`ACTIVE`], and that is only sound while no Rust reference into the
    /// session is alive — which a borrow of `self.engine` across the call would
    /// be.
    fn run<R>(&mut self, fallback: R, f: impl FnOnce(&Engine) -> R) -> R {
        let Some(engine) = self.engine.take() else { return fallback };
        let previous = ACTIVE.swap(self as *mut Session, Ordering::Relaxed);
        let result = f(&engine);
        ACTIVE.store(previous, Ordering::Relaxed);
        self.engine = Some(engine);
        result
    }

    /// Evaluate with a deadline, and drain the job queue so that a `then`
    /// handler has run before returning.
    fn eval_bounded(&mut self, source: &str, name: &str) -> Result<String, String> {
        let (result, rejected) = self.run((Err(no_engine()), None), |engine| {
            engine.set_time_limit(SCRIPT_BUDGET_MICROS);
            let outcome = engine.eval(source, name);
            // Drained even when the script threw: the jobs it queued before
            // failing are still owed their turn, and a page that puts its
            // rendering in a `then` would otherwise never draw.
            let pumped = engine.pump_jobs();
            engine.clear_time_limit();
            (outcome.map_err(|why| explain(&why)), pumped.err())
        });
        if let Some(why) = rejected {
            self.note(alloc::format!("unhandled rejection: {}", explain(&why)));
        }
        self.refresh_pending();
        result
    }

    /// Evaluate a snippet against this document and lay the page out again if it
    /// changed anything. The shell's `js` command and the self-tests use this.
    pub fn eval(&mut self, source: &str) -> Result<String, String> {
        let result = self.eval_bounded(source, "script");
        self.settle();
        result
    }

    /// Run the page's scripts, fire `DOMContentLoaded` and `load`, and let the
    /// timers they set have their first turn.
    ///
    /// `fetch` is asked for the body of a `<script src>`; it is given the address
    /// already resolved against this document's. Returning `None` skips that
    /// script, which is what a caller with no network does.
    ///
    /// One script failing does not stop the next, which is what browsers do and
    /// what makes a page with one broken script still mostly work.
    pub fn run_scripts_with(&mut self, mut fetch: impl FnMut(&str) -> Option<String>) {
        let scripts = core::mem::take(&mut self.page.scripts);
        let mut fetched = 0usize;

        // `defer` says "after the document is parsed", and `async` says "whenever
        // it is ready". The parse finished before any of this ran, and the fetch
        // is synchronous, so neither can be honoured as written — both are held
        // back to after the document-order scripts instead, which preserves the
        // one property pages actually depend on: that a deferred script sees the
        // whole document.
        let mut deferred: Vec<usize> = Vec::new();

        for (index, script) in scripts.iter().enumerate() {
            if script.defer || script.is_async {
                deferred.push(index);
                continue;
            }
            self.run_one(script, &mut fetch, &mut fetched);
        }
        for index in deferred {
            self.run_one(&scripts[index], &mut fetch, &mut fetched);
        }

        self.page.scripts = scripts;

        self.ready = Ready::Interactive;
        self.fire(JS_DOCUMENT, "DOMContentLoaded", "");
        self.ready = Ready::Complete;
        self.fire(JS_WINDOW, "load", "");

        // A `setTimeout(f, 0)` at load time should have run by the time the page
        // is first shown, which is what the previous engine did and what a page
        // that defers its own setup expects.
        self.pump();
        self.settle();
    }

    /// [`Session::run_scripts_with`] for a caller with no network.
    pub fn run_scripts(&mut self) {
        self.run_scripts_with(|_| None);
    }

    fn run_one(
        &mut self,
        script: &ScriptRef,
        fetch: &mut impl FnMut(&str) -> Option<String>,
        fetched: &mut usize,
    ) {
        if self.page_budget == 0 {
            return;
        }

        let (source, name) = match &script.src {
            Some(src) => {
                if *fetched >= MAX_EXTERNAL_SCRIPTS {
                    self.errors.push(alloc::format!(
                        "{} was not fetched: a page may load {} external scripts",
                        src, MAX_EXTERNAL_SCRIPTS
                    ));
                    return;
                }
                *fetched += 1;
                let resolved = super::resolve_url(&self.url, src);
                match fetch(&resolved) {
                    Some(body) => (body, resolved),
                    None => {
                        self.errors
                            .push(alloc::format!("{} was not fetched, so it did not run", src));
                        return;
                    }
                }
            }
            // Numbered by the element it came from, so that two inline scripts on
            // one page can be told apart in a stack trace.
            None => (script.source.clone(), alloc::format!("{}#script{}", self.url, script.node)),
        };

        let started = crate::clock::micros();
        // Where a `document.write` from this script lands.
        self.write_anchor = script.node;
        if let Err(why) = self.eval_bounded(&source, &name) {
            self.errors.push(why);
        }
        self.write_anchor = NO_NODE;
        let spent = crate::clock::micros().saturating_sub(started);
        self.page_budget = self.page_budget.saturating_sub(spent);
        if self.page_budget == 0 {
            self.errors
                .push("the rest of this page's scripts were skipped: they ran for too long".to_string());
        }
    }

    /// Dispatch an event at a node, through the capture, target and bubble
    /// phases, then drain the job queue.
    ///
    /// `init` is a JSON object for the event's own fields — `{"key":"a"}` for a
    /// keystroke — or empty for none. It is passed as a value rather than spliced
    /// into a source string, so a page's own text cannot become code.
    pub fn dispatch(&mut self, target: NodeId, kind: &str, init: &str) -> Dispatch {
        if target == NO_NODE {
            return Dispatch::default();
        }
        let result = self.fire(target as i64, kind, init);
        self.settle();
        result
    }

    /// Fire a click at an element and let it bubble.
    pub fn dispatch_click(&mut self, target: NodeId) -> Dispatch {
        self.dispatch(target, "click", "")
    }

    /// The same, carrying where on the page the pointer was, which a handler
    /// reads as `event.clientX`.
    pub fn dispatch_click_at(&mut self, target: NodeId, x: f32, y: f32) -> Dispatch {
        let init = alloc::format!("{{\"clientX\":{},\"clientY\":{}}}", x as i32, y as i32);
        self.dispatch(target, "click", &init)
    }

    /// Dispatch on the document, for the events that belong to it rather than to
    /// an element.
    pub fn dispatch_on_document(&mut self, kind: &str, init: &str) -> Dispatch {
        let result = self.fire(JS_DOCUMENT, kind, init);
        self.settle();
        result
    }

    fn fire(&mut self, target: i64, kind: &str, init: &str) -> Dispatch {
        let kind = kind.to_string();
        let init = init.to_string();
        let (outcome, rejected) = self.run((Err(no_engine()), None), |engine| {
            engine.set_time_limit(SCRIPT_BUDGET_MICROS);
            let flags = engine.call_global(
                "__os101_dispatch",
                &[Arg::Int(target as i32), Arg::Text(&kind), Arg::Text(&init)],
            );
            let pumped = engine.pump_jobs();
            engine.clear_time_limit();
            (flags.map_err(|why| explain(&why)), pumped.err())
        });
        if let Some(why) = rejected {
            self.note(alloc::format!("unhandled rejection: {}", explain(&why)));
        }
        self.refresh_pending();

        match outcome {
            Ok(flags) => {
                let bits: u32 = flags.parse().unwrap_or(0);
                Dispatch { handled: bits & 1 != 0, prevented: bits & 2 != 0 }
            }
            Err(why) => {
                self.note(alloc::format!("dispatching {}: {}", kind, why));
                Dispatch::default()
            }
        }
    }

    /// Run the timers that are due, the animation callbacks that are waiting and
    /// any queued promise jobs, then lay the page out again if any of them
    /// changed it.
    ///
    /// Called from the browser's idle path, so a page that continues in a
    /// `setTimeout` or a `.then` carries on while the user is looking at it. True
    /// if something ran.
    pub fn pump(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        let now = now_ms();
        let (outcome, rejected) = self.run((Err(no_engine()), None), |engine| {
            engine.set_time_limit(SCRIPT_BUDGET_MICROS);
            let ran = engine.call_global("__os101_tick", &[Arg::Number(now)]);
            let pumped = engine.pump_jobs();
            engine.clear_time_limit();
            (ran.map_err(|why| explain(&why)), pumped.err())
        });
        if let Some(why) = rejected {
            self.note(alloc::format!("unhandled rejection: {}", explain(&why)));
        }

        let ran = match outcome {
            // `__os101_tick` answers `<callbacks run>,<anything still waiting>`,
            // so one call both does the work and says whether to come back.
            Ok(answer) => {
                let mut fields = answer.split(',');
                let count: usize = fields.next().unwrap_or("0").parse().unwrap_or(0);
                self.pending = fields.next() == Some("1");
                count
            }
            Err(why) => {
                self.note(alloc::format!("running a timer: {}", why));
                // Something is badly wrong with the engine; asking it again every
                // pass would fill the log rather than fix it.
                self.pending = false;
                0
            }
        };
        let changed = self.dirty;
        self.settle();
        ran > 0 || changed
    }

    /// Is anything waiting for the next [`Session::pump`]?
    ///
    /// The browser asks so it can keep the event loop awake for a page with a
    /// timer running instead of halting until the next keystroke. Read from a
    /// flag rather than from the engine, because this is on the hot path of the
    /// main loop.
    pub fn has_pending_work(&self) -> bool {
        self.pending
    }

    /// Ask the engine whether anything is queued, after something that could
    /// have queued one. Every path that runs script goes through here.
    fn refresh_pending(&mut self) {
        let answer = self.run(String::new(), |engine| {
            if engine.has_pending_jobs() {
                return String::from("true");
            }
            engine.call_global("__os101_pending", &[]).unwrap_or_default()
        });
        self.pending = answer == "true";
    }

    /// Where a script asked the browser to go, if anywhere. Taken rather than
    /// read, so it is acted on once.
    pub fn take_pending_navigation(&mut self) -> Option<String> {
        self.pending_navigation.take()
    }

    /// Lay the page out again if a script changed the document.
    fn settle(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.page.refresh_ids();
        self.page.relayout();
    }

    fn note(&mut self, message: String) {
        crate::serial_println!("browser js: {}", message);
        if self.log.len() >= MAX_LOG {
            self.log.remove(0);
        }
        self.log.push(message);
    }
}

/// A failure message worth showing a user.
///
/// Two of the engine's own failures need translating. A script stopped by the
/// interrupt handler was not wrong, it was too long, and QuickJS says
/// "InternalError: interrupted", which reads like a kernel fault. And a runtime
/// that has run out of memory raises nothing at all, which the wrapper already
/// describes — but not in words that mean anything on a status line.
fn explain(why: &str) -> String {
    if why.contains("interrupted") {
        return "script stopped: it ran for too long".to_string();
    }
    if why.contains("without setting an exception") || why.contains("out of memory") {
        return "script stopped: it ran out of memory".to_string();
    }
    why.to_string()
}

fn no_engine() -> String {
    "this page has no script engine".to_string()
}

/// The clock the timers run on: monotonic milliseconds since boot, which is what
/// `__h_now_ms` hands to JavaScript as well, so the two agree about when a timer
/// is due. Not the wall clock — `Date.now()` is that, and it may step.
fn now_ms() -> f64 {
    (crate::clock::micros() / 1_000) as f64
}

// ── reaching the session from a native ──────────────────────────────────────

/// Run `f` against the session the engine is bound to.
///
/// # Safety
/// The pointer is published by [`Session::run`] only for as long as the engine
/// is inside a call, and only after the engine has been moved out of the session
/// — so there is no live Rust reference to alias. See the module comment.
fn host<R>(fallback: R, f: impl FnOnce(&mut Session) -> R) -> R {
    let active = ACTIVE.load(Ordering::Relaxed);
    if active.is_null() {
        return fallback;
    }
    f(unsafe { &mut *active })
}

/// A node argument, as the JavaScript side sends them: a real id, or one of the
/// negative sentinels.
fn node_arg(args: &Args, index: usize) -> NodeId {
    match args.int(index, JS_NONE) {
        id if id >= 0 => id as NodeId,
        _ => NO_NODE,
    }
}

fn text_arg(args: &Args, index: usize) -> String {
    args.string(index).unwrap_or_default()
}

/// A list of node ids, in the comma-separated form every list-returning native
/// answers with.
fn id_list(ids: &[NodeId]) -> Return {
    let mut out = String::new();
    for id in ids {
        if *id == NO_NODE {
            continue;
        }
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(&alloc::format!("{}", id));
    }
    Return::Text(out)
}

fn maybe_node(id: Option<NodeId>) -> Return {
    match id {
        Some(id) if id != NO_NODE => Return::Int(id as i32),
        _ => Return::Int(JS_NONE as i32),
    }
}

// ── the document, as the session sees it ────────────────────────────────────

impl Session {
    /// A node anywhere this session can reach: in the document, or in a subtree a
    /// script is holding on to.
    fn find(&self, id: NodeId) -> Option<&Node> {
        if id == NO_NODE {
            return None;
        }
        if let Some(node) = self.page.dom.by_id(id) {
            return Some(node);
        }
        self.detached.iter().find_map(|root| root.by_id(id))
    }

    fn find_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if id == NO_NODE {
            return None;
        }
        if self.page.dom.by_id(id).is_some() {
            return self.page.dom.by_id_mut(id);
        }
        // Located first and borrowed second: a `&mut` handed out of a loop over
        // `detached` would keep the whole vector borrowed.
        let which = self.detached.iter().position(|root| root.by_id(id).is_some())?;
        self.detached[which].by_id_mut(id)
    }

    fn element(&self, id: NodeId) -> Option<&super::dom::ElementData> {
        self.find(id)?.as_element()
    }

    fn touch(&mut self) {
        self.dirty = true;
    }

    /// The parent of a node, whether it is in the document or in a detached
    /// subtree. `None` for a document root and for a detached root, which is
    /// what makes `parentNode` on a freshly created element null.
    fn parent_of(&self, id: NodeId) -> Option<NodeId> {
        if let Some(parent) = self.page.dom.parent_of(id) {
            return Some(parent);
        }
        self.detached.iter().find_map(|root| root.parent_of(id))
    }

    /// Cut a node out of wherever it is and hand it over.
    fn take_node(&mut self, id: NodeId) -> Option<Node> {
        if id == NO_NODE || id == self.page.dom.id {
            return None;
        }
        if let Some(which) = self.detached.iter().position(|root| root.id == id) {
            return Some(self.detached.remove(which));
        }
        if let Some(parent) = self.page.dom.parent_of(id) {
            let parent = self.page.dom.by_id_mut(parent)?;
            let index = parent.children.iter().position(|child| child.id == id)?;
            self.dirty = true;
            return Some(parent.children.remove(index));
        }
        for root in self.detached.iter_mut() {
            let Some(parent) = root.parent_of(id) else { continue };
            let Some(holder) = root.by_id_mut(parent) else { continue };
            if let Some(index) = holder.children.iter().position(|child| child.id == id) {
                return Some(holder.children.remove(index));
            }
        }
        None
    }

    /// Hold on to a subtree that is not in the document, so a script can still
    /// reach it and put it back.
    fn park(&mut self, node: Node) {
        if self.detached.len() < MAX_DETACHED {
            self.detached.push(node);
        }
    }

    /// Give a fresh subtree its ids and park it.
    fn adopt(&mut self, mut node: Node) -> NodeId {
        self.page.adopt_ids(&mut node);
        let id = node.id;
        self.park(node);
        id
    }

    /// Move `child` under `parent`, before `before` if it is one of `parent`'s
    /// children and at the end otherwise.
    fn attach(&mut self, parent: NodeId, child: NodeId, before: NodeId) -> bool {
        if parent == child || parent == NO_NODE || child == NO_NODE {
            return false;
        }
        // Moving a node into its own subtree would build a cycle, and every
        // later tree walk would never come back.
        if let Some(node) = self.find(child) {
            if node.by_id(parent).is_some() {
                return false;
            }
        }
        if self.find(parent).is_none() {
            return false;
        }

        let index = match before {
            NO_NODE => None,
            reference => self
                .find(parent)
                .and_then(|node| node.children.iter().position(|c| c.id == reference)),
        };

        let Some(node) = self.take_node(child) else { return false };
        // `take_node` cannot have removed the parent — it is not in the subtree,
        // which was checked above — so this lookup still finds it.
        let Some(target) = self.find_mut(parent) else {
            self.park(node);
            return false;
        };
        match index {
            Some(at) if at <= target.children.len() => target.children.insert(at, node),
            _ => target.children.push(node),
        }
        self.touch();
        true
    }

    /// Remove `child` from `parent`, keeping it alive so a script can put it
    /// back — which is what the DOM promises and what `removeChild`'s return
    /// value is for.
    fn detach_from(&mut self, parent: NodeId, child: NodeId) -> bool {
        if self.parent_of(child) != Some(parent) {
            return false;
        }
        match self.take_node(child) {
            Some(node) => {
                self.park(node);
                self.touch();
                true
            }
            None => false,
        }
    }

    fn detach(&mut self, id: NodeId) -> bool {
        match self.take_node(id) {
            Some(node) => {
                self.park(node);
                self.touch();
                true
            }
            None => false,
        }
    }

    fn replace(&mut self, parent: NodeId, fresh: NodeId, stale: NodeId) -> bool {
        let is_child = self
            .find(parent)
            .map(|node| node.children.iter().any(|child| child.id == stale))
            .unwrap_or(false);
        if !is_child {
            return false;
        }
        // Put the new one where the old one is, then take the old one out. The
        // old node stays reachable, which is what `replaceChild` returning it is
        // for.
        self.attach(parent, fresh, stale) && self.detach_from(parent, stale)
    }

    fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) -> bool {
        let Some(element) = self.find_mut(id).and_then(|node| node.as_element_mut()) else {
            return false;
        };
        element.set_attr(name, value);
        // A field's value lives in the control table rather than in the
        // document, so an assignment has to reach both; otherwise the two
        // disagree and the relayout that follows puts the old value back.
        if name.eq_ignore_ascii_case("value") {
            if let Some(control) = self.page.forms.get_mut(id) {
                control.set_value(value);
            }
        }
        self.touch();
        true
    }

    fn remove_attribute(&mut self, id: NodeId, name: &str) -> bool {
        let Some(element) = self.find_mut(id).and_then(|node| node.as_element_mut()) else {
            return false;
        };
        element.remove_attr(name);
        self.touch();
        true
    }

    /// Everything `selector` matches below `root`, in document order.
    ///
    /// `root` itself is never a result, which is what `querySelector` says: a
    /// `<div>` asked for `'div'` answers with the first one inside it, not with
    /// itself.
    fn matching(&self, selector: &str, root: NodeId) -> Vec<NodeId> {
        let Some(parsed) = css::parse_selector(selector) else { return Vec::new() };
        let mut out = Vec::new();
        collect_matches(&self.page.dom, &parsed, root, &mut out);
        out.retain(|id| *id != root);
        out
    }

    /// The ancestor elements of a node, outermost first, which is what the
    /// selector matcher needs to answer a descendant combinator.
    fn ancestor_chain(&self, id: NodeId) -> Vec<NodeId> {
        let mut chain = Vec::new();
        let mut current = id;
        while let Some(parent) = self.parent_of(current) {
            chain.push(parent);
            current = parent;
            if chain.len() > 64 {
                break;
            }
        }
        chain
    }

    fn matches_selector(&self, id: NodeId, selector: &str) -> bool {
        let Some(parsed) = css::parse_selector(selector) else { return false };
        let Some(node) = self.find(id) else { return false };
        let chain = self.ancestor_chain(id);
        let mut ancestors = Vec::new();
        for above in chain.iter().rev() {
            if let Some(element) = self.element(*above) {
                ancestors.push(element);
            }
        }
        style::matches(node, &ancestors, &parsed)
    }

    /// Where a node ended up on the page, as `x,y,width,height`.
    ///
    /// The layout is brought up to date first: geometry is the one thing a
    /// script can ask for that a batched mutation would answer wrongly.
    fn rect_of(&mut self, id: NodeId) -> String {
        self.settle();
        let mut found: Option<(f32, f32, f32, f32)> = None;
        for (node, rect) in &self.page.display.geometry {
            if *node != id {
                continue;
            }
            let (left, top) = (rect.x, rect.y);
            let (right, bottom) = (rect.x + rect.width, rect.y + rect.height);
            found = Some(match found {
                None => (left, top, right, bottom),
                Some((l, t, r, b)) => (l.min(left), t.min(top), r.max(right), b.max(bottom)),
            });
        }
        match found {
            Some((l, t, r, b)) => alloc::format!("{},{},{},{}", l, t, r - l, b - t),
            // An element with no box — `display: none`, or one not in the
            // document — is all zeroes, exactly as in a real browser.
            None => String::from("0,0,0,0"),
        }
    }

    fn value_of(&self, id: NodeId) -> String {
        self.page
            .forms
            .get(id)
            .map(|control| control.value.clone())
            .or_else(|| self.element(id).and_then(|e| e.attr("value")).map(String::from))
            .unwrap_or_default()
    }

    /// One part of this document's address.
    fn location_part(&self, part: &str) -> String {
        let url = self.url.clone();
        let (scheme, rest) = match url.find("://") {
            Some(at) => (url[..at].to_string(), url[at + 3..].to_string()),
            None => (String::from("http"), url.clone()),
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (rest[..at].to_string(), rest[at..].to_string()),
            None => (rest.clone(), String::from("/")),
        };
        let (host_only, port) = match authority.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                (host.to_string(), port.to_string())
            }
            _ => (authority.clone(), String::new()),
        };
        let (before_hash, hash) = match path.split_once('#') {
            Some((before, after)) => (before.to_string(), alloc::format!("#{}", after)),
            None => (path.clone(), String::new()),
        };
        let (pathname, search) = match before_hash.split_once('?') {
            Some((before, after)) => (before.to_string(), alloc::format!("?{}", after)),
            None => (before_hash.clone(), String::new()),
        };

        match part {
            "href" => url,
            "protocol" => alloc::format!("{}:", scheme),
            "host" => authority,
            "hostname" => host_only,
            "port" => port,
            "pathname" => {
                if pathname.is_empty() {
                    String::from("/")
                } else {
                    pathname
                }
            }
            "search" => search,
            "hash" => hash,
            "origin" => alloc::format!("{}://{}", scheme, authority),
            _ => String::new(),
        }
    }
}

// ── the natives ─────────────────────────────────────────────────────────────

/// Install every host function the prelude expects.
///
/// The names are the contract with `domjs.js`, which lists them all in one place
/// and throws a describable error rather than "not a function" if one is
/// missing.
fn register_natives(engine: &mut Engine) -> Result<(), String> {
    // (name, declared arity, function). The arity is only what JavaScript sees
    // as `length`; every native still checks what actually arrived.
    let table: &[(&str, i32, crate::quickjs::NativeFn)] = &[
        ("__h_doc_node", 1, n_doc_node),
        ("__h_get_by_id", 1, n_get_by_id),
        ("__h_query_first", 2, n_query_first),
        ("__h_query_all", 2, n_query_all),
        ("__h_by_tag", 2, n_by_tag),
        ("__h_by_class", 2, n_by_class),
        ("__h_matches", 2, n_matches),
        ("__h_closest", 2, n_closest),
        ("__h_title", 0, n_title),
        ("__h_set_title", 1, n_set_title),
        ("__h_ready_state", 0, n_ready_state),
        ("__h_create_element", 1, n_create_element),
        ("__h_create_text", 1, n_create_text),
        ("__h_exists", 1, n_exists),
        ("__h_node_type", 1, n_node_type),
        ("__h_tag", 1, n_tag),
        ("__h_text", 1, n_text),
        ("__h_set_text", 2, n_set_text),
        ("__h_inner_text", 1, n_inner_text),
        ("__h_inner_html", 1, n_inner_html),
        ("__h_set_inner_html", 2, n_set_inner_html),
        ("__h_outer_html", 1, n_outer_html),
        ("__h_write", 1, n_write),
        ("__h_get_attr", 2, n_get_attr),
        ("__h_set_attr", 3, n_set_attr),
        ("__h_remove_attr", 2, n_remove_attr),
        ("__h_attr_names", 1, n_attr_names),
        ("__h_parent", 1, n_parent),
        ("__h_children", 2, n_children),
        ("__h_sibling", 3, n_sibling),
        ("__h_path", 1, n_path),
        ("__h_append", 2, n_append),
        ("__h_insert_before", 3, n_insert_before),
        ("__h_remove_child", 2, n_remove_child),
        ("__h_replace_child", 3, n_replace_child),
        ("__h_detach", 1, n_detach),
        ("__h_clone", 2, n_clone),
        ("__h_contains", 2, n_contains),
        ("__h_value", 1, n_value),
        ("__h_set_value", 2, n_set_value),
        ("__h_checked", 1, n_checked),
        ("__h_set_checked", 2, n_set_checked),
        ("__h_style_get", 2, n_style_get),
        ("__h_style_set", 3, n_style_set),
        ("__h_style_text", 1, n_style_text),
        ("__h_set_style_text", 2, n_set_style_text),
        ("__h_rect", 1, n_rect),
        ("__h_log", 2, n_log),
        ("__h_alert", 1, n_alert),
        ("__h_now_ms", 0, n_now_ms),
        ("__h_navigate", 1, n_navigate),
        ("__h_location", 1, n_location),
        ("__h_cookie", 0, n_cookie),
        ("__h_set_cookie", 1, n_set_cookie),
        ("__h_storage", 4, n_storage),
        ("__h_viewport", 1, n_viewport),
        ("__h_user_agent", 0, n_user_agent),
        ("__h_focus", 2, n_focus),
        ("__h_submit", 1, n_submit),
    ];

    for (name, arity, function) in table {
        engine.register_global(name, *arity, *function)?;
    }
    Ok(())
}

// Document and tree queries.

fn n_doc_node(args: &Args) -> Return {
    let which = args.int(0, 0);
    host(Return::Int(JS_NONE as i32), |session| {
        let root = session.page.dom.id;
        let found = match which {
            1 => session.page.dom.find_tag("html").map(|n| n.id).or(Some(root)),
            // A page with no `<body>` still has to answer `document.body`, or
            // `document.body.appendChild` — which is how half of all generated
            // pages build themselves — throws on a fragment. The outermost
            // element it does have is the honest stand-in.
            2 => session
                .page
                .dom
                .find_tag("body")
                .map(|n| n.id)
                .or_else(|| session.page.dom.find_tag("html").map(|n| n.id))
                .or(Some(root)),
            3 => session.page.dom.find_tag("head").map(|n| n.id).or(Some(root)),
            _ => Some(root),
        };
        maybe_node(found)
    })
}

fn n_get_by_id(args: &Args) -> Return {
    let wanted = text_arg(args, 0);
    host(Return::Int(JS_NONE as i32), |session| {
        let found = session.page.dom.descendants().iter().find_map(|node| {
            let element = node.as_element()?;
            (element.id()? == wanted).then_some(node.id)
        });
        maybe_node(found)
    })
}

fn n_query_first(args: &Args) -> Return {
    let root = node_arg(args, 0);
    let selector = text_arg(args, 1);
    host(Return::Int(JS_NONE as i32), |session| {
        maybe_node(session.matching(&selector, root).first().copied())
    })
}

fn n_query_all(args: &Args) -> Return {
    let root = node_arg(args, 0);
    let selector = text_arg(args, 1);
    host(Return::Text(String::new()), |session| {
        id_list(&session.matching(&selector, root))
    })
}

fn n_by_tag(args: &Args) -> Return {
    let root = node_arg(args, 0);
    let wanted = text_arg(args, 1);
    host(Return::Text(String::new()), |session| {
        let Some(node) = session.find(root) else { return Return::Text(String::new()) };
        let ids: Vec<NodeId> = node
            .descendants()
            .iter()
            .filter(|child| {
                child.id != root && (wanted == "*" || child.tag().eq_ignore_ascii_case(&wanted))
            })
            .map(|child| child.id)
            .collect();
        id_list(&ids)
    })
}

fn n_by_class(args: &Args) -> Return {
    let root = node_arg(args, 0);
    let wanted = text_arg(args, 1);
    host(Return::Text(String::new()), |session| {
        let Some(node) = session.find(root) else { return Return::Text(String::new()) };
        let ids: Vec<NodeId> = node
            .descendants()
            .iter()
            .filter(|child| {
                child.id != root
                    && child
                        .as_element()
                        // Case-sensitive, which is what a standards-mode document
                        // gets: `class="Panel"` is not `class="panel"`.
                        .map(|e| e.classes().any(|class| class == wanted))
                        .unwrap_or(false)
            })
            .map(|child| child.id)
            .collect();
        id_list(&ids)
    })
}

fn n_matches(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let selector = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.matches_selector(id, &selector))
    })
}

fn n_closest(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let selector = text_arg(args, 1);
    host(Return::Int(JS_NONE as i32), |session| {
        let mut current = id;
        for _ in 0..64 {
            if current == NO_NODE {
                break;
            }
            if session.matches_selector(current, &selector) {
                return maybe_node(Some(current));
            }
            match session.parent_of(current) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Return::Int(JS_NONE as i32)
    })
}

fn n_title(_args: &Args) -> Return {
    host(Return::Text(String::new()), |session| {
        Return::Text(session.page.title.clone())
    })
}

fn n_set_title(args: &Args) -> Return {
    let title = text_arg(args, 0);
    host(Return::Undefined, |session| {
        // The `<title>` element has to change too, not just the field: the title
        // is derived state, recomputed from the document on every relayout, so an
        // assignment that only touched the field would be undone by the next one.
        match session.page.dom.find_tag("title").map(|node| node.id) {
            Some(id) => {
                if let Some(node) = session.find_mut(id) {
                    node.set_text_content(title.clone());
                }
            }
            None => {
                let head = session
                    .page
                    .dom
                    .find_tag("head")
                    .map(|node| node.id)
                    .unwrap_or(session.page.dom.id);
                let element =
                    Node::element("title".to_string(), Vec::new(), alloc::vec![Node::text(title.clone())]);
                let fresh = session.adopt(element);
                session.attach(head, fresh, NO_NODE);
            }
        }
        session.page.title = title;
        session.touch();
        Return::Undefined
    })
}

fn n_ready_state(_args: &Args) -> Return {
    host(Return::Text(String::from("complete")), |session| {
        Return::Text(String::from(match session.ready {
            Ready::Loading => "loading",
            Ready::Interactive => "interactive",
            Ready::Complete => "complete",
        }))
    })
}

fn n_create_element(args: &Args) -> Return {
    let tag = text_arg(args, 0).trim().to_ascii_lowercase();
    if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Return::Int(JS_NONE as i32);
    }
    host(Return::Int(JS_NONE as i32), |session| {
        let id = session.adopt(Node::element(tag, Vec::new(), Vec::new()));
        maybe_node(Some(id))
    })
}

fn n_create_text(args: &Args) -> Return {
    let text = text_arg(args, 0);
    host(Return::Int(JS_NONE as i32), |session| {
        let id = session.adopt(Node::text(text));
        maybe_node(Some(id))
    })
}

// Node properties.

fn n_exists(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Bool(false), |session| {
        Return::Bool(session.page.dom.by_id(id).is_some())
    })
}

fn n_node_type(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Int(0), |session| match session.find(id) {
        Some(node) if node.as_element().is_some() => Return::Int(1),
        Some(_) => Return::Int(3),
        None => Return::Int(0),
    })
}

fn n_tag(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.find(id).map(|node| node.tag().to_string()).unwrap_or_default())
    })
}

fn n_text(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.find(id).map(|node| node.text_content()).unwrap_or_default())
    })
}

fn n_inner_text(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        let raw = session.find(id).map(|node| node.text_content()).unwrap_or_default();
        // `innerText` is what a reader sees, so the whitespace is collapsed the
        // way HTML collapses it. `textContent` is the source text and is not.
        Return::Text(super::collapse(&raw))
    })
}

fn n_set_text(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let text = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        match session.find_mut(id) {
            Some(node) => {
                node.set_text_content(text);
                session.touch();
                Return::Bool(true)
            }
            None => Return::Bool(false),
        }
    })
}

fn n_inner_html(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.find(id).map(serialise_children).unwrap_or_default())
    })
}

fn n_outer_html(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.find(id).map(serialise).unwrap_or_default())
    })
}

fn n_set_inner_html(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let html = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        if session.find(id).is_none() {
            return Return::Bool(false);
        }
        // Parsed against a fresh root and numbered before it goes in, so that
        // every node in the new subtree has an id a script can hold.
        let fragment = htmlparse::parse(&html);
        let mut children = fragment.children;
        for child in children.iter_mut() {
            session.page.adopt_ids(child);
        }
        if let Some(node) = session.find_mut(id) {
            node.children = children;
        }
        session.touch();
        Return::Bool(true)
    })
}

/// `document.write`, for the one case where it means anything here.
///
/// The parser has already finished by the time any script runs, so a write cannot
/// go into the stream the way it does in a real browser. What it can do is land
/// where the stream would have put it: immediately after the `<script>` element
/// doing the writing, which is where a browser's parser would have been. That
/// covers what old pages actually use it for — a script that writes the year, a
/// menu, a table of contents.
///
/// Outside a load-time script there is no such position, and false is the answer;
/// the JavaScript side logs it rather than throwing, because a page calling
/// `document.write` from a click handler in a real browser would have its whole
/// document replaced, and doing nothing is kinder than that.
fn n_write(args: &Args) -> Return {
    let html = text_arg(args, 0);
    host(Return::Bool(false), |session| {
        let mut anchor = session.write_anchor;
        if anchor == NO_NODE {
            return Return::Bool(false);
        }
        if html.is_empty() {
            return Return::Bool(true);
        }
        let Some(parent) = session.parent_of(anchor) else { return Return::Bool(false) };

        for mut node in htmlparse::parse(&html).children {
            session.page.adopt_ids(&mut node);
            let id = node.id;
            session.park(node);
            // `attach` inserts *before* a reference, so the reference is whatever
            // currently follows the cursor.
            let before = session
                .find(parent)
                .and_then(|holder| {
                    let at = holder.children.iter().position(|child| child.id == anchor)?;
                    holder.children.get(at + 1)
                })
                .map(|child| child.id)
                .unwrap_or(NO_NODE);
            session.attach(parent, id, before);
            // The cursor moves past what was written, so a second write lands
            // after the first rather than in front of it.
            anchor = id;
        }
        session.write_anchor = anchor;
        Return::Bool(true)
    })
}

fn n_get_attr(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let name = text_arg(args, 1);
    host(Return::Null, |session| {
        match session.element(id).and_then(|element| element.attr(&name)) {
            Some(value) => Return::Text(value.to_string()),
            None => Return::Null,
        }
    })
}

fn n_set_attr(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let name = text_arg(args, 1);
    let value = text_arg(args, 2);
    host(Return::Bool(false), |session| {
        Return::Bool(session.set_attribute(id, &name, &value))
    })
}

fn n_remove_attr(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let name = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.remove_attribute(id, &name))
    })
}

fn n_attr_names(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        let names = session
            .element(id)
            .map(|element| {
                element
                    .attrs
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        Return::Text(names)
    })
}

fn n_parent(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Int(JS_NONE as i32), |session| {
        maybe_node(session.parent_of(id))
    })
}

fn n_children(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let elements_only = args.boolean(1);
    host(Return::Text(String::new()), |session| {
        let Some(node) = session.find(id) else { return Return::Text(String::new()) };
        let ids: Vec<NodeId> = node
            .children
            .iter()
            .filter(|child| !elements_only || child.as_element().is_some())
            .map(|child| child.id)
            .collect();
        id_list(&ids)
    })
}

fn n_sibling(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let forwards = args.int(1, 1) > 0;
    let elements_only = args.boolean(2);
    host(Return::Int(JS_NONE as i32), |session| {
        let Some(parent) = session.parent_of(id) else { return Return::Int(JS_NONE as i32) };
        let Some(node) = session.find(parent) else { return Return::Int(JS_NONE as i32) };
        let Some(at) = node.children.iter().position(|child| child.id == id) else {
            return Return::Int(JS_NONE as i32);
        };

        let mut walk: Vec<&Node> = if forwards {
            node.children[at + 1..].iter().collect()
        } else {
            node.children[..at].iter().rev().collect()
        };
        if elements_only {
            walk.retain(|child| child.as_element().is_some());
        }
        maybe_node(walk.first().map(|child| child.id))
    })
}

fn n_path(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        let mut ids = alloc::vec![id];
        ids.extend(session.ancestor_chain(id));
        id_list(&ids)
    })
}

// Mutation.

fn n_append(args: &Args) -> Return {
    let parent = node_arg(args, 0);
    let child = node_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.attach(parent, child, NO_NODE))
    })
}

fn n_insert_before(args: &Args) -> Return {
    let parent = node_arg(args, 0);
    let child = node_arg(args, 1);
    let before = node_arg(args, 2);
    host(Return::Bool(false), |session| {
        Return::Bool(session.attach(parent, child, before))
    })
}

fn n_remove_child(args: &Args) -> Return {
    let parent = node_arg(args, 0);
    let child = node_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.detach_from(parent, child))
    })
}

fn n_replace_child(args: &Args) -> Return {
    let parent = node_arg(args, 0);
    let fresh = node_arg(args, 1);
    let stale = node_arg(args, 2);
    host(Return::Bool(false), |session| {
        Return::Bool(session.replace(parent, fresh, stale))
    })
}

fn n_detach(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Bool(false), |session| Return::Bool(session.detach(id)))
}

fn n_clone(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let deep = args.boolean(1);
    host(Return::Int(JS_NONE as i32), |session| {
        let Some(node) = session.find(id) else { return Return::Int(JS_NONE as i32) };
        let copy = clone_tree(node, deep, 0);
        let fresh = session.adopt(copy);
        maybe_node(Some(fresh))
    })
}

fn n_contains(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let other = node_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(match session.find(id) {
            Some(node) => node.by_id(other).is_some(),
            None => false,
        })
    })
}

// Form controls.

fn n_value(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.value_of(id))
    })
}

fn n_set_value(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let value = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.set_attribute(id, "value", &value))
    })
}

fn n_checked(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Bool(false), |session| {
        Return::Bool(session.element(id).and_then(|e| e.attr("checked")).is_some())
    })
}

fn n_set_checked(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let on = args.boolean(1);
    host(Return::Bool(false), |session| {
        Return::Bool(if on {
            session.set_attribute(id, "checked", "")
        } else {
            session.remove_attribute(id, "checked")
        })
    })
}

fn n_focus(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let on = args.boolean(1);
    host(Return::Bool(false), |session| {
        if on {
            Return::Bool(session.page.forms.focus_at(id, 0))
        } else {
            session.page.forms.blur();
            Return::Bool(true)
        }
    })
}

fn n_submit(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Bool(false), |session| {
        let url = session.url.clone();
        // `submission` is written around a control — the thing that was pressed —
        // because that is how a click reaches it. `form.submit()` names the form
        // instead, so one of its controls has to stand in.
        let control = match session.find(id).map(|node| node.tag().eq_ignore_ascii_case("form")) {
            Some(true) => session
                .page
                .forms
                .iter()
                .find(|control| {
                    control.form == id
                        && !control.disabled
                        && (control.kind.submits() || control.kind.editable())
                })
                .map(|control| control.node),
            _ => Some(id),
        };
        let Some(control) = control else { return Return::Bool(false) };

        // Submitting means navigating, and navigating from inside a native would
        // deadlock against the window manager's lock — so it is recorded and the
        // browser acts on it once the dispatch has returned.
        match session.page.forms.submission(&session.page.dom, control, &url) {
            Some(target) => {
                session.pending_navigation = Some(target);
                Return::Bool(true)
            }
            None => Return::Bool(false),
        }
    })
}

// Inline style.

fn n_style_get(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let property = text_arg(args, 1);
    host(Return::Text(String::new()), |session| {
        let inline = session.element(id).and_then(|e| e.attr("style")).unwrap_or("");
        Return::Text(find_declaration(inline, &property).unwrap_or_default())
    })
}

fn n_style_set(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let property = text_arg(args, 1);
    let value = text_arg(args, 2);
    host(Return::Bool(false), |session| {
        let inline = session
            .element(id)
            .and_then(|e| e.attr("style"))
            .unwrap_or("")
            .to_string();
        let updated = set_declaration(&inline, &property, &value);
        Return::Bool(session.set_attribute(id, "style", &updated))
    })
}

fn n_style_text(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(
            session
                .element(id)
                .and_then(|e| e.attr("style"))
                .unwrap_or("")
                .to_string(),
        )
    })
}

fn n_set_style_text(args: &Args) -> Return {
    let id = node_arg(args, 0);
    let css = text_arg(args, 1);
    host(Return::Bool(false), |session| {
        Return::Bool(session.set_attribute(id, "style", &css))
    })
}

fn n_rect(args: &Args) -> Return {
    let id = node_arg(args, 0);
    host(Return::Text(String::from("0,0,0,0")), |session| {
        Return::Text(session.rect_of(id))
    })
}

// The window and its surroundings.

fn n_log(args: &Args) -> Return {
    let level = text_arg(args, 0);
    let message = text_arg(args, 1);
    let prefix = match level.as_str() {
        "warn" => "warning: ",
        "error" => "error: ",
        _ => "",
    };
    host(Return::Undefined, |session| {
        session.note(alloc::format!("{}{}", prefix, message));
        Return::Undefined
    })
}

fn n_alert(args: &Args) -> Return {
    let message = text_arg(args, 0);
    host(Return::Undefined, |session| {
        session.note(message);
        Return::Undefined
    })
}

fn n_now_ms(_args: &Args) -> Return {
    Return::Number(now_ms())
}

fn n_navigate(args: &Args) -> Return {
    let target = text_arg(args, 0);
    host(Return::Undefined, |session| {
        let resolved = super::resolve_url(&session.url, &target);
        session.pending_navigation = Some(resolved);
        Return::Undefined
    })
}

fn n_location(args: &Args) -> Return {
    let part = text_arg(args, 0);
    host(Return::Text(String::new()), |session| {
        Return::Text(session.location_part(&part))
    })
}

fn n_cookie(_args: &Args) -> Return {
    host(Return::Text(String::new()), |session| {
        let jar = session
            .cookies
            .iter()
            .map(|(name, value)| alloc::format!("{}={}", name, value))
            .collect::<Vec<_>>()
            .join("; ");
        Return::Text(jar)
    })
}

fn n_set_cookie(args: &Args) -> Return {
    let declaration = text_arg(args, 0);
    host(Return::Undefined, |session| {
        // Only the first pair matters; `path`, `expires` and the rest describe a
        // persistence this jar does not have.
        let pair = declaration.split(';').next().unwrap_or("");
        if let Some((name, value)) = pair.split_once('=') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if !name.is_empty() && session.cookies.len() < 64 {
                match session.cookies.iter_mut().find(|(existing, _)| *existing == name) {
                    Some((_, slot)) => *slot = value,
                    None => session.cookies.push((name, value)),
                }
            }
        }
        Return::Undefined
    })
}

fn n_storage(args: &Args) -> Return {
    let kind = text_arg(args, 0);
    let op = text_arg(args, 1);
    let key = text_arg(args, 2);
    let value = text_arg(args, 3);
    host(Return::Null, |session| {
        if kind == "session" {
            return store_op(&mut session.session_store, &op, &key, &value);
        }
        let origin = session.location_part("origin");
        storage::with(&origin, |entries| store_op(entries, &op, &key, &value))
    })
}

/// One `Storage` operation against a flat key/value list.
fn store_op(entries: &mut Vec<(String, String)>, op: &str, key: &str, value: &str) -> Return {
    match op {
        "get" => match entries.iter().find(|(name, _)| name == key) {
            Some((_, found)) => Return::Text(found.clone()),
            None => Return::Null,
        },
        "set" => {
            let used: usize = entries.iter().map(|(k, v)| k.len() + v.len()).sum();
            match entries.iter_mut().find(|(name, _)| name == key) {
                Some((_, slot)) => *slot = value.to_string(),
                None if used + key.len() + value.len() <= MAX_STORE_BYTES => {
                    entries.push((key.to_string(), value.to_string()))
                }
                // A quota failure is a DOMException in a browser. Throwing from
                // a native is not something this bridge can do, so it is a
                // silent refusal — noted in the report rather than hidden.
                None => {}
            }
            Return::Undefined
        }
        "remove" => {
            entries.retain(|(name, _)| name != key);
            Return::Undefined
        }
        "clear" => {
            entries.clear();
            Return::Undefined
        }
        "key" => match key.parse::<usize>().ok().and_then(|at| entries.get(at)) {
            Some((name, _)) => Return::Text(name.clone()),
            None => Return::Null,
        },
        "length" => Return::Int(entries.len() as i32),
        _ => Return::Null,
    }
}

fn n_viewport(args: &Args) -> Return {
    let which = args.int(0, 0);
    host(Return::Number(0.0), |session| {
        let viewport = session.page.viewport();
        Return::Number(match which {
            1 => viewport.height as f64,
            _ => viewport.width as f64,
        })
    })
}

fn n_user_agent(_args: &Args) -> Return {
    Return::Text(crate::net::http::user_agent().to_string())
}

// ── localStorage, which outlives the page ───────────────────────────────────

/// `localStorage`, kept per origin and written to the data disk so it survives a
/// reboot — which is the whole difference between it and `sessionStorage`.
///
/// One flat file rather than one per origin: there are only ever a handful of
/// origins on a machine like this, and a single small write is cheaper on an ATA
/// disk than a directory walk.
mod storage {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use spin::Mutex;

    const NOTE: &str = "/disk/system/localstorage.txt";
    /// Everything, across every origin. Small because the disk write is
    /// synchronous and on the GUI's thread.
    const MAX_BYTES: usize = 32 * 1024;

    static STORE: Mutex<Option<Vec<(String, Vec<(String, String)>)>>> = Mutex::new(None);

    /// Run `f` against one origin's entries, loading the file on first use and
    /// writing it back if anything changed.
    pub fn with<R>(origin: &str, f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
        let mut guard = STORE.lock();
        if guard.is_none() {
            *guard = Some(load());
        }
        let store = guard.as_mut().expect("just filled in");

        let which = match store.iter().position(|(name, _)| name == origin) {
            Some(at) => at,
            None => {
                store.push((origin.to_string(), Vec::new()));
                store.len() - 1
            }
        };
        let before = store[which].1.len();
        let before_bytes: usize = store[which].1.iter().map(|(k, v)| k.len() + v.len()).sum();
        let result = f(&mut store[which].1);
        let after_bytes: usize = store[which].1.iter().map(|(k, v)| k.len() + v.len()).sum();
        if store[which].1.len() != before || after_bytes != before_bytes {
            save(store);
        }
        result
    }

    /// Lines of `origin<TAB>key<TAB>value`, with the three characters that would
    /// break that escaped. A value with a newline in it is ordinary — a page
    /// storing JSON with an indent produces one — so this cannot be skipped.
    fn escape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for c in text.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out
    }

    fn unescape(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }

    fn load() -> Vec<(String, Vec<(String, String)>)> {
        let Ok(bytes) = crate::fs::cmd_cat(NOTE) else { return Vec::new() };
        let Ok(text) = core::str::from_utf8(&bytes) else { return Vec::new() };
        let mut store: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for line in text.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(origin), Some(key), Some(value)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let origin = unescape(origin);
            let entry = match store.iter_mut().find(|(name, _)| *name == origin) {
                Some(found) => found,
                None => {
                    store.push((origin, Vec::new()));
                    store.last_mut().expect("just pushed")
                }
            };
            entry.1.push((unescape(key), unescape(value)));
        }
        store
    }

    fn save(store: &[(String, Vec<(String, String)>)]) {
        // No disk means no persistence, which is a degradation and not a
        // failure: the in-memory store still works for as long as the machine is
        // up.
        if !crate::fs::has_disk() {
            return;
        }
        let mut out = String::new();
        for (origin, entries) in store {
            for (key, value) in entries {
                if out.len() + key.len() + value.len() + origin.len() + 3 > MAX_BYTES {
                    break;
                }
                out.push_str(&escape(origin));
                out.push('\t');
                out.push_str(&escape(key));
                out.push('\t');
                out.push_str(&escape(value));
                out.push('\n');
            }
        }
        let _ = crate::fs::cmd_mkdir("/disk/system");
        let _ = crate::fs::cmd_write_file(NOTE, out.into_bytes());
    }

    /// Forget one origin's entries, and write the file back without them.
    ///
    /// For the self-test, which stores under an origin no real page has and has
    /// no business leaving anything behind on the user's disk. Deliberately not
    /// "forget everything": that would wipe a user's stored data on every boot.
    pub fn forget(origin: &str) {
        let mut guard = STORE.lock();
        if guard.is_none() {
            *guard = Some(load());
        }
        let store = guard.as_mut().expect("just filled in");
        let before = store.len();
        store.retain(|(name, _)| name != origin);
        if store.len() != before {
            save(store);
        }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Make sure the document has a `<head>`.
///
/// `document.head` has to be something, and a page built as a fragment has no
/// head at all. It is also the one element the user-agent sheet never renders,
/// so anything a script appends to it stays invisible — which is what a script
/// adding a stylesheet or a tracking tag expects.
fn ensure_head(page: &mut Page) {
    if page.dom.find_tag("head").is_some() {
        return;
    }
    page.dom
        .children
        .insert(0, Node::element("head".to_string(), Vec::new(), Vec::new()));
    page.refresh_ids();
}

/// Copy a subtree, leaving the copy unnumbered for [`Page::adopt_ids`].
fn clone_tree(node: &Node, deep: bool, depth: usize) -> Node {
    let children = if deep && depth < super::dom::MAX_DEPTH {
        node.children.iter().map(|child| clone_tree(child, true, depth + 1)).collect()
    } else {
        Vec::new()
    };
    match &node.kind {
        NodeKind::Text(text) => Node::text(text.clone()),
        NodeKind::Element(element) => {
            Node::element(element.tag.clone(), element.attrs.clone(), children)
        }
    }
}

/// Walk the tree below `root` collecting everything the selector matches.
fn collect_matches(root: &Node, selector: &css::Selector, from: NodeId, out: &mut Vec<NodeId>) {
    let Some(start) = root.by_id(from) else { return };

    // The ancestor chain above the starting point still counts for matching,
    // so a `nav a` selector run from inside a nav behaves correctly.
    let mut ancestors = Vec::new();
    let mut chain = Vec::new();
    let mut current = from;
    while let Some(parent) = root.parent_of(current) {
        chain.push(parent);
        current = parent;
        if chain.len() > 64 {
            break;
        }
    }
    for id in chain.iter().rev() {
        if let Some(element) = root.by_id(*id).and_then(|n| n.as_element()) {
            ancestors.push(element);
        }
    }

    walk_matches(start, selector, &mut ancestors, out, 0);
}

fn walk_matches<'a>(
    node: &'a Node,
    selector: &css::Selector,
    ancestors: &mut Vec<&'a super::dom::ElementData>,
    out: &mut Vec<NodeId>,
    depth: usize,
) {
    if depth >= super::dom::MAX_DEPTH || out.len() >= 1024 {
        return;
    }
    if style::matches(node, ancestors, selector) {
        out.push(node.id);
    }
    if let Some(element) = node.as_element() {
        ancestors.push(element);
    }
    for child in &node.children {
        walk_matches(child, selector, ancestors, out, depth + 1);
    }
    if node.as_element().is_some() {
        ancestors.pop();
    }
}

fn find_declaration(inline: &str, property: &str) -> Option<String> {
    inline.split(';').find_map(|declaration| {
        let (name, value) = declaration.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case(property)
            .then(|| value.trim().to_string())
    })
}

/// Rewrite an inline `style` attribute with one property changed.
///
/// An empty value removes the declaration, which is what assigning `''` to a
/// style property means.
fn set_declaration(inline: &str, property: &str, value: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;

    for declaration in inline.split(';') {
        let Some((name, existing)) = declaration.split_once(':') else { continue };
        if name.trim().eq_ignore_ascii_case(property) {
            replaced = true;
            if !value.trim().is_empty() {
                out.push(alloc::format!("{}: {}", property, value.trim()));
            }
        } else {
            out.push(alloc::format!("{}:{}", name.trim(), existing.trim_end()));
        }
    }

    if !replaced && !value.trim().is_empty() {
        out.push(alloc::format!("{}: {}", property, value.trim()));
    }
    out.join("; ")
}

/// Serialise a node back to HTML.
fn serialise(node: &Node) -> String {
    let mut out = String::new();
    serialise_into(node, &mut out, 0);
    out
}

fn serialise_children(node: &Node) -> String {
    let mut out = String::new();
    for child in &node.children {
        serialise_into(child, &mut out, 0);
    }
    out
}

fn serialise_into(node: &Node, out: &mut String, depth: usize) {
    if depth >= super::dom::MAX_DEPTH || out.len() > 256 * 1024 {
        return;
    }
    match &node.kind {
        NodeKind::Text(text) => out.push_str(text),
        NodeKind::Element(element) => {
            out.push('<');
            out.push_str(&element.tag);
            for (name, value) in &element.attrs {
                out.push(' ');
                out.push_str(name);
                out.push_str("=\"");
                out.push_str(value);
                out.push('"');
            }
            out.push('>');
            for child in &node.children {
                serialise_into(child, out, depth + 1);
            }
            out.push_str("</");
            out.push_str(&element.tag);
            out.push('>');
        }
    }
}

pub mod selftest;
