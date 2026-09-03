//! End-to-end tests of the HTTP client against a controllable origin server.

use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::Request;
use hdm_net::url::Url;
use hdm_testserver::{test_data, ServerBuilder};
use std::io::Read;

fn client() -> Client {
    Client::new(ClientConfig::new()).expect("client")
}

fn get(client: &Client, url: &str) -> hdm_net::Fetch {
    client
        .send(Request::get(Url::parse(url).unwrap()))
        .expect("request failed")
}

// ------------------------------------------------------------ basic transfer

#[test]
fn fetches_a_body_intact() {
    let data = test_data(50_000);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let mut fetch = get(&client(), &server.url("/f.bin"));
    assert_eq!(fetch.response.status, 200);
    assert_eq!(fetch.response.content_length(), Some(50_000));
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn handles_an_empty_body() {
    let server = ServerBuilder::new().file("/empty.bin", Vec::new()).start();
    let mut fetch = get(&client(), &server.url("/empty.bin"));
    assert_eq!(fetch.response.status, 200);
    assert_eq!(
        fetch.response.read_to_vec(usize::MAX).unwrap(),
        Vec::<u8>::new()
    );
}

#[test]
fn head_requests_carry_no_body() {
    let server = ServerBuilder::new().file("/f.bin", test_data(4096)).start();
    let mut fetch = client()
        .send(Request::head(Url::parse(&server.url("/f.bin")).unwrap()))
        .unwrap();
    assert_eq!(fetch.response.status, 200);
    // The length is advertised even though no bytes follow, which is exactly
    // what makes a HEAD probe useful before segmenting.
    assert_eq!(fetch.response.content_length(), Some(4096));
    assert!(fetch.response.accepts_ranges());
    assert!(fetch.response.read_to_vec(usize::MAX).unwrap().is_empty());
}

#[test]
fn decodes_chunked_transfer_encoding() {
    let data = test_data(25_000);
    let server = ServerBuilder::new().chunked("/c.bin", data.clone()).start();
    let mut fetch = get(&client(), &server.url("/c.bin"));
    assert_eq!(
        fetch.response.content_length(),
        None,
        "chunked has no Content-Length"
    );
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn reports_error_statuses() {
    let server = ServerBuilder::new().status("/gone", 410).start();
    let fetch = get(&client(), &server.url("/gone"));
    assert_eq!(fetch.response.status, 410);
    let missing = get(&client(), &server.url("/nope"));
    assert_eq!(missing.response.status, 404);
}

// -------------------------------------------------------------------- ranges

#[test]
fn range_requests_return_the_exact_slice() {
    let data = test_data(10_000);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let request =
        Request::get(Url::parse(&server.url("/f.bin")).unwrap()).with_range(1000, Some(1999));
    let mut fetch = client().send(request).unwrap();

    assert_eq!(fetch.response.status, 206);
    assert!(fetch.response.is_partial());
    assert_eq!(fetch.response.content_length(), Some(1000));
    // Content-Length is the slice, but total_size must report the whole file —
    // this is the number segmentation is planned from.
    assert_eq!(fetch.response.total_size(), Some(10_000));
    assert_eq!(
        fetch.response.read_to_vec(usize::MAX).unwrap(),
        data[1000..2000]
    );
}

#[test]
fn open_ended_range_runs_to_the_end() {
    let data = test_data(5000);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();
    let request = Request::get(Url::parse(&server.url("/f.bin")).unwrap()).with_range(4000, None);
    let mut fetch = client().send(request).unwrap();
    assert_eq!(
        fetch.response.read_to_vec(usize::MAX).unwrap(),
        data[4000..]
    );
}

#[test]
fn every_segment_of_a_split_download_reassembles_exactly() {
    // The core promise of a segmented downloader: N ranged requests must
    // concatenate to the same bytes as one plain request.
    let data = test_data(64 * 1024 + 517);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();
    let client = client();
    let url = server.url("/f.bin");

    for segments in [1u64, 2, 3, 8, 16] {
        let total = data.len() as u64;
        let per = total / segments;
        let mut assembled = Vec::new();
        for i in 0..segments {
            let start = i * per;
            let end = if i == segments - 1 {
                total - 1
            } else {
                start + per - 1
            };
            let request = Request::get(Url::parse(&url).unwrap()).with_range(start, Some(end));
            let mut fetch = client.send(request).unwrap();
            assembled.extend_from_slice(&fetch.response.read_to_vec(usize::MAX).unwrap());
        }
        assert_eq!(assembled, data, "reassembly with {segments} segments");
    }
}

#[test]
fn a_server_without_range_support_is_detectable() {
    let data = test_data(3000);
    let server = ServerBuilder::new()
        .file_with("/nr.bin", data.clone(), |f| f.accept_ranges = false)
        .start();

    let request =
        Request::get(Url::parse(&server.url("/nr.bin")).unwrap()).with_range(100, Some(199));
    let mut fetch = client().send(request).unwrap();

    // The server ignored Range and sent everything. The engine must notice the
    // 200 and fall back to a single connection rather than write the whole
    // file at offset 100.
    assert_eq!(fetch.response.status, 200);
    assert!(!fetch.response.is_partial());
    assert!(!fetch.response.accepts_ranges());
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn an_unsatisfiable_range_is_rejected() {
    let server = ServerBuilder::new().file("/f.bin", test_data(100)).start();
    let request =
        Request::get(Url::parse(&server.url("/f.bin")).unwrap()).with_range(500, Some(600));
    let fetch = client().send(request).unwrap();
    assert_eq!(fetch.response.status, 416);
}

// ----------------------------------------------------------------- redirects

#[test]
fn follows_a_redirect_chain_and_reports_the_final_url() {
    let data = test_data(1234);
    let server = ServerBuilder::new()
        .redirect("/a", 302, "/b")
        .redirect("/b", 301, "/c")
        .redirect("/c", 307, "/final.bin")
        .file("/final.bin", data.clone())
        .start();

    let mut fetch = get(&client(), &server.url("/a"));
    assert_eq!(fetch.response.status, 200);
    assert_eq!(
        fetch.final_url.path, "/final.bin",
        "the final URL is what segments must use"
    );
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn follows_an_absolute_redirect() {
    let data = test_data(64);
    let target = ServerBuilder::new().file("/t.bin", data.clone()).start();
    let source = ServerBuilder::new()
        .redirect("/go", 302, &target.url("/t.bin"))
        .start();

    let mut fetch = get(&client(), &source.url("/go"));
    assert_eq!(fetch.final_url.port, Some(target.port()));
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn detects_a_redirect_loop() {
    let server = ServerBuilder::new()
        .redirect("/a", 302, "/b")
        .redirect("/b", 302, "/a")
        .start();
    let err = client()
        .send(Request::get(Url::parse(&server.url("/a")).unwrap()))
        .unwrap_err();
    assert!(err.to_string().contains("loop"), "got: {err}");
}

/// A redirect to another host must not carry the Authorization header with it.
#[test]
fn credentials_are_not_forwarded_across_origins() {
    let data = test_data(32);
    // The second server records what it receives but needs no credentials.
    let target = ServerBuilder::new().file("/t.bin", data.clone()).start();
    let source = ServerBuilder::new()
        .redirect("/go", 302, &target.url("/t.bin"))
        .start();

    let request = Request::get(Url::parse(&source.url("/go")).unwrap())
        .header("Authorization", "Basic c2VjcmV0OnBhc3N3b3Jk");
    let mut fetch = client().send(request).unwrap();
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);

    let received = target.requests();
    assert!(
        received.iter().all(|r| r.header("Authorization").is_none()),
        "the Authorization header leaked to the redirect target: {received:?}"
    );
}

// ------------------------------------------------------------ authentication

#[test]
fn answers_a_basic_auth_challenge() {
    let data = test_data(500);
    let server = ServerBuilder::new()
        .basic_auth("/private.bin", "alice", "s3cret", data.clone())
        .start();

    let mut config = ClientConfig::new();
    config.credentials = Some(hdm_net::auth::Credentials {
        username: "alice".into(),
        password: "s3cret".into(),
    });
    let client = Client::new(config).unwrap();

    let mut fetch = client
        .send(Request::get(
            Url::parse(&server.url("/private.bin")).unwrap(),
        ))
        .unwrap();
    assert_eq!(fetch.response.status, 200);
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn credentials_embedded_in_the_url_are_used() {
    let data = test_data(64);
    let server = ServerBuilder::new()
        .basic_auth("/p.bin", "bob", "pw", data.clone())
        .start();
    let url = format!("http://bob:pw@127.0.0.1:{}/p.bin", server.port());
    let mut fetch = get(&client(), &url);
    assert_eq!(fetch.response.status, 200);
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

#[test]
fn wrong_basic_credentials_stay_rejected() {
    let server = ServerBuilder::new()
        .basic_auth("/p.bin", "alice", "right", test_data(10))
        .start();
    let mut config = ClientConfig::new();
    config.credentials = Some(hdm_net::auth::Credentials {
        username: "alice".into(),
        password: "wrong".into(),
    });
    let fetch = Client::new(config)
        .unwrap()
        .send(Request::get(Url::parse(&server.url("/p.bin")).unwrap()))
        .unwrap();
    assert_eq!(
        fetch.response.status, 401,
        "must not loop retrying a bad password"
    );
}

#[test]
fn answers_a_digest_auth_challenge() {
    let data = test_data(777);
    let server = ServerBuilder::new()
        .digest_auth("/d.bin", "carol", "hunter2", data.clone())
        .start();

    let mut config = ClientConfig::new();
    config.credentials = Some(hdm_net::auth::Credentials {
        username: "carol".into(),
        password: "hunter2".into(),
    });
    let mut fetch = Client::new(config)
        .unwrap()
        .send(Request::get(Url::parse(&server.url("/d.bin")).unwrap()))
        .unwrap();

    assert_eq!(fetch.response.status, 200, "digest handshake failed");
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);
}

// -------------------------------------------------------------- truncation

/// A connection that dies mid-body must surface as an error, never as a short
/// file. Silently accepting this is how a download manager corrupts data.
#[test]
fn a_truncated_body_is_an_error_not_a_short_read() {
    let server = ServerBuilder::new()
        .file_with("/cut.bin", test_data(100_000), |f| f.cut_after = Some(4096))
        .start();

    let mut fetch = get(&client(), &server.url("/cut.bin"));
    assert_eq!(fetch.response.content_length(), Some(100_000));

    let mut sink = Vec::new();
    let err = fetch
        .response
        .body
        .read_to_end(&mut sink)
        .expect_err("a truncated body must not read as success");
    assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof, "got: {err}");
    assert!(sink.len() < 100_000);
}

// ------------------------------------------------------------------ metadata

#[test]
fn reads_validators_used_for_resume() {
    let server = ServerBuilder::new()
        .file_with("/f.bin", test_data(10), |f| {
            f.etag = Some("\"abc-123\"".into());
            f.last_modified = Some("Mon, 01 Jan 2024 00:00:00 GMT".into());
        })
        .start();
    let fetch = get(&client(), &server.url("/f.bin"));
    assert_eq!(fetch.response.etag().as_deref(), Some("\"abc-123\""));
    assert_eq!(
        fetch.response.last_modified().as_deref(),
        Some("Mon, 01 Jan 2024 00:00:00 GMT")
    );
    assert_eq!(
        fetch.response.content_type(),
        Some("application/octet-stream")
    );
}

#[test]
fn filename_prefers_content_disposition() {
    let server = ServerBuilder::new()
        .file_with("/download.php", test_data(10), |f| {
            f.content_disposition = Some("attachment; filename=\"ubuntu-24.04.iso\"".into());
        })
        .start();
    let fetch = get(&client(), &server.url("/download.php"));
    assert_eq!(
        fetch.response.filename(&fetch.final_url).as_deref(),
        Some("ubuntu-24.04.iso"),
        "a generic script URL must not become the filename"
    );
}

#[test]
fn filename_handles_rfc5987_encoding() {
    let server = ServerBuilder::new()
        .file_with("/get", test_data(10), |f| {
            // A Persian filename, the case the fa locale makes likely.
            f.content_disposition = Some(
                "attachment; filename=\"fallback.bin\"; \
                 filename*=UTF-8''%D9%81%D8%A7%DB%8C%D9%84.zip"
                    .into(),
            );
        })
        .start();
    let fetch = get(&client(), &server.url("/get"));
    assert_eq!(
        fetch.response.filename(&fetch.final_url).as_deref(),
        Some("فایل.zip"),
        "filename* must win over the ASCII fallback"
    );
}

#[test]
fn filename_falls_back_to_the_url() {
    let server = ServerBuilder::new()
        .file("/path/movie.mkv", test_data(10))
        .start();
    let fetch = get(&client(), &server.url("/path/movie.mkv"));
    assert_eq!(
        fetch.response.filename(&fetch.final_url).as_deref(),
        Some("movie.mkv")
    );
}

// ------------------------------------------------------------------- cookies

#[test]
fn cookies_set_during_a_redirect_reach_the_final_request() {
    // A sign-in bounce that sets a session cookie, then redirects to the file.
    let data = test_data(256);
    let server = ServerBuilder::new()
        .route(
            "/login",
            hdm_testserver::Route::Redirect {
                status: 302,
                location: "/file.bin".into(),
            },
        )
        .file("/file.bin", data.clone())
        .start();

    let client = client();
    client.cookies().lock().unwrap().add_raw(
        "session",
        "xyz",
        &Url::parse(&server.url("/")).unwrap(),
    );
    let mut fetch = client
        .send(Request::get(Url::parse(&server.url("/login")).unwrap()))
        .unwrap();
    assert_eq!(fetch.response.read_to_vec(usize::MAX).unwrap(), data);

    let final_request = server.requests().last().cloned().unwrap();
    assert_eq!(final_request.header("Cookie"), Some("session=xyz"));
}

// ---------------------------------------------------------- extension replay

#[test]
fn extra_headers_are_sent_on_every_request() {
    let server = ServerBuilder::new().file("/f.bin", test_data(10)).start();
    let mut config = ClientConfig::new();
    config
        .extra_headers
        .set("Referer", "https://example.com/page");
    config
        .extra_headers
        .set("User-Agent", "Mozilla/5.0 (pretend browser)");
    let client = Client::new(config).unwrap();
    let _ = client
        .send(Request::get(Url::parse(&server.url("/f.bin")).unwrap()))
        .unwrap();

    let request = server.requests().pop().unwrap();
    assert_eq!(request.header("Referer"), Some("https://example.com/page"));
    assert_eq!(
        request.header("User-Agent"),
        Some("Mozilla/5.0 (pretend browser)")
    );
}

#[test]
fn identity_encoding_is_always_requested() {
    // Compression would break the byte-offset correspondence segmentation
    // depends on, so it must never be negotiated.
    let server = ServerBuilder::new().file("/f.bin", test_data(10)).start();
    let _ = get(&client(), &server.url("/f.bin"));
    let request = server.requests().pop().unwrap();
    assert_eq!(request.header("Accept-Encoding"), Some("identity"));
}
