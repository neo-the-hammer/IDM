//! The plugin bridge, including what happens when a plugin misbehaves.
//!
//! The daemon runs for days. A plugin that hangs, crashes or returns nonsense
//! must be an error the caller can act on, never something that wedges the
//! process — so each of those is exercised here with a stand-in interpreter.

use hdm_core::plugins::{status, PluginHost};
use hdm_json::{json, Json};
use std::time::{Duration, Instant};

/// Points the bridge at a stand-in interpreter that misbehaves on purpose.
///
/// The stand-ins are committed under `tests/fixtures` rather than written here.
/// Writing an executable and then exec'ing it inside a multi-threaded process
/// races with `fork`: another thread's copy of the write descriptor can still
/// be open when `execve` runs, and the kernel answers `ETXTBSY`. Committing
/// them removes the write, and the race with it.
#[cfg(unix)]
fn with_fake_python<T>(fixture: &str, body: impl FnOnce(PluginHost) -> T) -> T {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(fixture);
    assert!(script.is_file(), "missing fixture {}", script.display());
    // Built directly rather than through discovery, which caches its result and
    // so cannot be redirected per test.
    body(PluginHost::for_test(script, plugin_root()))
}

fn plugin_root() -> std::path::PathBuf {
    // The real package, so the discovery probe inside the stand-in succeeds.
    let mut here = std::env::current_dir().unwrap();
    loop {
        if here
            .join("python")
            .join("hdm_plugins")
            .join("__init__.py")
            .is_file()
        {
            return here.join("python");
        }
        if !here.pop() {
            panic!("cannot find the hdm_plugins package from the test's working directory");
        }
    }
}

// ------------------------------------------------------- the real plugin host

#[test]
fn the_real_plugin_host_answers() {
    let Ok(host) = PluginHost::discover() else {
        panic!("the plugin host should be discoverable from the repository");
    };
    let capabilities = host.capabilities().expect("capabilities");
    assert!(capabilities.bool_or("ok", false));
    let actions = capabilities.get("actions").and_then(Json::as_arr).unwrap();
    let names: Vec<&str> = actions.iter().filter_map(Json::as_str).collect();
    for expected in ["links", "media", "ytdlp", "capabilities"] {
        assert!(
            names.contains(&expected),
            "missing action `{expected}` in {names:?}"
        );
    }
}

#[test]
fn it_extracts_links_and_resolves_them() {
    let host = PluginHost::discover().unwrap();
    let html = r#"<html><head><title>Files</title><base href="http://cdn.example.com/d/"></head>
        <body><a href="a.zip">Archive</a><a href="/up.iso">Up</a>
        <a href="javascript:void(0)">skip</a></body></html>"#;
    let result = host.links("http://example.com/page.html", html).unwrap();

    let links = result.get("links").and_then(Json::as_arr).unwrap();
    let urls: Vec<&str> = links
        .iter()
        .filter_map(|l| l.get("url").and_then(Json::as_str))
        .collect();
    assert!(
        urls.contains(&"http://cdn.example.com/d/a.zip"),
        "<base> was ignored: {urls:?}"
    );
    assert!(urls.contains(&"http://cdn.example.com/up.iso"));
    assert!(
        !urls.iter().any(|u| u.starts_with("javascript:")),
        "a javascript: URL is not fetchable and must be dropped"
    );
    assert_eq!(result.str_or("title", ""), "Files");
}

#[test]
fn it_distinguishes_a_stream_from_a_file() {
    let host = PluginHost::discover().unwrap();
    let html = r#"<video src="master.m3u8" poster="thumb.jpg"></video><a href="clip.mp4">c</a>"#;
    let result = host.media("http://example.com/watch/", html).unwrap();
    let media = result.get("media").and_then(Json::as_arr).unwrap();

    let manifest = media
        .iter()
        .find(|m| m.str_or("url", "").ends_with(".m3u8"))
        .unwrap();
    assert!(
        manifest.bool_or("streaming", false),
        "a manifest is not a plain file"
    );
    let clip = media
        .iter()
        .find(|m| m.str_or("url", "").ends_with(".mp4"))
        .unwrap();
    assert!(!clip.bool_or("streaming", true));
    assert!(
        !media
            .iter()
            .any(|m| m.str_or("url", "").ends_with("thumb.jpg")),
        "a poster frame is a picture of the video, not the video"
    );
}

/// yt-dlp is optional, and its absence must be a plain statement rather than a
/// mysterious failure.
#[test]
fn yt_dlp_absence_is_reported_clearly() {
    let host = PluginHost::discover().unwrap();
    let capabilities = host.capabilities().unwrap();
    let ytdlp = capabilities.get("ytdlp").unwrap();

    if ytdlp.bool_or("available", false) {
        // Installed on this machine; nothing to assert about its absence.
        return;
    }
    let reason = ytdlp.str_or("reason", "");
    assert!(
        reason.contains("yt-dlp"),
        "the reason should name the tool: {reason}"
    );
    assert!(
        reason.contains("install"),
        "the reason should say how to fix it: {reason}"
    );

    let error = host.ytdlp("https://example.com/watch").unwrap_err();
    assert!(error.contains("yt-dlp"), "got: {error}");
}

#[test]
fn status_describes_the_plugin_layer() {
    let value = status();
    assert!(value.bool_or("available", false), "{value}");
    assert!(value.get("python").is_some());
    assert!(value.get("capabilities").is_some());
}

// -------------------------------------------------------- misbehaving plugins

/// A plugin caught in a loop must be killed, not left to block the daemon.
#[cfg(unix)]
#[test]
fn a_hanging_plugin_is_killed() {
    with_fake_python("hang.sh", |host| {
        let started = Instant::now();
        let error = host
            .request(json!({"action": "ping"}), Duration::from_millis(600))
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(error.contains("did not reply"), "got: {error}");
        assert!(
            elapsed < Duration::from_secs(5),
            "the caller waited {elapsed:?}"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_crashing_plugin_reports_its_last_words() {
    with_fake_python("crash.sh", |host| {
        let error = host
            .request(json!({"action": "ping"}), Duration::from_secs(5))
            .unwrap_err();
        assert!(
            error.contains("ImportError"),
            "the reason should survive: {error}"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_plugin_that_returns_garbage_is_rejected() {
    with_fake_python("garbage.sh", |host| {
        let error = host
            .request(json!({"action": "ping"}), Duration::from_secs(5))
            .unwrap_err();
        assert!(error.contains("malformed JSON"), "got: {error}");
    });
}

#[cfg(unix)]
#[test]
fn a_plugin_that_says_nothing_is_an_error_not_a_hang() {
    with_fake_python("silent.sh", |host| {
        let error = host
            .request(json!({"action": "ping"}), Duration::from_secs(5))
            .unwrap_err();
        assert!(
            error.contains("without replying") || error.contains("failed"),
            "got: {error}"
        );
    });
}

/// One bad call must not poison the next. A fresh process per request is what
/// buys this, and it is worth confirming rather than assuming.
#[cfg(unix)]
#[test]
fn a_failure_does_not_affect_the_next_call() {
    let host = PluginHost::discover().unwrap();
    let bad = host
        .request(json!({"action": "no-such-action"}), Duration::from_secs(10))
        .unwrap();
    assert!(!bad.bool_or("ok", true));

    let good = host.capabilities().unwrap();
    assert!(
        good.bool_or("ok", false),
        "the host stopped working after one bad request"
    );
}
