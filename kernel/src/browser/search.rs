//! Turning what the user typed into a URL, and the pages OS101 serves itself.
//!
//! # Searching, and the thing Google will not do
//!
//! Typing words in the address bar searches Google, at `www.google.com`, over
//! a real TLS connection to Google's own servers. Typing an address goes
//! there.
//!
//! Google's *results*, though, cannot be shown. Not for want of TLS — that
//! part works now — but because Google no longer puts results in the HTML it
//! serves. Every response to `/search?q=…` is a small JavaScript program that
//! fetches the results and builds the page in the browser, and it is guarded:
//! a client that identifies itself as an older browser is sent an "update your
//! browser" page instead, and one that looks modern is sent the script. There
//! is no parameter, no header and no user agent that produces server-rendered
//! results; this was checked against a dozen variants of the request,
//! including Google's own no-JavaScript retry flow. OS101's script engine is
//! nowhere near able to run what Google ships.
//!
//! So Google is fetched, and then rebuilt from the parts of it that still
//! work. [`is_google_page`] recognises the two pages that arrive empty, and
//! [`google_page`] puts a usable one in their place: Google's wordmark, a
//! search box that submits to Google's own `/search`, and Google's own
//! completions for what was typed — [`SUGGEST`] answers a client like this one
//! with a small JSON array and no script at all, which makes it the last piece
//! of Google that a browser this size can read.
//!
//! Searching from that box goes to Google. When Google answers with its
//! program, [`needs_javascript`] recognises it, [`google_header`] says so, and
//! the same query is run against [`FALLBACK_SEARCH`] — DuckDuckGo's "lite"
//! interface, which still serves plain HTML. The user asked to search Google;
//! what they get is Google's page when Google will serve one, Google's box and
//! Google's suggestions when it will not, and results either way.
//!
//! # The image proxy
//!
//! [`image_gateway`] still routes pictures through `wsrv.nl`, which is no
//! longer about TLS. It is a format converter and a resizer: the kernel
//! decodes PNG, JPEG, GIF and BMP but not WebP or AVIF, which much of the web
//! now serves, and it decodes into a plain `Vec<Color>` — so a six-megapixel
//! photograph would cost twenty-four megabytes of heap to look at. Asking the
//! proxy for a bounded JPEG solves both.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::escape;

/// Where a search goes. Google, as asked for.
const SEARCH: &str = "https://www.google.com/search?q=";

/// Where results come from when Google will not serve any without JavaScript.
///
/// DuckDuckGo's lite interface: no scripts, no images, plain HTML tables, and
/// it answers a client like this one.
pub const FALLBACK_SEARCH: &str = "https://lite.duckduckgo.com/lite/?q=";

/// Google's completion service, which is the one part of Google that still
/// answers a browser like this one.
///
/// It replies with a small JSON array over TLS — no script, no key, no cookie
/// — so the completions on OS101's Google page are Google's own, for the words
/// the user actually typed. See [`suggestions`].
const SUGGEST: &str = "https://suggestqueries.google.com/complete/search?client=firefox&q=";

/// Fetches an image and re-encodes it to something the kernel can decode.
const IMAGES: &str = "https://wsrv.nl/?url=";

/// Photos by keyword. Sizes and a `lock` are part of the path, so the same
/// lock gives the same photograph at any size — which is what lets a thumbnail
/// and the full-size version on the image page be the same picture.
const PHOTOS: &str = "loremflickr.com";

/// The scheme for pages the browser generates itself.
pub const INTERNAL: &str = "os101:";

/// Where a new browser window starts.
pub const HOME: &str = "os101:home";

/// How many photographs an image search offers.
const RESULTS: usize = 8;

