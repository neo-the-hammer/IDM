//! Integration tests for the local API, driven over a real socket.

use hdm_api::websocket;
use hdm_api::ApiServer;
use hdm_core::manager::Manager;
use hdm_json::{json, parse, Json};
use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::Request;
use hdm_net::url::Url;
use hdm_testserver::tls::TempDir;
use hdm_testserver::{test_data, ServerBuilder};
use std::sync::Arc;

struct Fixture {
    base: String,
    token: String,
    client: Client,
    bound: hdm_api::Bound,
    #[allow(dead_code)]
    dir: TempDir,
    manager: Arc<Manager>,
}

impl Fixture {
    fn start() -> Fixture {
        let dir = TempDir::new("hydra-api").unwrap();
        let manager = Manager::load(dir.path(), dir.path().to_path_buf());
        let token = "test-token-0123456789abcdef".to_string();
        let server = ApiServer {
            manager: manager.clone(),
            token: token.clone(),
            ui_dir: None,
            extra_origins: Vec::new(),
            version: "0.1.0-test".into(),
        };
        let (bound, _) = server.start(0).expect("server");
        Fixture {
            base: format!("http://127.0.0.1:{}", bound.addr.port()),
            token,
            client: Client::new(ClientConfig::new()).unwrap(),
            bound,
            dir,
            manager,
        }
    }

    /// Sends a request with the correct token.
    fn call(&self, method: &str, path: &str, body: Option<Json>) -> (u16, Json) {
        self.call_with(method, path, body, Some(&self.token.clone()), None)
    }

    fn call_with(
        &self,
        method: &str,
        path: &str,
        body: Option<Json>,
        token: Option<&str>,
        origin: Option<&str>,
    ) -> (u16, Json) {
        let url = Url::parse(&format!("{}{path}", self.base)).unwrap();
        let mut request = Request::get(url);
        request.method = method.to_string();
        if let Some(token) = token {
            request
                .headers
                .set("Authorization", format!("Bearer {token}"));
        }
        if let Some(origin) = origin {
            request.headers.set("Origin", origin);
        }
        if let Some(body) = body {
            request.headers.set("Content-Type", "application/json");
            request.body = Some(body.to_string_compact().into_bytes());
        }
        let mut fetch = self.client.send(request).expect("request");
        let status = fetch.response.status;
        let text = fetch.response.read_to_string(4 * 1024 * 1024).unwrap();
        (status, parse(&text).unwrap_or(Json::Null))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.bound.stop();
    }
}

// ---------------------------------------------------------------- access

/// Anything that can start downloads and write files must not be reachable
/// without the token.
#[test]
fn the_api_is_closed_without_a_token() {
    let api = Fixture::start();
    assert_eq!(
        api.call_with("GET", "/api/v1/downloads", None, None, None)
            .0,
        401
    );
    assert_eq!(
        api.call_with("GET", "/api/v1/settings", None, None, None).0,
        401
    );
    assert_eq!(
        api.call_with(
            "POST",
            "/api/v1/downloads",
            Some(json!({"url": "http://x/"})),
            None,
            None
        )
        .0,
        401
    );
}

#[test]
fn a_wrong_token_is_rejected() {
    let api = Fixture::start();
    assert_eq!(
        api.call_with(
            "GET",
            "/api/v1/downloads",
            None,
            Some("not-the-token"),
            None
        )
        .0,
        401
    );
    // A token that is a prefix of the real one must not pass either.
    assert_eq!(
        api.call_with("GET", "/api/v1/downloads", None, Some("test-token"), None)
            .0,
        401
    );
}

#[test]
fn the_right_token_is_accepted() {
    let api = Fixture::start();
    let (status, body) = api.call("GET", "/api/v1/health", None);
    assert_eq!(status, 200);
    assert_eq!(body.str_or("status", ""), "ok");
}

/// Without this check, any web page the user visits could drive the daemon
/// through the browser, since the token travels in a header the page controls.
#[test]
fn a_foreign_origin_is_refused() {
    let api = Fixture::start();
    for origin in ["https://evil.example", "http://attacker.test:8080", "null"] {
        let (status, _) = api.call_with(
            "GET",
            "/api/v1/health",
            None,
            Some(&api.token),
            Some(origin),
        );
        assert_eq!(status, 403, "origin {origin} should have been refused");
    }
}

#[test]
fn extension_and_local_origins_are_allowed() {
    let api = Fixture::start();
    for origin in [
        "chrome-extension://mnopqrstuvwxyzabcdefghijklmnop",
        "moz-extension://11111111-2222-3333-4444-555555555555",
        "http://localhost:5173",
        "http://127.0.0.1:47113",
    ] {
        let (status, _) = api.call_with(
            "GET",
            "/api/v1/health",
            None,
            Some(&api.token),
            Some(origin),
        );
        assert_eq!(status, 200, "origin {origin} should have been allowed");
    }
}

