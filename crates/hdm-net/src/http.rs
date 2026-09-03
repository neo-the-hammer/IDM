//! HTTP/1.1 requests and streaming responses.

use crate::headers::Headers;
use crate::stream::Stream;
use crate::url::{percent_decode_str, Url};
use std::io::{self, BufRead, BufReader, Read, Write};

/// Caps on what a server may send us before we give up.
///
/// A download manager talks to servers chosen by whoever sent the link, so a
/// hostile or broken one must not be able to make us allocate without bound.
const MAX_LINE: usize = 16 * 1024;
const MAX_HEADER_BYTES: usize = 256 * 1024;
const MAX_HEADERS: usize = 200;
/// Redirect chains longer than this are treated as a loop.
pub const MAX_REDIRECTS: usize = 20;

pub const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (compatible; HydraDM/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/neo-the-hammer/IDM)"
);

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Headers,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn get(url: Url) -> Request {
        Request {
            method: "GET".into(),
            url,
            headers: Headers::new(),
            body: None,
        }
    }

    pub fn head(url: Url) -> Request {
        Request {
            method: "HEAD".into(),
            url,
            headers: Headers::new(),
            body: None,
        }
    }

    /// Requests bytes `start..=end`, or `start..` when `end` is `None`.
    pub fn with_range(mut self, start: u64, end: Option<u64>) -> Request {
        let value = match end {
            Some(e) => format!("bytes={start}-{e}"),
            None => format!("bytes={start}-"),
        };
        self.headers.set("Range", value);
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Request {
        self.headers.set(name, value);
        self
    }

    /// Fills in the headers every request needs, without overriding anything
    /// the caller (or the browser extension) already set.
    fn apply_defaults(&mut self, keep_alive: bool, via_proxy: bool) {
        self.headers.set("Host", self.url.host_header());
        self.headers.set_if_absent("User-Agent", USER_AGENT);
        self.headers.set_if_absent("Accept", "*/*");
        // Identity encoding is deliberate. Segmented downloading needs byte
        // offsets in the response to match byte offsets in the file, and a
        // compressed transfer breaks that correspondence entirely.
        self.headers.set("Accept-Encoding", "identity");
        let connection = if keep_alive { "keep-alive" } else { "close" };
        if via_proxy {
            self.headers.set("Proxy-Connection", connection);
        }
        self.headers.set("Connection", connection);
        match &self.body {
            Some(b) => self.headers.set("Content-Length", b.len().to_string()),
            None => {
                if matches!(self.method.as_str(), "POST" | "PUT" | "PATCH") {
                    self.headers.set("Content-Length", "0");
                }
            }
        }
    }

    /// Serializes and sends the request.
    ///
    /// `absolute_target` is set when talking to a forward proxy, which expects
    /// the full URL on the request line rather than just the path.
    pub fn write_to(
        &mut self,
        out: &mut Stream,
        keep_alive: bool,
        absolute_target: bool,
    ) -> io::Result<()> {
        self.apply_defaults(keep_alive, absolute_target);
        let target = if absolute_target {
            self.url.to_string_safe()
        } else {
            self.url.request_target()
        };
        let mut head = format!("{} {} HTTP/1.1\r\n", self.method, target);
        self.headers.write_to(&mut head);
        out.write_all(head.as_bytes())?;
        if let Some(body) = &self.body {
            out.write_all(body)?;
        }
        out.flush()
    }
}

/// A response whose body has not been read yet.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Headers,
    pub body: Body,
}

impl Response {
    /// Reads the status line and headers, skipping any 1xx informational
    /// responses (`100 Continue`, `103 Early Hints`) that precede the real one.
    pub fn read(stream: Stream, method: &str) -> io::Result<Response> {
        let mut reader = BufReader::with_capacity(16 * 1024, stream);

        let (status, reason, headers) = loop {
            let (status, reason) = read_status_line(&mut reader)?;
            let headers = read_headers(&mut reader)?;
            if (100..200).contains(&status) {
                continue;
            }
            break (status, reason, headers);
        };

        let mode = BodyMode::detect(status, method, &headers)?;
        Ok(Response {
            status,
            reason,
            headers,
            body: Body { reader, mode },
        })
    }

    /// `Content-Length`, when the server stated one.
    pub fn content_length(&self) -> Option<u64> {
        self.headers.get("Content-Length")?.trim().parse().ok()
    }