/// Does this look like somewhere to go, rather than something to search for?
///
/// The test a real browser uses, near enough: an explicit scheme, a host with
/// a dot in it and no spaces, or `localhost`. Everything else is a query —
/// which is the right default, because a search for a mistyped domain is
/// recoverable and a failed lookup is not.
pub fn looks_like_url(input: &str) -> bool {
    let s = input.trim();
    if s.is_empty() || s.contains(char::is_whitespace) {
        return false;
    }
    if s.starts_with(INTERNAL) || s.contains("://") {
        return true;
    }

    let host = s.split(['/', '?', '#']).next().unwrap_or(s);
    let host = host.split('@').next_back().unwrap_or(host);
    let (name, port) = match host.rsplit_once(':') {
        Some((n, p)) => (n, Some(p)),
        None => (host, None),
    };
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    if name.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // A dotted quad: the labels are all numeric, so the domain test below
    // would reject it for having no letters in its last label.
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() == 4
        && labels
            .iter()
            .all(|l| !l.is_empty() && l.len() <= 3 && l.bytes().all(|b| b.is_ascii_digit()))
    {
        return true;
    }

    // A trailing label of two or more letters is what makes it a domain and
    // not a sentence with a full stop in it.
    match name.rsplit_once('.') {
        Some((before, tld)) => {
            !before.is_empty()
                && tld.len() >= 2
                && tld.bytes().all(|b| b.is_ascii_alphabetic())
                && !name.contains("..")
        }
        None => false,
    }
}

/// What the address bar should navigate to for `input`.
pub fn address_to_url(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return HOME.to_string();
    }
    if let Some(query) = strip_prefix_ignore_case(s, "images:").or(strip_prefix_ignore_case(s, "img:")) {
        return image_search_url(query.trim());
    }
    if !looks_like_url(s) {
        return search_url(s);
    }
    if s.starts_with(INTERNAL) || s.contains("://") {
        return s.to_string();
    }
    // A bare hostname means HTTPS, the way it does in any other browser. A
    // site that only speaks plain HTTP is still reachable — see
    // [`plain_fallback`], which the browser tries when the secure attempt
    // cannot connect at all.
    format!("https://{}", s)
}

/// Web search for `query`.
pub fn search_url(query: &str) -> String {
    format!("{}{}", SEARCH, encode(query))
}

/// The same query, sent somewhere that will answer without JavaScript.
pub fn fallback_search_url(query: &str) -> String {
    format!("{}{}", FALLBACK_SEARCH, encode(query))
}

/// The query a search URL was built from, if it is one.
///
/// Used to re-run a search elsewhere when the first engine returns a page we
/// cannot render, and to label the notice with what was actually searched for.
pub fn query_of(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("?")?;
    for pair in rest.split('&') {
        if let Some((name, value)) = pair.split_once('=') {
            if name == "q" || name == "query" {
                let query = decode(value);
                if !query.is_empty() {
                    return Some(query);
                }
            }
        }
    }
    None
}

/// Does this response need a browser we do not have?
///
/// Google answers `/search` with a script that builds the page client-side,
/// wrapped in a `<noscript>` block that redirects to a "turn on JavaScript"
/// notice. That redirect path is the marker, and the check is deliberately
/// narrow: it looks for Google's own retry URL rather than for the presence of
/// a script, because a page that merely uses JavaScript usually renders here
/// perfectly well.
pub fn needs_javascript(body: &str) -> bool {
    body[..body.len().min(8192)].contains("/httpservice/retry/enablejs")
}

/// The plain-HTTP form of a secure URL, for a host that has no TLS at all.
///
/// Only worth trying when the secure attempt failed to connect or handshake;
/// a server that answered and said no is not helped by asking again in clear.
pub fn plain_fallback(url: &str) -> Option<String> {
    url.strip_prefix("https://").map(|rest| format!("http://{}", rest))
}

/// Put a banner at the top of a page fetched from somewhere else.
///
/// The point is that the user is never quietly given something other than what
/// they asked for: if the results below did not come from where the address
/// bar says, the page says so before they read a word of it.
pub fn with_notice(html: &str, notice: &str) -> String {
    let banner = format!(
        "<div style=\"background-color:#1E293B; color:#E2E8F0; padding:8px; \
         margin-bottom:8px\">{}</div>",
        notice
    );
    // Slip it just inside <body> so the page's own styling still applies.
    match html.find("<body") {
        Some(start) => match html[start..].find('>') {
            Some(offset) => {
                let at = start + offset + 1;
                format!("{}{}{}", &html[..at], banner, &html[at..])
            }
            None => format!("{}{}", banner, html),
        },
        None => format!("{}{}", banner, html),
    }
}

