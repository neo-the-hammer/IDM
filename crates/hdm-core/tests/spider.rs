//! The site grabber, against a small local site.

use hdm_core::spider::{crawl, CrawlOptions};
use hdm_json::{parse, Json};
use hdm_testserver::{test_data, Route, ServerBuilder, TestServer};

/// A three-level site with files scattered through it, plus a few traps: an
/// off-site link, a page above the starting directory, and a cycle.
fn site() -> TestServer {
    let page = |body: &str| -> Vec<u8> {
        format!("<html><head><title>Page</title></head><body>{body}</body></html>").into_bytes()
    };

    ServerBuilder::new()
        .route(
            "/docs/index.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(page(
                    r#"<a href="manual.pdf">Manual</a>
                       <a href="chapter1.html">Chapter one</a>
                       <a href="/outside.html">Up a level</a>
                       <a href="http://example.invalid/away.html">Off site</a>
                       <a href="notes.txt">Notes</a>"#,
                ));
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .route(
            "/docs/chapter1.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(page(
                    r#"<a href="figure.png">Figure</a>
                       <a href="chapter2.html">Chapter two</a>
                       <a href="index.html">Back</a>"#,
                ));
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .route(
            "/docs/chapter2.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(page(
                    r#"<a href="deep.zip">Archive</a><a href="chapter1.html">Back</a>"#,
                ));
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .route(
            "/outside.html",
            Route::File(std::sync::Arc::new({
                let mut file =
                    hdm_testserver::FileRoute::new(page(r#"<a href="secret.iso">Secret</a>"#));
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .file("/docs/manual.pdf", test_data(64))
        .file("/docs/notes.txt", test_data(32))
        .file("/docs/figure.png", test_data(48))
        .file("/docs/deep.zip", test_data(96))
        .file("/secret.iso", test_data(16))
        .start()
}

fn options() -> CrawlOptions {
    CrawlOptions {
        // The local server needs no politeness delay, and waiting would only
        // make the suite slow.
        delay: std::time::Duration::ZERO,
        respect_robots: false,
        ..CrawlOptions::default()
    }
}

fn urls(result: &hdm_core::spider::CrawlResult) -> Vec<String> {
    let mut names: Vec<String> = result.files.iter().map(|f| f.filename.clone()).collect();
    names.sort();
    names
}

#[test]
fn a_depth_of_zero_reads_only_the_starting_page() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 0,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(result.pages_visited, 1);
    assert_eq!(urls(&result), ["manual.pdf", "notes.txt"]);
}

#[test]
fn deeper_crawls_reach_further() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 1,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(urls(&result), ["figure.png", "manual.pdf", "notes.txt"]);

    let deeper = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 2,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(
        urls(&deeper),
        ["deep.zip", "figure.png", "manual.pdf", "notes.txt"]
    );
}

/// A crawl that followed every link would wander off across the internet.
#[test]
fn it_stays_on_the_starting_host() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 3,
            same_host: true,
            ..options()
        },
    )
    .expect("crawl");
    assert!(
        result.files.iter().all(|f| f.url.contains("127.0.0.1")),
        "the crawl left the host: {:?}",
        result.files
    );
}

/// Anchoring to the starting directory is what makes "grab this folder" mean
/// what the user expects.
#[test]
fn it_stays_under_the_starting_directory() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 3,
            stay_under_path: true,
            ..options()
        },
    )
    .expect("crawl");
    assert!(
        !urls(&result).contains(&"secret.iso".to_string()),
        "the crawl rose above its starting directory: {:?}",
        urls(&result)
    );

    // Turning the restriction off reaches it.
    let unrestricted = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 3,
            stay_under_path: false,
            ..options()
        },
    )
    .expect("crawl");
    assert!(urls(&unrestricted).contains(&"secret.iso".to_string()));
}

/// The site contains a cycle; a crawler without a visited set would loop.
#[test]
fn it_never_visits_a_page_twice() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 5,
            ..options()
        },
    )
    .expect("crawl");
    // Four pages exist under /docs; the cycle must not inflate this.
    assert_eq!(result.pages_visited, 3, "revisited a page");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
}

#[test]
fn extension_filters_apply() {
    let server = site();
    let only_archives = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 3,
            include_extensions: vec!["zip".into(), "pdf".into()],
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(urls(&only_archives), ["deep.zip", "manual.pdf"]);

    let without_text = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 3,
            exclude_extensions: vec!["txt".into()],
            ..options()
        },
    )
    .expect("crawl");
    assert!(!urls(&without_text).contains(&"notes.txt".to_string()));
}

#[test]
fn limits_stop_the_crawl_and_say_so() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 5,
            max_files: 2,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(result.files.len(), 2);
    assert!(
        result.truncated,
        "hitting a limit must be reported, not hidden"
    );

    let few_pages = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 5,
            max_pages: 1,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(few_pages.pages_visited, 1);
    assert!(few_pages.truncated);
}

/// Each file records the page it came from, which becomes its Referer — many
/// servers refuse a download without one.
#[test]
fn files_remember_the_page_they_came_from() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 2,
            ..options()
        },
    )
    .expect("crawl");
    let figure = result
        .files
        .iter()
        .find(|f| f.filename == "figure.png")
        .unwrap();
    assert!(
        figure.found_on.ends_with("/docs/chapter1.html"),
        "got {}",
        figure.found_on
    );
    let manual = result
        .files
        .iter()
        .find(|f| f.filename == "manual.pdf")
        .unwrap();
    assert_eq!(
        manual.text, "Manual",
        "the link text is a better title than the filename"
    );
}

