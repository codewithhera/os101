//! An HTTP/1.1 client, over TCP or TLS.
//!
//! `https://` runs the same request through [`super::tls`] instead of straight
//! down the socket. Everything above that — redirects, headers, chunked
//! bodies — is identical, because TLS is a stream like any other once the
//! handshake is done.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::{dns, tcp, tls, TICKS_PER_SEC};

/// What OS101 tells servers it is.
///
/// It would be nicer to send just `OS101/0.1`. In practice a server that does
/// not recognise the client increasingly serves it an "unsupported browser"
/// page instead of the site — Google does exactly that — so this leads with
/// the tokens those checks look for and names OS101 within them. It is not a
/// lie about what we are; it is the shape of string the modern web insists on.
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; OS101 x86_64) AppleWebKit/537.36 (KHTML, like Gecko) OS101/0.1 Safari/537.36";

/// What this browser calls itself, for a page's `navigator.userAgent` — the same
/// string the requests carry, since a page branching on it should see what the
/// server saw.
pub fn user_agent() -> &'static str {
    USER_AGENT
}

/// How many redirects to follow before giving up.
const MAX_REDIRECTS: usize = 5;
/// Cap on a response body, to bound memory use. Generous enough for a
/// photograph — a 1280×720 JPEG runs to about 300 KB — because the browser
/// fetches images through this same path.
pub const MAX_BODY: usize = 1024 * 1024;

pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
    /// Whether to wrap the connection in TLS.
    pub secure: bool,
}

impl Url {
    /// Parse an absolute URL. A bare `host/path` is accepted and assumed to be
    /// HTTPS, because that is what a bare hostname means on today's web — and
    /// a server that only speaks HTTP will redirect us back down.
    pub fn parse(input: &str) -> Result<Url, &'static str> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("empty URL");
        }

        let (rest, secure) = match trimmed.strip_prefix("https://") {
            Some(rest) => (rest, true),
            None => match trimmed.strip_prefix("http://") {
                Some(rest) => (rest, false),
                None => (trimmed, true),
            },
        };

        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err("the URL has no host");
        }

        let default_port = if secure { 443 } else { 80 };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "invalid port in URL")?),
            None => (authority, default_port),
        };

        Ok(Url {
            host: host.to_string(),
            port,
            path: path.to_string(),
            secure,
        })
    }

    pub fn to_absolute(&self) -> String {
        let scheme = if self.secure { "https" } else { "http" };
        let default_port = if self.secure { 443 } else { 80 };
        if self.port == default_port {
            alloc::format!("{}://{}{}", scheme, self.host, self.path)
        } else {
            alloc::format!("{}://{}:{}{}", scheme, self.host, self.port, self.path)
        }
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// The URL the body actually came from, after any redirects.
    pub final_url: String,
}

impl Response {
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Fetch a URL, following redirects.
pub fn get(url: &str) -> Result<Response, String> {
    let mut current = Url::parse(url).map_err(|e| e.to_string())?;

    for _ in 0..MAX_REDIRECTS {
        let response = fetch_once(&current)?;

        // 3xx with a Location header means go again.
        if (300..400).contains(&response.status) {
            if let Some(location) = &response.redirect {
                let next = resolve_redirect(&current, location)?;
                current = next;
                continue;
            }
        }

        return Ok(Response {
            status: response.status,
            content_type: response.content_type,
            body: response.body,
            final_url: current.to_absolute(),
        });
    }

    Err("too many redirects".to_string())
}

struct RawResponse {
    status: u16,
    content_type: String,
    redirect: Option<String>,
    body: Vec<u8>,
}

/// Turn a `Location` header into a URL, handling the relative forms.
fn resolve_redirect(base: &Url, location: &str) -> Result<Url, String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return Url::parse(location).map_err(|e| e.to_string());
    }
    // A protocol-relative `//host/path` keeps the scheme it was found under.
    if let Some(stripped) = location.strip_prefix("//") {
        let scheme = if base.secure { "https" } else { "http" };
        return Url::parse(&alloc::format!("{}://{}", scheme, stripped)).map_err(|e| e.to_string());
    }
    let path = if location.starts_with('/') {
        location.to_string()
    } else {
        // Relative to the directory part of the current path.
        let dir = match base.path.rfind('/') {
            Some(i) => &base.path[..=i],
            None => "/",
        };
        alloc::format!("{}{}", dir, location)
    };
    Ok(Url {
        host: base.host.clone(),
        port: base.port,
        path,
        secure: base.secure,
    })
}