// ── Google ──────────────────────────────────────────────────────────────────

/// How many of Google's completions to offer.
const MAX_SUGGESTIONS: usize = 8;

/// Google's brand colours, in the order the letters of the wordmark take them.
const WORDMARK: [(&str, &str); 6] = [
    ("G", "#4285F4"),
    ("o", "#EA4335"),
    ("o", "#FBBC05"),
    ("g", "#4285F4"),
    ("l", "#34A853"),
    ("e", "#EA4335"),
];

/// Where to ask Google what `query` might be short for.
pub fn suggest_url(query: &str) -> String {
    format!("{}{}", SUGGEST, encode(query))
}

/// The completions in a Google Suggest reply.
///
/// The reply is `["typed",["first","second",…],[],{…}]`, so the completions
/// are the strings of the second array and there is no need for a JSON parser
/// to find them: read from the first `,[` to the `]` that closes it and take
/// every quoted string in between.
pub fn suggestions(json: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(open) = json.find(",[") else { return out };
    let rest = &json[open + 2..];
    let end = rest.find(']').unwrap_or(rest.len());

    let mut current: Option<String> = None;
    let mut escaped = false;
    for ch in rest[..end].chars() {
        match current.as_mut() {
            Some(buf) => {
                if escaped {
                    buf.push(ch);
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    let done = current.take().unwrap_or_default();
                    if !done.is_empty() && out.len() < MAX_SUGGESTIONS {
                        out.push(done);
                    }
                } else {
                    buf.push(ch);
                }
            }
            None if ch == '"' => current = Some(String::new()),
            None => {}
        }
    }
    out
}

/// Is this a page of Google's that will arrive empty?
///
/// Google's homepage and its results page are both JavaScript programs rather
/// than documents, so both need the page below in their place. Everything else
/// on the domain — the support pages, for instance — is ordinary HTML and is
/// left alone.
pub fn is_google_page(url: &str) -> bool {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let (host, path) = match after_scheme.find(['/', '?', '#']) {
        Some(at) => (&after_scheme[..at], &after_scheme[at..]),
        None => (after_scheme, "/"),
    };
    let host = host.split(':').next().unwrap_or(host);
    let is_google = host.eq_ignore_ascii_case("google.com")
        || host.to_ascii_lowercase().starts_with("www.google.");
    if !is_google {
        return false;
    }
    matches!(path.split(['?', '#']).next().unwrap_or("/"), "/" | "" | "/search" | "/webhp")
}

/// Google's wordmark, a letter at a time so each keeps its colour.
fn wordmark(size: usize) -> String {
    let mut out = format!("<div style=\"font-size:{}px; margin-bottom:8px\"><b>", size);
    for (letter, color) in WORDMARK {
        out.push_str(&format!("<span style=\"color:{}\">{}</span>", color, letter));
    }
    out.push_str("</b></div>");
    out
}

/// Escape a string for use inside a double-quoted attribute.
///
/// [`escape`] is for text, where a quote is just a quote; in an attribute it
/// ends the value, and everything after it is read as more markup.
fn attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The search box, which submits to Google exactly as Google's own does.
fn search_box(query: &str) -> String {
    format!(
        "<form action=\"https://www.google.com/search\" method=\"get\" \
         style=\"margin-bottom:12px\">\
         <input type=\"search\" name=\"q\" size=\"42\" value=\"{}\"> \
         <input type=\"submit\" value=\"Google Search\">\
         </form>",
        attribute(query),
    )
}

/// Google's completions for `query`, as links that run the search.
fn suggestion_list(suggestions: &[String]) -> String {
    if suggestions.is_empty() {
        return String::new();
    }
    let mut out = String::from("<p><span style=\"color:#5F6368\">Google suggests</span> ");
    for suggestion in suggestions {
        out.push_str(&format!(
            "<a href=\"{}\" style=\"color:#1A0DAB\">{}</a> &middot; ",
            attribute(&search_url(suggestion)),
            escape(suggestion),
        ));
    }
    out.push_str("</p>");
    out
}

