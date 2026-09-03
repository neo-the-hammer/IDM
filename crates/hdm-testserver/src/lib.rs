//! A controllable HTTP/1.1 origin server for testing the download engine.
//!
//! The engine's hardest requirements — segmented range requests, resume across
//! a restart, retry after a mid-stream disconnect, throttling — can only be
//! tested against a server that misbehaves on demand. This one does: it can
//! refuse ranges, drop the connection after N bytes, change its ETag between
//! requests, stall, or demand authentication.
//!
//! Test-only. Not part of the shipped product.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub mod tls;

/// How a route behaves.
#[derive(Clone)]
pub enum Route {
    /// Serves bytes, honouring `Range` unless configured otherwise.
    File(Arc<FileRoute>),
    /// Replies with a redirect to `location`.
    Redirect { status: u16, location: String },
    /// Replies with a bare status code and a short text body.
    Status(u16),
    /// Serves bytes with `Transfer-Encoding: chunked` and no length.
    Chunked(Arc<Vec<u8>>),
    /// Requires HTTP Basic credentials before delegating to the inner route.
    BasicAuth {
        user: String,
        pass: String,
        inner: Box<Route>,
    },
    /// Requires HTTP Digest credentials before delegating to the inner route.
    DigestAuth {
        user: String,
        pass: String,
        realm: String,
        nonce: String,
        inner: Box<Route>,
    },
}

/// Knobs for a file route.
pub struct FileRoute {
    pub data: Vec<u8>,
    /// When false the server answers `Accept-Ranges: none` and ignores `Range`,
    /// forcing the engine down its single-connection path.
    pub accept_ranges: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub content_type: Option<String>,
    pub content_disposition: Option<String>,
    /// Closes the connection abruptly after this many body bytes, every time.
    pub cut_after: Option<u64>,
    /// Closes the connection after this many body bytes, but only for the
    /// first `cut_times` requests — so a retry eventually succeeds.
    pub cut_times: Arc<AtomicU64>,
    /// Sleeps this long before each 8 KiB of body, to simulate a slow link.
    pub delay_per_chunk: Option<Duration>,
    /// Replaces the ETag after the first response, to test that resume
    /// correctly refuses to continue into a changed file.
    pub mutate_etag_after_first: bool,
    served: AtomicU64,
}

impl FileRoute {
    pub fn new(data: Vec<u8>) -> FileRoute {
        FileRoute {
            data,
            accept_ranges: true,
            etag: Some("\"v1\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2020 07:28:00 GMT".to_string()),
            content_type: Some("application/octet-stream".to_string()),
            content_disposition: None,
            cut_after: None,
            cut_times: Arc::new(AtomicU64::new(0)),
            delay_per_chunk: None,
            mutate_etag_after_first: false,
            served: AtomicU64::new(0),
        }
    }
}

/// A parsed request, as seen by the server.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Default)]
pub struct ServerBuilder {
    routes: HashMap<String, Route>,
}

impl ServerBuilder {
    pub fn new() -> ServerBuilder {
        ServerBuilder::default()
    }

    /// A plain, fully range-capable file.
    pub fn file(mut self, path: &str, data: Vec<u8>) -> ServerBuilder {
        self.routes.insert(
            path.to_string(),
            Route::File(Arc::new(FileRoute::new(data))),
        );
        self
    }

    /// A file with custom behaviour.
    pub fn file_with(
        mut self,
        path: &str,
        data: Vec<u8>,
        configure: impl FnOnce(&mut FileRoute),
    ) -> ServerBuilder {
        let mut route = FileRoute::new(data);
        configure(&mut route);
        self.routes
            .insert(path.to_string(), Route::File(Arc::new(route)));
        self
    }

    pub fn redirect(mut self, path: &str, status: u16, location: &str) -> ServerBuilder {
        self.routes.insert(
            path.to_string(),
            Route::Redirect {
                status,
                location: location.to_string(),
            },
        );
        self
    }

    pub fn status(mut self, path: &str, status: u16) -> ServerBuilder {
        self.routes.insert(path.to_string(), Route::Status(status));
        self
    }

    pub fn chunked(mut self, path: &str, data: Vec<u8>) -> ServerBuilder {
        self.routes
            .insert(path.to_string(), Route::Chunked(Arc::new(data)));
        self
    }

    pub fn basic_auth(
        mut self,
        path: &str,
        user: &str,
        pass: &str,
        data: Vec<u8>,
    ) -> ServerBuilder {
        self.routes.insert(
            path.to_string(),
            Route::BasicAuth {
                user: user.into(),
                pass: pass.into(),
                inner: Box::new(Route::File(Arc::new(FileRoute::new(data)))),
            },
        );
        self
    }