    /// The total size of the resource, taking `Content-Range` into account for
    /// a 206 response, where `Content-Length` is only the length of the slice.
    pub fn total_size(&self) -> Option<u64> {
        if self.status == 206 {
            if let Some(range) = self.headers.get("Content-Range") {
                if let Some(total) = parse_content_range_total(range) {
                    return Some(total);
                }
            }
        }
        self.content_length()
    }

    /// Whether the server honoured a `Range` request.
    pub fn is_partial(&self) -> bool {
        self.status == 206
    }

    /// Whether the server advertises byte-range support, which is what makes
    /// segmentation and resume possible.
    pub fn accepts_ranges(&self) -> bool {
        self.headers
            .get("Accept-Ranges")
            .map(|v| !v.eq_ignore_ascii_case("none"))
            .unwrap_or(false)
    }

    pub fn etag(&self) -> Option<String> {
        self.headers.get("ETag").map(|s| s.trim().to_string())
    }

    pub fn last_modified(&self) -> Option<String> {
        self.headers
            .get("Last-Modified")
            .map(|s| s.trim().to_string())
    }

    pub fn content_type(&self) -> Option<&str> {
        self.headers
            .get("Content-Type")
            .map(|v| v.split(';').next().unwrap_or(v).trim())
    }

    /// The `Location` of a redirect, resolved against the request URL.
    pub fn location(&self, base: &Url) -> Option<Url> {
        base.join(self.headers.get("Location")?).ok()
    }

    pub fn is_redirect(&self) -> bool {
        matches!(self.status, 301 | 302 | 303 | 307 | 308)
    }

    /// The filename to save as: `Content-Disposition` if present and usable,
    /// otherwise the last path segment of the URL.
    pub fn filename(&self, url: &Url) -> Option<String> {
        self.headers
            .get("Content-Disposition")
            .and_then(parse_content_disposition_filename)
            .or_else(|| url.filename())
            .map(|name| sanitize_filename(&name))
            .filter(|name| !name.is_empty())
    }

    /// Reads the whole body into memory, refusing anything over `limit`.
    /// Only for small documents — HTML pages for the spider, API replies.
    pub fn read_to_vec(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = self.body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if out.len() + n > limit {
                return Err(io::Error::other(format!("response exceeds {limit} bytes")));
            }
            out.extend_from_slice(&buf[..n]);
        }
        Ok(out)
    }

    pub fn read_to_string(&mut self, limit: usize) -> io::Result<String> {
        Ok(String::from_utf8_lossy(&self.read_to_vec(limit)?).into_owned())
    }

    /// Closes the underlying connection immediately, unblocking any read.
    pub fn shutdown(&self) {
        self.body.reader.get_ref().shutdown();
    }
}

/// How the end of the body is determined.
enum BodyMode {
    /// No body at all: HEAD, 204, 304, or 1xx.
    Empty,
    /// `Content-Length` bytes remain.
    Length(u64),
    Chunked(ChunkState),
    /// No framing: the body ends when the connection closes.
    UntilEof,
}

enum ChunkState {
    NeedSize,
    InChunk(u64),
    Trailers,
    Done,
}

impl BodyMode {
    fn detect(status: u16, method: &str, headers: &Headers) -> io::Result<BodyMode> {
        // RFC 7230 section 3.3.3.
        if method.eq_ignore_ascii_case("HEAD")
            || status == 204
            || status == 304
            || (100..200).contains(&status)
        {
            return Ok(BodyMode::Empty);
        }
        if let Some(te) = headers.get("Transfer-Encoding") {
            // Only the final encoding matters, and `chunked` must be last.
            if te
                .rsplit(',')
                .next()
                .map(str::trim)
                .unwrap_or("")
                .eq_ignore_ascii_case("chunked")
            {
                return Ok(BodyMode::Chunked(ChunkState::NeedSize));
            }
            return Err(io::Error::other(format!(
                "unsupported Transfer-Encoding: {te}"
            )));
        }
        // Conflicting Content-Length values are a request-smuggling signature;
        // refuse rather than pick one.
        let mut lengths = headers.get_all("Content-Length");
        if let Some(first) = lengths.next() {
            let value: u64 = first
                .trim()
                .parse()
                .map_err(|_| io::Error::other(format!("bad Content-Length: {first:?}")))?;
            for other in lengths {
                if other.trim().parse::<u64>().ok() != Some(value) {
                    return Err(io::Error::other("conflicting Content-Length headers"));
                }
            }
            return Ok(BodyMode::Length(value));
        }
        Ok(BodyMode::UntilEof)
    }
}

/// The response body, as a `Read`.
pub struct Body {
    reader: BufReader<Stream>,
    mode: BodyMode,
}