/// The header that stands in for Google's chrome above a page of results.
///
/// It carries the wordmark, a box holding the query that was searched for so
/// it can be edited and run again, Google's own completions for it, and the
/// sentence explaining where the results below actually came from.
pub fn google_header(query: &str, suggestions: &[String], source: &str, google_url: &str) -> String {
    format!(
        "<div style=\"background-color:#FFFFFF; color:#202124; padding:8px\">\
         {}{}{}\
         <p style=\"color:#5F6368\">Google answers <b>/search</b> with a JavaScript \
         program rather than results, so these are from {}. \
         <a href=\"{}\" style=\"color:#1A0DAB\">Google's own page anyway</a></p>\
         </div>",
        wordmark(24),
        search_box(query),
        suggestion_list(suggestions),
        escape(source),
        attribute(google_url),
    )
}

/// OS101's Google: the page `www.google.com` shows when Google's own arrives
/// as a program instead of a document.
///
/// Google's homepage is a quarter of a megabyte of script that builds
/// everything you see, so fetching it — which OS101 does, over TLS, from
/// Google — yields a document with nothing in it to draw. This is that page
/// rebuilt from what is actually usable: the wordmark, a box that submits to
/// Google's real search, and Google's real completions underneath it.
pub fn google_page(query: &str, suggestions: &[String]) -> String {
    format!(
        "<html><head><title>Google</title></head>\
         <body style=\"background-color:#FFFFFF; color:#202124; padding:32px; \
         text-align:center\">\
         {}{}{}\
         <p style=\"color:#5F6368\">This page was fetched from Google over TLS. \
         What Google sent is a JavaScript program that assembles the page in the \
         browser, which OS101's script engine cannot run — so the box above is \
         OS101's, and it searches Google. Google will answer a search with a \
         program too, and the browser will say so and show results for the same \
         words from a search engine that still serves HTML.</p>\
         <p style=\"color:#5F6368\">Typing words in the address bar searches \
         Google as well.</p>\
         </body></html>",
        wordmark(32),
        search_box(query),
        suggestion_list(suggestions),
    )
}

/// The browser's own image-search page for `query`.
pub fn image_search_url(query: &str) -> String {
    format!("{}images?q={}", INTERNAL, encode(query))
}

/// Route an image through the proxy so it arrives as a JPEG the kernel can
/// decode, at a size it can afford.
///
/// `width` caps what the proxy sends; passing 0 leaves the size alone.
pub fn image_gateway(url: &str, width: usize) -> String {
    let bare = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let size = if width == 0 {
        String::new()
    } else {
        format!("&w={}", width)
    };
    format!("{}{}{}&output=jpg", IMAGES, encode(bare), size)
}

/// Is this one of the pages the browser writes itself?
pub fn is_internal(url: &str) -> bool {
    url.starts_with(INTERNAL)
}

/// The HTML for an internal page, or None if there is no such page.
pub fn internal_page(url: &str) -> Option<String> {
    let rest = url.strip_prefix(INTERNAL)?;
    let (name, query) = match rest.split_once('?') {
        Some((n, q)) => (n, q),
        None => (rest, ""),
    };
    match name {
        "home" => Some(home_page()),
        "images" => Some(image_page(&param(query, "q"))),
        "scripting" => Some(String::from(SCRIPTING_PAGE)),
        _ => None,
    }
}

/// A page that builds itself.
///
/// Nothing in its markup is content: every row in the table below is written by
/// the script, using the parts of JavaScript a page written this decade actually
/// uses — classes, generators, `async`/`await`, template literals, tagged regular
/// expressions, `BigInt`, `Map`. It is here because it is the one page whose
/// result is the same every time, on a machine with no network, which makes it
/// the honest answer to "does the script engine work" — and because the previous
/// engine could not parse past its first line.
const SCRIPTING_PAGE: &str = include_str!("scripting.html");