    pub fn digest_auth(
        mut self,
        path: &str,
        user: &str,
        pass: &str,
        data: Vec<u8>,
    ) -> ServerBuilder {
        self.routes.insert(
            path.to_string(),
            Route::DigestAuth {
                user: user.into(),
                pass: pass.into(),
                realm: "hydra-test".into(),
                nonce: "deadbeefcafe".into(),
                inner: Box::new(Route::File(Arc::new(FileRoute::new(data)))),
            },
        );
        self
    }

    pub fn route(mut self, path: &str, route: Route) -> ServerBuilder {
        self.routes.insert(path.to_string(), route);
        self
    }

    /// Starts a plaintext server on an ephemeral port.
    pub fn start(self) -> TestServer {
        TestServer::start(self.routes, None)
    }

    /// Starts an HTTPS server using the supplied certificate and key.
    pub fn start_tls(self, cert: tls::CertPaths) -> TestServer {
        TestServer::start(self.routes, Some(cert))
    }
}

pub struct TestServer {
    addr: SocketAddr,
    running: Arc<AtomicBool>,
    /// Every request the server has answered, in order.
    pub log: Arc<Mutex<Vec<RecordedRequest>>>,
    scheme: &'static str,
}

impl TestServer {
    fn start(routes: HashMap<String, Route>, cert: Option<tls::CertPaths>) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("cannot bind test server");
        let addr = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let log = Arc::new(Mutex::new(Vec::new()));
        let scheme = if cert.is_some() { "https" } else { "http" };

        let tls_ctx =
            cert.map(|c| Arc::new(tls::ServerContext::new(&c).expect("TLS server setup")));

        {
            let running = running.clone();
            let log = log.clone();
            let routes = Arc::new(routes);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let routes = routes.clone();
                    let log = log.clone();
                    let tls_ctx = tls_ctx.clone();
                    thread::spawn(move || {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
                        match tls_ctx {
                            Some(ctx) => {
                                if let Ok(s) = ctx.accept(stream) {
                                    serve(s, &routes, &log);
                                }
                            }
                            None => serve(stream, &routes, &log),
                        }
                    });
                }
            });
        }

        TestServer {
            addr,
            running,
            log,
            scheme,
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The base URL, e.g. `http://127.0.0.1:38211`.
    pub fn base(&self) -> String {
        format!("{}://127.0.0.1:{}", self.scheme, self.addr.port())
    }

    /// A full URL for `path`. TLS routes use `localhost` so that the
    /// certificate's subject alternative name matches.
    pub fn url(&self, path: &str) -> String {
        if self.scheme == "https" {
            format!("https://localhost:{}{}", self.addr.port(), path)
        } else {
            format!("{}{}", self.base(), path)
        }
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().unwrap().clone()
    }

    pub fn request_count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    pub fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // Unblock the accept loop so the thread can notice and exit.
        let _ = TcpStream::connect(self.addr);
    }
}

/// A connection that can be read, written and abruptly reset.
pub trait Conn: Read + Write + Send {
    fn hard_close(&mut self);
}

impl Conn for TcpStream {
    fn hard_close(&mut self) {
        // Closing with bytes still outstanding against the advertised
        // Content-Length is exactly the truncation a real network failure
        // produces, and is what the client must detect and retry.
        let _ = self.shutdown(Shutdown::Both);
    }
}

fn serve<C: Conn + 'static>(
    conn: C,
    routes: &HashMap<String, Route>,
    log: &Mutex<Vec<RecordedRequest>>,
) {
    let mut reader = BufReader::new(conn);
    // Serve requests until the client goes away or asks us to close.
    loop {
        let Some(request) = read_request(&mut reader) else {
            return;
        };
        log.lock().unwrap().push(request.clone());

        let path = request.target.split('?').next().unwrap_or("/").to_string();
        let keep_alive = request
            .header("Connection")
            .map(|v| !v.eq_ignore_ascii_case("close"))
            .unwrap_or(true);

        let route = routes.get(&path).cloned();
        let conn = reader.get_mut();
        let completed = match route {
            Some(route) => dispatch(conn, &request, &route, keep_alive),
            None => {
                write_simple(conn, 404, "Not Found", b"not found", keep_alive);
                true
            }
        };
        if !keep_alive || !completed {
            return;
        }
    }
}