#[test]
fn a_missing_page_is_reported_without_stopping_the_crawl() {
    let server = ServerBuilder::new()
        .route(
            "/index.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(
                    br#"<a href="gone.html">Broken</a><a href="ok.zip">Good</a>"#.to_vec(),
                );
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .file("/ok.zip", test_data(16))
        .start();

    let result = crawl(
        &server.url("/index.html"),
        &CrawlOptions {
            max_depth: 2,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(
        urls(&result),
        ["ok.zip"],
        "one broken link must not lose the good ones"
    );
    assert_eq!(
        result.errors.len(),
        1,
        "the failure should be reported: {:?}",
        result.errors
    );
    assert!(result.errors[0].contains("404"));
}

// ------------------------------------------------------------------- robots

#[test]
fn robots_rules_are_honoured() {
    let server = ServerBuilder::new()
        .route(
            "/robots.txt",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(
                    b"User-agent: *\nDisallow: /private/\n".to_vec(),
                );
                file.content_type = Some("text/plain".into());
                file
            })),
        )
        .route(
            "/index.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(
                    br#"<a href="/private/hidden.html">Private</a>
                        <a href="/public/open.html">Public</a>"#
                        .to_vec(),
                );
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .route(
            "/private/hidden.html",
            Route::File(std::sync::Arc::new({
                let mut file =
                    hdm_testserver::FileRoute::new(br#"<a href="secret.zip">s</a>"#.to_vec());
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .route(
            "/public/open.html",
            Route::File(std::sync::Arc::new({
                let mut file =
                    hdm_testserver::FileRoute::new(br#"<a href="fine.zip">f</a>"#.to_vec());
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .file("/private/secret.zip", test_data(8))
        .file("/public/fine.zip", test_data(8))
        .start();

    let polite = crawl(
        &server.url("/index.html"),
        &CrawlOptions {
            max_depth: 3,
            stay_under_path: false,
            respect_robots: true,
            delay: std::time::Duration::ZERO,
            ..CrawlOptions::default()
        },
    )
    .expect("crawl");
    assert_eq!(
        urls(&polite),
        ["fine.zip"],
        "a disallowed path was crawled anyway"
    );

    // The override exists, and works.
    let rude = crawl(
        &server.url("/index.html"),
        &CrawlOptions {
            max_depth: 3,
            stay_under_path: false,
            ..options()
        },
    )
    .expect("crawl");
    assert!(urls(&rude).contains(&"secret.zip".to_string()));
}

// -------------------------------------------------------------- API shape

#[test]
fn options_parse_from_json_and_are_capped() {
    let value: Json = parse(
        r#"{"depth": 99, "maxPages": 999999, "include": [".ZIP", "pdf"],
            "exclude": ["Tmp"], "delayMs": 50, "sameHost": false}"#,
    )
    .unwrap();
    let options = CrawlOptions::from_json(&value);
    // An accidental depth of 99 would crawl for hours.
    assert_eq!(options.max_depth, 8, "depth must be capped");
    assert_eq!(options.max_pages, 5_000, "page count must be capped");
    assert_eq!(
        options.include_extensions,
        ["zip", "pdf"],
        "extensions are normalized"
    );
    assert_eq!(options.exclude_extensions, ["tmp"]);
    assert!(!options.same_host);
}

#[test]
fn a_result_serializes_for_the_api() {
    let server = site();
    let result = crawl(
        &server.url("/docs/index.html"),
        &CrawlOptions {
            max_depth: 1,
            ..options()
        },
    )
    .expect("crawl");
    let value = result.to_json();
    assert!(value.get("files").and_then(Json::as_arr).is_some());
    assert_eq!(value.u64_or("pagesVisited", 0), result.pages_visited as u64);
    let first = value.get("files").unwrap().idx(0).unwrap();
    for key in ["url", "filename", "extension", "foundOn", "text"] {
        assert!(first.get(key).is_some(), "missing `{key}` in {first}");
    }
}

/// Where the crawler *goes* and what it *collects* are separate questions. A
/// site's files very often live on a CDN, so those are collected by default —
/// but the restriction is available for anyone who wants it.
#[test]
fn off_host_files_are_collected_unless_asked_otherwise() {
    let server = ServerBuilder::new()
        .route(
            "/index.html",
            Route::File(std::sync::Arc::new({
                let mut file = hdm_testserver::FileRoute::new(
                    br#"<a href="local.zip">local</a>
                        <a href="http://cdn.example.invalid/remote.zip">on a CDN</a>"#
                        .to_vec(),
                );
                file.content_type = Some("text/html".into());
                file
            })),
        )
        .file("/local.zip", test_data(8))
        .start();

    let default = crawl(&server.url("/index.html"), &options()).expect("crawl");
    assert_eq!(
        urls(&default),
        ["local.zip", "remote.zip"],
        "a file on a CDN is still the site's file"
    );

    let restricted = crawl(
        &server.url("/index.html"),
        &CrawlOptions {
            files_same_host: true,
            ..options()
        },
    )
    .expect("crawl");
    assert_eq!(urls(&restricted), ["local.zip"]);
}