/// A photograph for `query`, varied by `lock` and sized to `width` × `height`.
fn photo_url(query: &str, lock: usize, width: usize, height: usize) -> String {
    let tags = if query.is_empty() {
        String::from("wallpaper")
    } else {
        // Spaces become commas: the photo service treats them as separate tags
        // and returns a picture matching all of them.
        query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(",")
    };
    let source = format!("{}/{}/{}/{}?lock={}", PHOTOS, width, height, tags, lock);
    format!("{}{}&output=jpg", IMAGES, encode(&source))
}

fn image_page(query: &str) -> String {
    let shown = if query.is_empty() { "wallpaper" } else { query };
    let mut html = format!(
        "<html><head><title>Images for {}</title></head>\
         <body style=\"background-color:#0F172A; color:#E2E8F0; padding:12px\">\
         <h1 style=\"color:#F8FAFC\">Images for “{}”</h1>\
         <p style=\"color:#94A3B8\">Right-click a picture to save it to \
         <b>/disk/downloads</b> or set it as the desktop wallpaper. \
         Click one to open it full size.</p>",
        escape(shown),
        escape(shown),
    );
    for lock in 1..=RESULTS {
        html.push_str(&format!(
            "<a href=\"{}\"><img src=\"{}\" width=\"288\" height=\"162\" alt=\"{} {}\"></a> ",
            escape(&photo_url(query, lock, 1280, 720)),
            escape(&photo_url(query, lock, 288, 162)),
            escape(shown),
            lock,
        ));
    }
    html.push_str(
        "<p style=\"color:#64748B\">Photographs from Flickr, resized and \
         re-encoded to JPEG by the images.weserv.nl proxy so the kernel's \
         decoders can read them.</p></body></html>",
    );
    html
}

fn home_page() -> String {
    let mut html = String::from(
        "<html><head><title>OS101</title></head>\
         <body style=\"background-color:#0F172A; color:#E2E8F0; padding:16px\">\
         <h1 style=\"color:#38BDF8\">OS101</h1>\
         <p style=\"color:#CBD5E1\">Type words in the address bar to search \
         Google, or an address to go straight there. Prefix a search with \
         <b>images:</b> — or press the <b>Images</b> button — to look for \
         pictures you can save and use as wallpaper.</p>\
         <h3 style=\"color:#F8FAFC\">Somewhere to start</h3><ul>",
    );
    let links: [(&str, &str); 6] = [
        ("https://www.google.com/", "Google"),
        ("https://en.wikipedia.org/wiki/Operating_system", "Wikipedia — Operating system"),
        ("os101:images?q=mountain", "Pictures of mountains"),
        ("http://info.cern.ch/hypertext/WWW/TheProject.html", "The first website ever published"),
        ("https://example.com/", "example.com"),
        ("os101:scripting", "A page that builds itself with JavaScript"),
    ];
    for (href, label) in links {
        html.push_str(&format!("<li><a href=\"{}\">{}</a></li>", href, label));
    }
    html.push_str(
        "</ul><p style=\"color:#64748B\">Pages load over TLS, straight from the \
         site — OS101 speaks HTTPS now. Certificates are not checked, so this \
         hides your traffic from anyone watching but not from whoever runs the \
         network: do not type a password into it.</p></body></html>",
    );
    html
}

/// The value of `name` in a `a=1&b=2` query string, percent-decoded.
fn param(query: &str, name: &str) -> String {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == name {
                return decode(v);
            }
        }
    }
    String::new()
}

/// Percent-encode everything that is not an unreserved URL character. Spaces
/// become `%20` rather than `+`, which every server accepts and which keeps
/// the encoding reversible by [`decode`].
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Reverse [`encode`], leaving anything malformed as written.
pub fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi << 4 | lo);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