/// Returns false if the connection was deliberately broken.
fn dispatch<C: Conn>(
    conn: &mut C,
    request: &RecordedRequest,
    route: &Route,
    keep_alive: bool,
) -> bool {
    match route {
        Route::Status(code) => {
            write_simple(
                conn,
                *code,
                "Status",
                format!("status {code}").as_bytes(),
                keep_alive,
            );
            true
        }
        Route::Redirect { status, location } => {
            let head = format!(
                "HTTP/1.1 {status} Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\n\
                 Connection: {}\r\n\r\n",
                if keep_alive { "keep-alive" } else { "close" }
            );
            let _ = conn.write_all(head.as_bytes());
            true
        }
        Route::Chunked(data) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: \
                 application/octet-stream\r\nConnection: {}\r\n\r\n",
                if keep_alive { "keep-alive" } else { "close" }
            );
            let _ = conn.write_all(head.as_bytes());
            if request.method != "HEAD" {
                for chunk in data.chunks(1000) {
                    let _ = conn.write_all(format!("{:x}\r\n", chunk.len()).as_bytes());
                    let _ = conn.write_all(chunk);
                    let _ = conn.write_all(b"\r\n");
                }
                let _ = conn.write_all(b"0\r\n\r\n");
            }
            true
        }
        Route::BasicAuth { user, pass, inner } => {
            let expected = format!("Basic {}", base64(format!("{user}:{pass}").as_bytes()));
            match request.header("Authorization") {
                Some(got) if got == expected => dispatch(conn, request, inner, keep_alive),
                _ => {
                    let head = format!(
                        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"hydra-test\"\r\n\
                         Content-Length: 0\r\nConnection: {}\r\n\r\n",
                        if keep_alive { "keep-alive" } else { "close" }
                    );
                    let _ = conn.write_all(head.as_bytes());
                    true
                }
            }
        }
        Route::DigestAuth {
            user,
            pass,
            realm,
            nonce,
            inner,
        } => {
            if digest_ok(request, user, pass, realm, nonce) {
                dispatch(conn, request, inner, keep_alive)
            } else {
                let head = format!(
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"{realm}\", \
                     qop=\"auth\", nonce=\"{nonce}\", algorithm=MD5\r\nContent-Length: 0\r\n\
                     Connection: {}\r\n\r\n",
                    if keep_alive { "keep-alive" } else { "close" }
                );
                let _ = conn.write_all(head.as_bytes());
                true
            }
        }
        Route::File(file) => serve_file(conn, request, file, keep_alive),
    }
}

fn serve_file<C: Conn>(
    conn: &mut C,
    request: &RecordedRequest,
    file: &FileRoute,
    keep_alive: bool,
) -> bool {
    let served = file.served.fetch_add(1, Ordering::SeqCst);
    let total = file.data.len() as u64;

    let etag = match (&file.etag, file.mutate_etag_after_first, served) {
        (Some(_), true, n) if n > 0 => Some("\"v2-changed\"".to_string()),
        (Some(e), _, _) => Some(e.clone()),
        (None, _, _) => None,
    };

    // Resolve the requested range.
    let (start, end, partial) = match request.header("Range").filter(|_| file.accept_ranges) {
        Some(spec) => match parse_range(spec, total) {
            Some((s, e)) => (s, e, true),
            None => {
                let head = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{total}\r\n\
                     Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = conn.write_all(head.as_bytes());
                return false;
            }
        },
        None => (0, total.saturating_sub(1), false),
    };
    let length = if total == 0 { 0 } else { end - start + 1 };

    let mut head = String::new();
    head.push_str(if partial {
        "HTTP/1.1 206 Partial Content\r\n"
    } else {
        "HTTP/1.1 200 OK\r\n"
    });
    head.push_str(&format!("Content-Length: {length}\r\n"));
    if partial {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    head.push_str(&format!(
        "Accept-Ranges: {}\r\n",
        if file.accept_ranges { "bytes" } else { "none" }
    ));
    if let Some(e) = &etag {
        head.push_str(&format!("ETag: {e}\r\n"));
    }
    if let Some(lm) = &file.last_modified {
        head.push_str(&format!("Last-Modified: {lm}\r\n"));
    }
    if let Some(ct) = &file.content_type {
        head.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(cd) = &file.content_disposition {
        head.push_str(&format!("Content-Disposition: {cd}\r\n"));
    }
    head.push_str(&format!(
        "Connection: {}\r\n\r\n",
        if keep_alive { "keep-alive" } else { "close" }
    ));

    if conn.write_all(head.as_bytes()).is_err() {
        return false;
    }
    if request.method == "HEAD" {
        return true;
    }

    // Decide whether this response gets cut short.
    let cut_at = match file.cut_after {
        Some(n) => {
            let budget = file.cut_times.load(Ordering::SeqCst);
            // 0 means "always cut"; otherwise cut only while the budget lasts.
            if budget == 0 && file.cut_times_is_unlimited() {
                Some(n)
            } else if budget > 0 {
                file.cut_times.fetch_sub(1, Ordering::SeqCst);
                Some(n)
            } else {
                None
            }
        }
        None => None,
    };

    let body = &file.data[start as usize..(end + 1).min(total) as usize];
    let mut written = 0u64;
    for chunk in body.chunks(8192) {
        if let Some(limit) = cut_at {
            if written >= limit {
                conn.hard_close();
                return false;
            }
        }
        if let Some(d) = file.delay_per_chunk {
            thread::sleep(d);
        }
        let take = match cut_at {
            Some(limit) => chunk.len().min((limit - written) as usize),
            None => chunk.len(),
        };
        if conn.write_all(&chunk[..take]).is_err() {
            return false;
        }
        written += take as u64;
        if cut_at == Some(written) {
            let _ = conn.flush();
            conn.hard_close();
            return false;
        }
    }
    let _ = conn.flush();
    true
}

impl FileRoute {
    /// `cut_times` starts at 0 meaning "unlimited" only when `cut_after` was
    /// set without a budget. Builders that want a budget set it explicitly.
    fn cut_times_is_unlimited(&self) -> bool {
        self.cut_times.load(Ordering::SeqCst) == 0
    }

    /// Cuts the connection for the next `n` requests only.
    pub fn cut_for_next(&mut self, bytes: u64, n: u64) {
        self.cut_after = Some(bytes);
        self.cut_times = Arc::new(AtomicU64::new(n));
    }
}

fn write_simple<C: Conn>(conn: &mut C, status: u16, reason: &str, body: &[u8], keep_alive: bool) {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\n\
         Connection: {}\r\n\r\n",
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
}

fn read_request<C: Read>(reader: &mut BufReader<C>) -> Option<RecordedRequest> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let line = line.trim_end();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();

    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).ok()? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }

    // Consume any request body so the next request on this connection parses.
    if let Some((_, len)) = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        if let Ok(n) = len.parse::<usize>() {
            let mut body = vec![0u8; n];
            let _ = reader.read_exact(&mut body);
        }
    }

    Some(RecordedRequest {
        method,
        target,
        headers,
    })
}

