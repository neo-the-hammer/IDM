//! The local HTTP server: REST, WebSocket, and the web UI.
//!
//! It listens on the loopback interface only. Everything it exposes — starting
//! downloads, reading settings, writing files anywhere the user can — would be
//! a serious hole if it were reachable from the network or from a web page, so
//! the access rules are enforced here rather than being left to callers.

use crate::routes;
use crate::websocket;
use hdm_core::manager::Manager;
use hdm_crypto::constant_time_eq;
use hdm_json::{json, Json};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Caps on what a client may send.
const MAX_REQUEST_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 100;
const MAX_BODY: usize = 4 * 1024 * 1024;
/// Ceiling on concurrent connections, so a runaway client cannot exhaust threads.
const MAX_CONNECTIONS: usize = 128;
/// How often the event stream pushes a snapshot.
const EVENT_INTERVAL: Duration = Duration::from_millis(500);

pub struct ApiServer {
    pub manager: Arc<Manager>,
    /// The bearer token every request must carry.
    pub token: String,
    /// Directory holding the built web UI, if it is being served.
    pub ui_dir: Option<PathBuf>,
    /// Extra origins allowed through CORS, beyond the built-in local ones.
    pub extra_origins: Vec<String>,
    pub version: String,
}

/// A running server.
pub struct Bound {
    pub addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl Bound {
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Release);
        // Unblock the accept loop so it notices.
        let _ = TcpStream::connect(self.addr);
    }
}