pub fn selftest() -> crate::selftest::Report {
    let mut r = crate::selftest::Report::new();

    for host in [
        "example.com",
        "http://example.com",
        "https://en.wikipedia.org/wiki/Cat",
        "info.cern.ch/hypertext/WWW/TheProject.html",
        "localhost",
        "localhost:8000",
        "10.0.2.2:8000/index.html",
        "os101:home",
        "sub.domain.co.uk/a?b=c",
    ] {
        r.check("url recognised", looks_like_url(host));
    }
    for query in [
        "cat",
        "how do i write an operating system",
        "rust no_std",
        "3.5 metres",
        "",
        "  ",
        "hello.",
        "..",
        "a..b.com",
        "host:notaport",
    ] {
        r.check("query not mistaken for a url", !looks_like_url(query));
    }

    r.check("empty goes home", address_to_url("  ") == HOME);
    r.check(
        "words become a google search",
        address_to_url("hello world") == "https://www.google.com/search?q=hello%20world",
    );
    r.check(
        "a bare host gets a secure scheme",
        address_to_url("example.com") == "https://example.com",
    );
    r.check(
        "a full url is left alone",
        address_to_url(" http://example.com/a?b=c ") == "http://example.com/a?b=c",
    );
    r.check(
        "images: prefix searches pictures",
        address_to_url("images: red panda") == "os101:images?q=red%20panda",
    );
    r.check(
        "img: is the same",
        address_to_url("IMG:cat") == address_to_url("images:cat"),
    );

    // A search has to be recoverable from its own URL, or the fallback cannot
    // know what to search for.
    r.check(
        "a query survives the round trip",
        query_of(&search_url("how do i write an os")).as_deref() == Some("how do i write an os"),
    );
    r.check(
        "the fallback carries the same query",
        query_of(&fallback_search_url("red panda")).as_deref() == Some("red panda"),
    );
    r.check("a plain page has no query", query_of("https://example.com/a").is_none());
    r.check("an empty query is not a query", query_of("https://x.com/s?q=").is_none());
    r.check(
        "a query among other parameters is found",
        query_of("https://x.com/s?hl=en&q=cats&n=10").as_deref() == Some("cats"),
    );

    // Recognising Google's script page is what turns a blank screen into an
    // explanation plus results.
    r.check(
        "google's script page is recognised",
        needs_javascript("<html><body><noscript><meta content=\"0;url=/httpservice/retry/enablejs?sei=x\"></noscript>"),
    );
    r.check(
        "an ordinary page is not",
        !needs_javascript("<html><body><script>var a=1</script><h1>Hello</h1></body></html>"),
    );
    r.check("an empty page is not", !needs_javascript(""));

    let noticed = with_notice("<html><body class=\"x\"><p>Hi</p></body></html>", "Heads up");
    r.check("a notice lands inside the body", noticed.contains("<body class=\"x\"><div"));
    r.check("a notice keeps the page", noticed.contains("<p>Hi</p>"));
    r.check("a notice says its piece", noticed.contains("Heads up"));
    r.check(
        "a page with no body still gets the notice",
        with_notice("<p>bare</p>", "Heads up").contains("Heads up"),
    );

    // Google's own pages get OS101's Google in their place; the rest of the
    // domain is ordinary HTML that renders perfectly well.
    for url in [
        "https://www.google.com/",
        "https://www.google.com",
        "http://google.com/",
        "https://www.google.com/search?q=cats",
        "https://www.google.co.uk/search?q=cats",
        "https://www.google.com/webhp?hl=en",
    ] {
        r.check("google's own page is recognised", is_google_page(url));
    }
    for url in [
        "https://support.google.com/websearch/answer/1",
        "https://www.google.com/maps",
        "https://news.google.com/",
        "https://example.com/search?q=cats",
        "https://notgoogle.com/",
        "os101:home",
    ] {
        r.check("other pages are left alone", !is_google_page(url));
    }

    let reply = "[\"hobby op\",[\"hobby operating system\",\"hobby optics\"],[],{\"a\":[]}]";
    let parsed = suggestions(reply);
    r.check("suggestions are read out", parsed.len() == 2);
    r.check("the first suggestion is right", parsed.first().map(|s| s.as_str()) == Some("hobby operating system"));
    r.check("the typed words are not a suggestion", !parsed.iter().any(|s| s == "hobby op"));
    r.check("a reply with no completions is empty", suggestions("[\"x\",[],[],{}]").is_empty());
    r.check("nonsense yields nothing", suggestions("not json at all").is_empty());
    r.check("an empty reply yields nothing", suggestions("").is_empty());
    r.check(
        "an escaped quote survives",
        suggestions("[\"x\",[\"a \\\"b\\\" c\"],[]]").first().map(|s| s.as_str()) == Some("a \"b\" c"),
    );
    r.check(
        "the list is capped",
        suggestions(&{
            let mut s = String::from("[\"x\",[");
            for i in 0..40 {
                s.push_str(&format!("\"s{}\",", i));
            }
            s.push_str("\"last\"],[]]");
            s
        })
        .len()
            == MAX_SUGGESTIONS,
    );
    r.check("suggest asks google", suggest_url("red panda").starts_with(SUGGEST));
    r.check("suggest encodes the query", suggest_url("red panda").ends_with("red%20panda"));

    let page = google_page("cats", &[String::from("cats and dogs")]);
    r.check("google page is titled google", page.contains("<title>Google</title>"));
    r.check("google page is in google's colours", page.contains("#4285F4") && page.contains("#34A853"));
    r.check("google page has a search box", page.contains("name=\"q\""));
    r.check(
        "the box submits to google",
        page.contains("action=\"https://www.google.com/search\""),
    );
    r.check("the box holds the query", page.contains("value=\"cats\""));
    r.check("google's suggestions are offered", page.contains("cats and dogs"));
    r.check(
        "a suggestion links to a search",
        page.contains("https://www.google.com/search?q=cats%20and%20dogs"),
    );
    r.check(
        "an empty query leaves the box empty",
        google_page("", &[]).contains("value=\"\""),
    );
    r.check(
        "a query cannot break out of the box",
        !google_page("\"><script>evil()</script>", &[]).contains("<script>evil()"),
    );
    r.check(
        "a quote cannot close the value attribute",
        !google_page("a\"b", &[]).contains("value=\"a\"b\""),
    );
    r.check("attributes escape quotes", attribute("a\"b") == "a&quot;b");
    r.check("attributes escape markup", attribute("<a>&") == "&lt;a&gt;&amp;");

    let header = google_header("cats", &[], "DuckDuckGo", "https://www.google.com/search?q=cats");
    r.check("the header names the other engine", header.contains("DuckDuckGo"));
    r.check(
        "the header keeps a way back to google",
        header.contains("https://www.google.com/search?q=cats"),
    );
    r.check("the header carries the query", header.contains("value=\"cats\""));

    r.check(
        "a secure url has a plain form",
        plain_fallback("https://example.com/a").as_deref() == Some("http://example.com/a"),
    );
    r.check(
        "a plain url has no fallback",
        plain_fallback("http://example.com/a").is_none(),
    );
    r.check("an internal page has no fallback", plain_fallback("os101:home").is_none());

    let proxied = image_gateway("https://example.com/a b.png", 640);
    r.check("image proxy strips the scheme", !proxied.contains("https%3A"));
    r.check("image proxy passes a width", proxied.contains("&w=640"));
    r.check("image proxy asks for jpeg", proxied.ends_with("&output=jpg"));
    r.check("image proxy encodes spaces", proxied.contains("%20"));
    r.check(
        "no width means no resize",
        !image_gateway("http://example.com/a.png", 0).contains("&w="),
    );

    r.check("internal urls detected", is_internal("os101:images?q=x"));
    r.check("external urls are not", !is_internal("http://example.com"));
    r.check("unknown internal page", internal_page("os101:nowhere").is_none());
    r.check("plain urls have no internal page", internal_page("http://a.com").is_none());

    let home = internal_page("os101:home").unwrap_or_default();
    r.check("home page has a title", home.contains("<title>OS101</title>"));
    r.check("home page links to google", home.contains("https://www.google.com/"));
    r.check("home page warns about certificates", home.contains("not checked"));
    r.check("home page links to the scripting page", home.contains("os101:scripting"));

    // The scripting page is a page whose content is entirely written by its own
    // script, so what its markup has to contain is the script and no answers.
    let scripting = internal_page("os101:scripting").unwrap_or_default();
    r.check("the scripting page exists", !scripting.is_empty());
    r.check("and is mostly script", scripting.contains("<script>"));
    r.check(
        "its results are not in its markup",
        !scripting.contains("checks passed<"),
    );

    let images = internal_page("os101:images?q=red%20panda").unwrap_or_default();
    r.check("image page names the query", images.contains("red panda"));
    r.check(
        "image page has one picture per result",
        images.matches("<img").count() == RESULTS,
    );
    r.check(
        "image page links each thumbnail",
        images.matches("<a href").count() == RESULTS,
    );
    r.check(
        "thumbnails are the small size",
        images.contains("288") && images.contains("1280"),
    );
    r.check(
        "tags are comma separated for the photo service",
        images.contains("red%2Cpanda"),
    );
    r.check(
        "an empty query still finds something",
        internal_page("os101:images?q=")
            .unwrap_or_default()
            .contains("wallpaper"),
    );
    r.check(
        "a quoted query cannot break out of the html",
        !internal_page("os101:images?q=%3Cscript%3E")
            .unwrap_or_default()
            .contains("<script>"),
    );

    r.check("encode leaves unreserved alone", encode("aZ0-_.~") == "aZ0-_.~");
    r.check("encode escapes the rest", encode("a/b c&d") == "a%2Fb%20c%26d");
    r.check("decode reverses encode", decode(&encode("a/b c&d=é")) == "a/b c&d=é");
    r.check("decode accepts plus as space", decode("a+b") == "a b");
    r.check("decode tolerates a stray percent", decode("100%") == "100%");
    r.check("decode tolerates bad hex", decode("%zz") == "%zz");

    r.check(
        "filename from a plain url",
        filename_for("http://example.com/pictures/cat.png", "jpg") == "cat.png",
    );
    r.check(
        "filename gains an extension",
        filename_for("http://example.com/pictures/cat", "jpg") == "cat.jpg",
    );
    r.check(
        "filename ignores a query string",
        filename_for("http://example.com/cat.jpg?w=10", "jpg") == "cat.jpg",
    );
    r.check(
        "filename comes from the proxied address",
        filename_for(&image_gateway("https://example.com/photos/kitten.png", 0), "jpg")
            == "kitten.png",
    );
    r.check(
        "filename skips bare dimensions",
        filename_for(&photo_url("red panda", 3, 1280, 720), "jpg") == "red-panda.jpg",
    );
    r.check(
        "filename always has something to say",
        filename_for("http://example.com/", "jpg") == "example.com.jpg",
    );

    r
}