// ------------------------------------------------------------- downloads

#[test]
fn a_download_can_be_added_listed_and_removed() {
    let api = Fixture::start();
    let origin = ServerBuilder::new().file("/f.bin", test_data(2048)).start();

    let (status, created) = api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({"url": (origin.url("/f.bin")), "autostart": false, "connections": 4})),
    );
    assert_eq!(status, 201, "{created:?}");
    let id = created.str_or("id", "").to_string();
    assert!(!id.is_empty());

    let (_, listing) = api.call("GET", "/api/v1/downloads", None);
    let downloads = listing.get("downloads").and_then(Json::as_arr).unwrap();
    assert_eq!(downloads.len(), 1);
    assert_eq!(downloads[0].str_or("id", ""), id);

    let (status, one) = api.call("GET", &format!("/api/v1/downloads/{id}"), None);
    assert_eq!(status, 200);
    assert_eq!(one.str_or("id", ""), id);

    let (status, _) = api.call("DELETE", &format!("/api/v1/downloads/{id}"), None);
    assert_eq!(status, 200);
    let (_, listing) = api.call("GET", "/api/v1/downloads", None);
    assert!(listing
        .get("downloads")
        .and_then(Json::as_arr)
        .unwrap()
        .is_empty());
}

/// A typo should be reported when the link is added, not hours later when a
/// queued download finally runs.
#[test]
fn a_bad_url_is_rejected_on_submission() {
    let api = Fixture::start();
    for body in [
        json!({}),
        json!({"url": "not a url"}),
        json!({"url": ""}),
        // Schemes the engine cannot handle must not be silently accepted.
        json!({"url": "file:///etc/passwd"}),
        json!({"url": "javascript:alert(1)"}),
        json!({"url": "magnet:?xt=urn:btih:abc"}),
    ] {
        let (status, response) = api.call("POST", "/api/v1/downloads", Some(body.clone()));
        assert_eq!(
            status, 400,
            "expected {body} to be rejected, got {response}"
        );
        assert!(response.get("error").is_some());
    }
}

/// A header value containing CR or LF would let a crafted request inject extra
/// headers into every request the download makes.
#[test]
fn header_injection_is_stripped() {
    let api = Fixture::start();
    let origin = ServerBuilder::new().file("/f.bin", test_data(10)).start();
    let (status, created) = api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({
            "url": (origin.url("/f.bin")),
            "autostart": false,
            "headers": [
                {"name": "X-Good", "value": "fine"},
                {"name": "X-Bad", "value": "a\r\nX-Injected: yes"}
            ]
        })),
    );
    assert_eq!(status, 201);

    let headers = created
        .get("spec")
        .and_then(|s| s.get("headers"))
        .and_then(Json::as_arr)
        .unwrap();
    let names: Vec<&str> = headers.iter().map(|h| h.str_or("name", "")).collect();
    assert!(names.contains(&"X-Good"));
    assert!(
        !names.contains(&"X-Bad"),
        "a header with CRLF was stored: {headers:?}"
    );
}

#[test]
fn download_actions_are_reachable() {
    let api = Fixture::start();
    let origin = ServerBuilder::new().file("/f.bin", test_data(4096)).start();
    let (_, created) = api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({"url": (origin.url("/f.bin")), "autostart": false})),
    );
    let id = created.str_or("id", "").to_string();

    for action in ["pause", "start", "pause", "restart"] {
        let (status, body) = api.call("POST", &format!("/api/v1/downloads/{id}/{action}"), None);
        assert_eq!(status, 200, "{action} failed: {body}");
    }
    let (status, _) = api.call("POST", &format!("/api/v1/downloads/{id}/nonsense"), None);
    assert_eq!(status, 404);
}

#[test]
fn actions_on_a_missing_download_report_not_found() {
    let api = Fixture::start();
    assert_eq!(api.call("GET", "/api/v1/downloads/nope", None).0, 404);
    assert_eq!(api.call("DELETE", "/api/v1/downloads/nope", None).0, 404);
    assert_eq!(
        api.call("POST", "/api/v1/downloads/nope/pause", None).0,
        400
    );
}

#[test]
fn a_checksum_without_an_algorithm_is_inferred_from_its_length() {
    let api = Fixture::start();
    let origin = ServerBuilder::new().file("/f.bin", test_data(10)).start();
    let (status, created) = api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({
            "url": (origin.url("/f.bin")),
            "autostart": false,
            "checksum": ("ab".repeat(32))
        })),
    );
    assert_eq!(status, 201);
    assert_eq!(
        created.get("spec").unwrap().str_or("checksumAlgo", ""),
        "sha256",
        "a 64-character digest is unambiguously SHA-256"
    );

    let (status, _) = api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({"url": (origin.url("/f.bin")), "checksum": "abc"})),
    );
    assert_eq!(status, 400, "an unidentifiable digest must be refused");
}