impl Body {
    /// Whether the body has a known length that has been fully consumed.
    /// Used to decide whether a connection may be reused.
    pub fn is_complete(&self) -> bool {
        matches!(
            self.mode,
            BodyMode::Empty | BodyMode::Length(0) | BodyMode::Chunked(ChunkState::Done)
        )
    }

    /// Recovers the connection for reuse, if the body was framed and finished.
    pub fn into_stream(self) -> Option<Stream> {
        self.is_complete().then(|| self.reader.into_inner())
    }
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Body")
            .field("complete", &self.is_complete())
            .finish()
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.mode {
            BodyMode::Empty => Ok(0),
            BodyMode::Length(remaining) => {
                if *remaining == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(*remaining as usize);
                let n = self.reader.read(&mut buf[..want])?;
                if n == 0 {
                    // The socket closed with bytes still outstanding. Saying so
                    // explicitly is what lets the engine retry the segment
                    // instead of writing a short file.
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("connection closed with {remaining} bytes still expected"),
                    ));
                }
                *remaining -= n as u64;
                Ok(n)
            }
            BodyMode::UntilEof => self.reader.read(buf),
            BodyMode::Chunked(_) => self.read_chunked(buf),
        }
    }
}

impl Body {
    fn read_chunked(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let BodyMode::Chunked(state) = &mut self.mode else {
                unreachable!("read_chunked is only called in chunked mode")
            };
            match state {
                ChunkState::Done => return Ok(0),
                ChunkState::NeedSize => {
                    let line = read_line(&mut self.reader)?;
                    // A chunk-size line may carry extensions after a ';'.
                    let size_text = line.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(size_text, 16)
                        .map_err(|_| io::Error::other(format!("bad chunk size: {size_text:?}")))?;
                    *state = if size == 0 {
                        ChunkState::Trailers
                    } else {
                        ChunkState::InChunk(size)
                    };
                }
                ChunkState::InChunk(remaining) => {
                    let want = buf.len().min(*remaining as usize);
                    let n = self.reader.read(&mut buf[..want])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "connection closed mid-chunk",
                        ));
                    }
                    *remaining -= n as u64;
                    if *remaining == 0 {
                        // Consume the CRLF that terminates the chunk data.
                        let trailing = read_line(&mut self.reader)?;
                        if !trailing.is_empty() {
                            return Err(io::Error::other("chunk not terminated by CRLF"));
                        }
                        *state = ChunkState::NeedSize;
                    }
                    return Ok(n);
                }
                ChunkState::Trailers => {
                    // Discard trailer headers up to the blank line.
                    let mut count = 0;
                    loop {
                        let line = read_line(&mut self.reader)?;
                        if line.is_empty() {
                            break;
                        }
                        count += 1;
                        if count > MAX_HEADERS {
                            return Err(io::Error::other("too many trailer headers"));
                        }
                    }
                    *state = ChunkState::Done;
                    return Ok(0);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ parsing

/// Reads one CRLF-terminated line, without the terminator.
fn read_line(reader: &mut BufReader<Stream>) -> io::Result<String> {
    let mut raw = Vec::new();
    let mut limited = reader.take(MAX_LINE as u64);
    let n = limited.read_until(b'\n', &mut raw)?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed mid-header",
        ));
    }
    if raw.last() != Some(&b'\n') {
        return Err(io::Error::other(format!(
            "header line longer than {MAX_LINE} bytes"
        )));
    }
    raw.pop();
    if raw.last() == Some(&b'\r') {
        raw.pop();
    }
    // Headers are ASCII by specification; lossy conversion keeps a server that
    // sends Latin-1 in a header from failing the whole download.
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

fn read_status_line(reader: &mut BufReader<Stream>) -> io::Result<(u16, String)> {
    let line = read_line(reader)?;
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(io::Error::other(format!("not an HTTP response: {line:?}")));
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other(format!("bad status line: {line:?}")))?;
    if !(100..=599).contains(&status) {
        return Err(io::Error::other(format!(
            "status code out of range: {status}"
        )));
    }
    Ok((status, parts.next().unwrap_or("").to_string()))
}

fn read_headers(reader: &mut BufReader<Stream>) -> io::Result<Headers> {
    let mut headers = Headers::new();
    let mut total = 0usize;
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(headers);
        }
        total += line.len();
        if total > MAX_HEADER_BYTES {
            return Err(io::Error::other("response headers are too large"));
        }
        if headers.len() >= MAX_HEADERS {
            return Err(io::Error::other("too many response headers"));
        }
        // Obsolete line folding: a continuation line begins with whitespace.
        // RFC 7230 has it deprecated; we accept it but flatten it to a space.
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        headers
            .parse_line(&line)
            .map_err(|e| io::Error::other(format!("malformed response header: {e}")))?;
    }
}