/// Parses `bytes=start-end`, `bytes=start-` and `bytes=-suffix`.
fn parse_range(spec: &str, total: u64) -> Option<(u64, u64)> {
    let spec = spec.trim().strip_prefix("bytes=")?.trim();
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        // Suffix range: the last N bytes.
        let n: u64 = b.trim().parse().ok()?;
        if n == 0 || total == 0 {
            return None;
        }
        return Some((total.saturating_sub(n), total - 1));
    }
    let start: u64 = a.trim().parse().ok()?;
    if start >= total {
        return None;
    }
    let end = if b.trim().is_empty() {
        total - 1
    } else {
        b.trim().parse::<u64>().ok()?.min(total - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

// --- tiny local copies, so this crate depends on nothing -------------------

fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Verifies an RFC 2617 Digest `Authorization` header.
fn digest_ok(request: &RecordedRequest, user: &str, pass: &str, realm: &str, nonce: &str) -> bool {
    let Some(header) = request.header("Authorization") else {
        return false;
    };
    let Some(rest) = header.strip_prefix("Digest ") else {
        return false;
    };

    let mut params: HashMap<String, String> = HashMap::new();
    for part in rest.split(',') {
        if let Some((k, v)) = part.split_once('=') {
            params.insert(
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches('"').to_string(),
            );
        }
    }
    if params.get("username").map(String::as_str) != Some(user) {
        return false;
    }
    if params.get("nonce").map(String::as_str) != Some(nonce) {
        return false;
    }

    let uri = params.get("uri").cloned().unwrap_or_default();
    let ha1 = md5_hex(format!("{user}:{realm}:{pass}").as_bytes());
    let ha2 = md5_hex(format!("{}:{}", request.method, uri).as_bytes());
    let expected = match params.get("qop").map(String::as_str) {
        Some("auth") => {
            let nc = params.get("nc").cloned().unwrap_or_default();
            let cnonce = params.get("cnonce").cloned().unwrap_or_default();
            md5_hex(format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}").as_bytes())
        }
        _ => md5_hex(format!("{ha1}:{nonce}:{ha2}").as_bytes()),
    };
    params.get("response").map(String::as_str) == Some(expected.as_str())
}

/// A local MD5, so the test server needs no dependency on the crate under test.
fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());

    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in msg.chunks(64) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    }
    h.iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Deterministic pseudo-random test data, so failures are reproducible.
pub fn test_data(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u64 = 0x243F6A8885A308D3;
    while out.len() < len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}