// --------------------------------------------------------------- settings

#[test]
fn settings_can_be_read_and_written() {
    let api = Fixture::start();
    let (status, settings) = api.call("GET", "/api/v1/settings", None);
    assert_eq!(status, 200);
    assert_eq!(
        settings.u64_or("speedLimit", 999),
        0,
        "unlimited by default"
    );

    let mut updated = settings.clone();
    updated.insert("speedLimit", Json::from(512_000u64));
    updated.insert("language", Json::Str("fa".into()));
    let (status, saved) = api.call("PUT", "/api/v1/settings", Some(updated));
    assert_eq!(status, 200);
    assert_eq!(saved.u64_or("speedLimit", 0), 512_000);
    assert_eq!(saved.str_or("language", ""), "fa");

    // And it stuck.
    assert_eq!(api.manager.settings().speed_limit, 512_000);
}

#[test]
fn totals_reflect_the_list() {
    let api = Fixture::start();
    let origin = ServerBuilder::new().file("/f.bin", test_data(10)).start();
    api.call(
        "POST",
        "/api/v1/downloads",
        Some(json!({"url": (origin.url("/f.bin")), "autostart": false})),
    );
    let (status, totals) = api.call("GET", "/api/v1/totals", None);
    assert_eq!(status, 200);
    assert_eq!(totals.u64_or("total", 0), 1);
}

#[test]
fn unknown_endpoints_are_not_found() {
    let api = Fixture::start();
    assert_eq!(api.call("GET", "/api/v1/nonsense", None).0, 404);
    assert_eq!(api.call("GET", "/api/v2/downloads", None).0, 404);
}

// -------------------------------------------------------------- websocket

#[test]
fn the_websocket_accept_key_follows_rfc6455() {
    // The worked example from RFC 6455 section 1.3.
    assert_eq!(
        websocket::accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

/// The event stream carries the whole download list, so it needs the same
/// protection as the REST surface.
#[test]
fn the_event_stream_requires_a_token() {
    let api = Fixture::start();
    let url = Url::parse(&format!("{}/api/v1/events", api.base)).unwrap();
    let request = Request::get(url)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    let fetch = api.client.send(request).unwrap();
    assert_eq!(fetch.response.status, 401);
}

#[test]
fn the_event_stream_upgrades_with_a_token() {
    let api = Fixture::start();
    // The token goes in the query string because a browser cannot set headers
    // on a WebSocket handshake.
    let url = Url::parse(&format!("{}/api/v1/events?token={}", api.base, api.token)).unwrap();
    let request = Request::get(url)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    let fetch = api.client.send(request).unwrap();
    assert_eq!(fetch.response.status, 101);
    assert_eq!(
        fetch.response.headers.get("Sec-WebSocket-Accept"),
        Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
    fetch.response.shutdown();
}

// ------------------------------------------------------------ static UI

/// The daemon can read anything the user can, so a traversing path must never
/// escape the UI directory.
#[test]
fn the_ui_route_refuses_path_traversal() {
    let dir = TempDir::new("hydra-ui").unwrap();
    let ui = dir.path().join("ui");
    std::fs::create_dir_all(&ui).unwrap();
    std::fs::write(ui.join("index.html"), "<h1>Hydra</h1>").unwrap();
    // A file that exists outside the UI root, which must stay unreachable.
    std::fs::write(dir.path().join("secret.txt"), "SECRET").unwrap();

    let manager = Manager::load(dir.path(), dir.path().to_path_buf());
    let server = ApiServer {
        manager,
        token: "t".repeat(32),
        ui_dir: Some(ui.clone()),
        extra_origins: Vec::new(),
        version: "test".into(),
    };
    let (bound, _) = server.start(0).unwrap();
    let base = format!("http://127.0.0.1:{}", bound.addr.port());
    let client = Client::new(ClientConfig::new()).unwrap();

    let fetch_text = |path: &str| -> String {
        let url = Url::parse(&format!("{base}{path}")).unwrap();
        let mut fetch = client.send(Request::get(url)).unwrap();
        fetch.response.read_to_string(1024 * 1024).unwrap()
    };

    assert!(
        fetch_text("/").contains("Hydra"),
        "the UI itself must be served"
    );
    for attack in [
        "/../secret.txt",
        "/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/%2e%2e%2fsecret.txt",
        "/..%2fsecret.txt",
        "/./../secret.txt",
    ] {
        let body = fetch_text(attack);
        assert!(
            !body.contains("SECRET"),
            "traversal succeeded via {attack}: {body}"
        );
    }
    bound.stop();
}