/// Extracts the total size from `Content-Range: bytes 0-99/12345`.
/// A `*` total means the server does not know it.
pub fn parse_content_range_total(value: &str) -> Option<u64> {
    let total = value.rsplit('/').next()?.trim();
    if total == "*" {
        return None;
    }
    total.parse().ok()
}

/// Extracts the byte range from `Content-Range: bytes 100-199/12345`.
pub fn parse_content_range_span(value: &str) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes")?.trim();
    let range = spec.split('/').next()?.trim();
    let (start, end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?))
}

/// Parses a filename out of `Content-Disposition` (RFC 6266).
///
/// `filename*` wins over `filename` when both are present, because it carries
/// an explicit charset and can represent non-Latin names correctly.
pub fn parse_content_disposition_filename(value: &str) -> Option<String> {
    let mut plain: Option<String> = None;

    for part in split_header_params(value) {
        // The first parameter is the disposition type ("attachment") and has
        // no `=`; skip anything unparseable rather than abandoning the header.
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let raw = raw.trim();

        if key == "filename*" {
            // ext-value: charset '' language 'pct-encoded (RFC 5987)
            let mut fields = raw.splitn(3, '\'');
            let (Some(charset), Some(_language), Some(encoded)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let charset = charset.trim().to_ascii_lowercase();
            let bytes = crate::url::percent_decode(encoded);
            let decoded = match charset.as_str() {
                "utf-8" | "utf8" => String::from_utf8_lossy(&bytes).into_owned(),
                // ISO-8859-1 maps byte-for-byte onto the first 256 code points.
                "iso-8859-1" | "latin1" => bytes.iter().map(|&b| b as char).collect(),
                _ => continue,
            };
            if !decoded.is_empty() {
                return Some(decoded);
            }
        } else if key == "filename" && plain.is_none() {
            let unquoted = unquote(raw);
            if !unquoted.is_empty() {
                plain = Some(percent_decode_str(&unquoted));
            }
        }
    }
    plain
}

/// Splits a header value on `;`, ignoring separators inside quoted strings.
fn split_header_params(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut escaped = false;
    for c in value.chars() {
        match c {
            '\\' if in_quotes && !escaped => {
                escaped = true;
                current.push(c);
            }
            '"' if !escaped => {
                in_quotes = !in_quotes;
                current.push(c);
            }
            ';' if !in_quotes => {
                parts.push(std::mem::take(&mut current));
            }
            c => {
                escaped = false;
                current.push(c);
            }
        }
    }
    parts.push(current);
    parts
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let inner = &trimmed[1..trimmed.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut escaped = false;
        for c in inner.chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        trimmed.to_string()
    }
}

/// Reduces a server-supplied name to something safe to create on disk.
///
/// The name in `Content-Disposition` is attacker-controlled whenever the link
/// is, so path separators, parent references, NUL, and Windows reserved device
/// names all have to go. Without this, `filename="../../.bashrc"` would write
/// outside the download directory.
pub fn sanitize_filename(name: &str) -> String {
    // Take the last path segment under either separator.
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);

    let mut cleaned: String = base
        .chars()
        .map(|c| match c {
            // Illegal on Windows, and control characters everywhere.
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\' => '_',
            c if (c as u32) < 0x20 || c as u32 == 0x7f => '_',
            c => c,
        })
        .collect();

    // Windows strips trailing dots and spaces, which can turn "a. " into "a".
    cleaned = cleaned
        .trim_matches(|c: char| c == ' ' || c == '.')
        .to_string();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return String::new();
    }

    // Reserved DOS device names, with or without an extension.
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        cleaned.insert(0, '_');
    }

    // Most filesystems cap a name at 255 bytes; truncate on a char boundary
    // and keep the extension, which is what makes the file openable.
    if cleaned.len() > 200 {
        let ext = cleaned
            .rsplit_once('.')
            .map(|(_, e)| e)
            .filter(|e| e.len() <= 16)
            .unwrap_or("")
            .to_string();
        let keep = 200 - ext.len() - if ext.is_empty() { 0 } else { 1 };
        let mut stem: String = cleaned.chars().collect::<Vec<_>>().into_iter().collect();
        while stem.len() > keep {
            stem.pop();
        }
        cleaned = if ext.is_empty() {
            stem
        } else {
            format!("{stem}.{ext}")
        };
    }
    cleaned
}