impl ApiServer {
    /// Binds to loopback on `port` (0 for any free port) and serves until stopped.
    pub fn start(self, port: u16) -> io::Result<(Bound, std::thread::JoinHandle<()>)> {
        // Loopback only. Binding to 0.0.0.0 would expose an unauthenticated-by-
        // default control surface to the whole network.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let server = Arc::new(self);

        let handle = {
            let shutdown = shutdown.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    if connections.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                        // Shed load rather than spawn without bound.
                        drop(stream);
                        continue;
                    }
                    connections.fetch_add(1, Ordering::AcqRel);
                    let server = server.clone();
                    let connections = connections.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
                        server.handle_connection(stream);
                        connections.fetch_sub(1, Ordering::AcqRel);
                    });
                }
            })
        };
        Ok((Bound { addr, shutdown }, handle))
    }

    fn handle_connection(&self, mut stream: TcpStream) {
        let Some(request) = read_request(&mut stream) else {
            let _ = write_response(&mut stream, 400, "text/plain", b"bad request", None);
            return;
        };

        // A hostname that resolves to 127.0.0.1 lets a remote page reach this
        // server while keeping its own origin -- the DNS rebinding attack. The
        // Origin check below stops the scripted case; requiring a loopback Host
        // stops it before routing, including for navigations that send no
        // Origin at all.
        if !host_is_loopback(request.header("Host")) {
            let _ = write_response(
                &mut stream,
                403,
                "application/json",
                br#"{"error":"unrecognized Host header"}"#,
                None,
            );
            return;
        }

        // A browser page on another origin must not be able to drive the
        // daemon, so cross-origin requests are refused before anything else.
        if let Some(origin) = request.header("Origin") {
            if !self.origin_allowed(origin) {
                let _ = write_response(
                    &mut stream,
                    403,
                    "application/json",
                    br#"{"error":"origin not allowed"}"#,
                    None,
                );
                return;
            }
        }

        if request.method == "OPTIONS" {
            let _ = write_preflight(&mut stream, request.header("Origin"));
            return;
        }

        // The event stream is a WebSocket upgrade rather than a normal reply.
        if request.path == "/api/v1/events" {
            if !self.authorized(&request) {
                let _ = write_response(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#,
                    None,
                );
                return;
            }
            self.serve_events(stream, &request);
            return;
        }

        if request.path.starts_with("/api/") {
            if !self.authorized(&request) {
                let _ = write_response(
                    &mut stream,
                    401,
                    "application/json",
                    br#"{"error":"unauthorized"}"#,
                    None,
                );
                return;
            }
            let (status, body) = routes::dispatch(&self.manager, &request, &self.version);
            let _ = write_response(
                &mut stream,
                status,
                "application/json",
                body.to_string_compact().as_bytes(),
                request.header("Origin"),
            );
            return;
        }

        self.serve_ui(&mut stream, &request);
    }

    /// Checks the bearer token.
    ///
    /// The token may also arrive as a query parameter, because a browser cannot
    /// set headers on a WebSocket handshake. That is the only reason it is
    /// accepted there.
    fn authorized(&self, request: &HttpRequest) -> bool {
        let presented = request
            .header("Authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string)
            .or_else(|| request.query("token"));

        match presented {
            // Compared in constant time so a local attacker cannot recover the
            // token a byte at a time by timing rejections.
            Some(value) => constant_time_eq(value.as_bytes(), self.token.as_bytes()),
            None => false,
        }
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        // Browser extensions and the local UI itself.
        if origin.starts_with("chrome-extension://")
            || origin.starts_with("moz-extension://")
            || origin.starts_with("safari-web-extension://")
        {
            return true;
        }
        if let Ok(url) = hdm_net::url::Url::parse(origin) {
            if matches!(url.host.as_str(), "127.0.0.1" | "localhost" | "::1") {
                return true;
            }
        }
        self.extra_origins.iter().any(|allowed| allowed == origin)
    }

    /// Upgrades to a WebSocket and pushes progress snapshots.
    fn serve_events(&self, mut stream: TcpStream, request: &HttpRequest) {
        let Some(key) = request.header("Sec-WebSocket-Key") else {
            let _ = write_response(&mut stream, 400, "text/plain", b"not a websocket", None);
            return;
        };
        let accept = websocket::accept_key(key);
        let handshake = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if stream.write_all(handshake.as_bytes()).is_err() {
            return;
        }
        let _ = stream.set_read_timeout(None);
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));

        // A reader thread watches for the client going away. Without it the
        // writer would only notice when a send finally failed, which on a
        // half-open connection can take minutes.
        let closed = Arc::new(AtomicBool::new(false));
        if let Ok(mut reader) = stream.try_clone() {
            let closed = closed.clone();
            let mut sink = match stream.try_clone() {
                Ok(s) => s,
                Err(_) => return,
            };
            std::thread::spawn(move || loop {
                match websocket::read_frame(&mut reader, &mut sink) {
                    Ok(websocket::Incoming::Closed) | Err(_) => {
                        closed.store(true, Ordering::Release);
                        return;
                    }
                    Ok(_) => {}
                }
            });
        }

        let mut last_payload = String::new();
        while !closed.load(Ordering::Acquire) {
            let snapshot = json!({
                "type": "snapshot",
                "downloads": (Json::Arr(self.manager.snapshot())),
                "totals": (self.manager.totals())
            })
            .to_string_compact();

            // Only send when something actually changed. An idle Hydra should
            // not wake the browser twice a second for nothing.
            if snapshot != last_payload {
                if websocket::write_text(&mut stream, &snapshot).is_err() {
                    break;
                }
                last_payload = snapshot;
            }
            std::thread::sleep(EVENT_INTERVAL);
        }
        let _ = websocket::write_close(&mut stream);
    }

    /// Serves the built web UI.
    fn serve_ui(&self, stream: &mut TcpStream, request: &HttpRequest) {
        let Some(root) = &self.ui_dir else {
            let _ = write_response(
                stream,
                404,
                "text/plain",
                b"the web UI is not installed in this build",
                None,
            );
            return;
        };

        let relative = request.path.trim_start_matches('/');
        let relative = if relative.is_empty() {
            "index.html"
        } else {
            relative
        };

        let Some(path) = safe_join(root, relative) else {
            let _ = write_response(stream, 403, "text/plain", b"forbidden", None);
            return;
        };

        // Unknown paths fall back to index.html so the UI can own its routing.
        let path = if path.is_file() {
            path
        } else {
            root.join("index.html")
        };

        match std::fs::read(&path) {
            Ok(body) => {
                let body = if path.extension().and_then(|e| e.to_str()) == Some("html") {
                    inject_token(&body, &self.token)
                } else {
                    body
                };
                let _ = write_response(stream, 200, mime_for(&path), &body, None);
            }
            Err(_) => {
                let _ = write_response(stream, 404, "text/plain", b"not found", None);
            }
        }
    }
}