/// A filename to save `url` under, derived from its last meaningful path
/// segment. Falls back to a generic name, and always ends in an extension.
pub fn filename_for(url: &str, fallback_ext: &str) -> String {
    // A proxied image's own path says nothing useful ("/"), so prefer the
    // address of the picture it was asked to fetch.
    let source = match url.split_once("url=") {
        Some((_, rest)) => decode(rest.split('&').next().unwrap_or(rest)),
        None => url.to_string(),
    };
    let path = source
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(&source);
    let path = path.split(['?', '#']).next().unwrap_or(path);

    let mut stem = String::new();
    for segment in path.split('/').rev() {
        let cleaned: String = segment
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
            .collect();
        let cleaned = cleaned.trim_matches('-').to_string();
        // Skip the bare dimensions in a path like /1280/720/cat.
        if !cleaned.is_empty() && !cleaned.bytes().all(|b| b.is_ascii_digit()) {
            stem = cleaned;
            break;
        }
    }
    if stem.is_empty() {
        stem = String::from("image");
    }
    if stem.len() > 40 {
        stem.truncate(40);
    }

    let known = [".png", ".jpg", ".jpeg", ".gif", ".bmp"];
    let lower = stem.to_ascii_lowercase();
    if known.iter().any(|e| lower.ends_with(e)) {
        stem
    } else {
        format!("{}.{}", stem.trim_end_matches('.'), fallback_ext)
    }
}