fn request_text(url: &Url) -> String {
    // `Connection: close` means the server closes when the body is done,
    // which is how we know we have all of it without chunked decoding.
    //
    // `Accept-Encoding: identity` because OS101 has no gzip decoder, and a
    // server that compresses the body would leave us rendering binary.
    alloc::format!(
        "GET {} HTTP/1.1\r\n\
         Host: {}\r\n\
         User-Agent: {}\r\n\
         Accept: text/html,application/xhtml+xml,text/plain,image/*,*/*;q=0.8\r\n\
         Accept-Language: en-US,en;q=0.9\r\n\
         Accept-Encoding: identity\r\n\
         Connection: close\r\n\
         \r\n",
        url.path, url.host, USER_AGENT
    )
}

fn fetch_once(url: &Url) -> Result<RawResponse, String> {
    let addr = dns::resolve(&url.host).map_err(|e| e.to_string())?;
    let request = request_text(url);

    let raw = if url.secure {
        fetch_over_tls(addr, url, request.as_bytes())?
    } else {
        fetch_over_tcp(addr, url, request.as_bytes())?
    };

    if raw.is_empty() {
        return Err("the server closed the connection without replying".to_string());
    }
    parse_response(&raw)
}

fn fetch_over_tcp(addr: super::ip::Ipv4Addr, url: &Url, request: &[u8]) -> Result<Vec<u8>, String> {
    tcp::connect(addr, url.port).map_err(|e| e.to_string())?;

    if let Err(e) = tcp::send(request) {
        tcp::close();
        return Err(e.to_string());
    }

    let raw = tcp::recv_to_end(20 * TICKS_PER_SEC).map_err(|e| {
        tcp::close();
        e.to_string()
    })?;
    tcp::close();
    Ok(raw)
}

fn fetch_over_tls(addr: super::ip::Ipv4Addr, url: &Url, request: &[u8]) -> Result<Vec<u8>, String> {
    let mut stream = tls::connect(addr, url.port, &url.host)?;

    if let Err(e) = stream.send(request) {
        stream.close();
        return Err(e);
    }

    let raw = match stream.recv_to_end(25 * TICKS_PER_SEC) {
        Ok(raw) => raw,
        Err(e) => {
            stream.close();
            return Err(e);
        }
    };
    stream.close();
    Ok(raw)
}

fn parse_response(raw: &[u8]) -> Result<RawResponse, String> {
    // Headers end at the first blank line.
    let split = find_subslice(raw, b"\r\n\r\n")
        .map(|i| (i, i + 4))
        .or_else(|| find_subslice(raw, b"\n\n").map(|i| (i, i + 2)))
        .ok_or_else(|| "malformed response: no header terminator".to_string())?;

    let head = String::from_utf8_lossy(&raw[..split.0]).into_owned();
    let mut body = raw[split.1..].to_vec();

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| alloc::format!("malformed status line: {}", status_line))?;

    let mut content_type = String::new();
    let mut redirect = None;
    let mut chunked = false;

    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue };
        let value = value.trim();
        // Header names are case-insensitive.
        let name = name.trim().to_ascii_lowercase();
        match name.as_str() {
            "content-type" => content_type = value.to_string(),
            "location" => redirect = Some(value.to_string()),
            "transfer-encoding" if value.eq_ignore_ascii_case("chunked") => chunked = true,
            _ => {}
        }
    }

    if chunked {
        body = decode_chunked(&body);
    }
    body.truncate(MAX_BODY);

    Ok(RawResponse { status, content_type, redirect, body })
}

/// Undo `Transfer-Encoding: chunked`.
pub(crate) fn decode_chunked(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;

    while pos < body.len() {
        let Some(eol) = find_subslice(&body[pos..], b"\r\n") else { break };
        let size_line = &body[pos..pos + eol];
        // The size may be followed by chunk extensions after a ';'.
        let size_str = core::str::from_utf8(size_line).unwrap_or("").trim();
        let size_str = size_str.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_str, 16) else { break };

        pos += eol + 2;
        if size == 0 {
            break;
        }
        let end = (pos + size).min(body.len());
        out.extend_from_slice(&body[pos..end]);
        pos = end + 2; // step over the chunk's trailing CRLF
    }

    out
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