/// Stamps the API token into the served page so the UI can authenticate.
///
/// This is safe because the page is only ever served over loopback to a
/// request whose Host and Origin have already been checked: a remote page
/// cannot read the response cross-origin, and a local process could read the
/// token file directly in any case.
fn inject_token(html: &[u8], token: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    let escaped = token
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;");
    let tag = format!("<meta name=\"hydra-token\" content=\"{escaped}\" />\n    ");
    match text.find("<title>") {
        Some(index) => {
            let mut out = String::with_capacity(text.len() + tag.len());
            out.push_str(&text[..index]);
            out.push_str(&tag);
            out.push_str(&text[index..]);
            out.into_bytes()
        }
        // No <title> to anchor to; the UI falls back to ?token=.
        None => html.to_vec(),
    }
}

/// Accepts only Host values naming the loopback interface.
fn host_is_loopback(host: Option<&str>) -> bool {
    let Some(host) = host else {
        // HTTP/1.1 requires a Host header; a request without one is malformed.
        return false;
    };
    // Strip the port, taking care with a bracketed IPv6 literal.
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    matches!(name, "127.0.0.1" | "localhost" | "::1" | "[::1]") || name.starts_with("127.")
}

/// Resolves `relative` under `root`, refusing anything that escapes it.
///
/// Without this a request for `/../../../../etc/shadow` would be served
/// verbatim, since the process can read whatever the user can.
fn safe_join(root: &Path, relative: &str) -> Option<PathBuf> {
    let decoded = hdm_net::percent_decode_str(relative);
    if decoded.contains('\0') {
        return None;
    }
    let mut path = root.to_path_buf();
    for segment in decoded.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            // Absolute components and drive letters would replace the root.
            s if s.contains(':') => return None,
            s => path.push(s),
        }
    }
    // Belt and braces: confirm the result really is inside the root.
    let canonical_root = root.canonicalize().ok()?;
    match path.canonicalize() {
        Ok(canonical) => canonical.starts_with(&canonical_root).then_some(path),
        // The file does not exist; the segment checks above already ensured the
        // path cannot have escaped.
        Err(_) => Some(path),
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// A parsed request.
pub struct HttpRequest {
    pub method: String,
    /// Path only, percent-decoded separators preserved.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn query(&self, name: &str) -> Option<String> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    pub fn query_flag(&self, name: &str) -> bool {
        self.query(name)
            .map(|v| v != "false" && v != "0")
            .unwrap_or(false)
    }

    /// The body parsed as JSON, or `Json::Null` when there is none.
    pub fn json(&self) -> Result<Json, String> {
        if self.body.is_empty() {
            return Ok(Json::Null);
        }
        let text = std::str::from_utf8(&self.body).map_err(|_| "body is not UTF-8".to_string())?;
        hdm_json::parse(text).map_err(|e| e.to_string())
    }

    /// The path split on `/`, with empty segments dropped.
    pub fn segments(&self) -> Vec<&str> {
        self.path.split('/').filter(|s| !s.is_empty()).collect()
    }
}

fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut line = String::new();
    let mut limited = (&mut reader).take(MAX_REQUEST_LINE as u64);
    if limited.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.trim_end().split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        let mut limited = (&mut reader).take(MAX_REQUEST_LINE as u64);
        if limited.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return None;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let length: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    if length > 0 && reader.read_exact(&mut body).is_err() {
        return None;
    }

    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let query = query_string
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (
                hdm_net::percent_decode_str(&k.replace('+', " ")),
                hdm_net::percent_decode_str(&v.replace('+', " ")),
            )
        })
        .collect();

    Some(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    origin: Option<&str>,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\
         X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n",
        body.len()
    );
    if let Some(origin) = origin {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn write_preflight(stream: &mut TcpStream, origin: Option<&str>) -> io::Result<()> {
    let mut head = String::from(
        "HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, \
         OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\n\
         Access-Control-Max-Age: 600\r\nContent-Length: 0\r\nConnection: close\r\n",
    );
    if let Some(origin) = origin {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        ));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.flush()
}

/// Used by the event stream to pace snapshots.
#[allow(dead_code)]
fn elapsed_since(start: Instant) -> Duration {
    start.elapsed()
}
