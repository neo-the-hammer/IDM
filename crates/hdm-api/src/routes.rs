//! The REST API.
//!
//! Every route returns JSON and a status code. Errors are `{"error": "..."}`
//! so the UI and the CLI can render them without guessing.

use crate::server::HttpRequest;
use hdm_core::batch;
use hdm_core::engine::{DownloadSpec, MAX_CONNECTIONS};
use hdm_core::manager::Manager;
use hdm_core::media::{self, MediaSelection};
use hdm_core::platform;
use hdm_core::plugins;
use hdm_core::queue::Queue;
use hdm_core::spider::{self, CrawlOptions};
use hdm_core::store::Settings;
use hdm_json::{json, Json};
use std::path::PathBuf;
use std::sync::Arc;

fn ok(value: Json) -> (u16, Json) {
    (200, value)
}

fn error(status: u16, message: impl Into<String>) -> (u16, Json) {
    (status, json!({ "error": (message.into()) }))
}

/// Routes one request.
pub fn dispatch(manager: &Arc<Manager>, request: &HttpRequest, version: &str) -> (u16, Json) {
    let segments = request.segments();
    // Everything lives under /api/v1.
    let path: Vec<&str> = match segments.as_slice() {
        ["api", "v1", rest @ ..] => rest.to_vec(),
        _ => return error(404, "unknown endpoint"),
    };

    match (request.method.as_str(), path.as_slice()) {
        ("GET", ["health"]) => ok(json!({"status": "ok", "version": version})),

        ("GET", ["version"]) => ok(json!({
            "name": "Hydra Download Manager",
            "version": version,
            "maxConnections": (MAX_CONNECTIONS)
        })),

        ("GET", ["totals"]) => ok(manager.totals()),

        // ------------------------------------------------------- downloads
        ("GET", ["downloads"]) => ok(json!({
            "downloads": (Json::Arr(manager.snapshot())),
            "totals": (manager.totals())
        })),

        ("POST", ["downloads"]) => add_download(manager, request),

        ("GET", ["downloads", id]) => match manager.snapshot_one(id) {
            Some(value) => ok(value),
            None => error(404, format!("no download {id}")),
        },

        ("DELETE", ["downloads", id]) => {
            let delete_files = request.query_flag("deleteFiles");
            match manager.remove(id, delete_files) {
                Ok(()) => ok(json!({"removed": (*id)})),
                Err(e) => error(404, e),
            }
        }

        ("POST", ["downloads", id, action]) => {
            let result = match *action {
                "start" | "resume" => manager.start(id),
                "pause" => manager.pause(id),
                "cancel" => manager.cancel(id),
                "restart" => manager.restart(id),
                "reveal" => manager.reveal(id),
                "limit" => set_limit(manager, id, request),
                "queue" => set_queue(manager, id, request),
                other => return error(404, format!("unknown action `{other}`")),
            };
            match result {
                Ok(()) => match manager.snapshot_one(id) {
                    Some(value) => ok(value),
                    None => ok(json!({"ok": true})),
                },
                Err(e) => error(400, e),
            }
        }

        // -------------------------------------------------- bulk operations
        ("POST", ["downloads-pause-all"]) => {
            manager.pause_all();
            ok(json!({"ok": true}))
        }
        ("POST", ["downloads-resume-all"]) => {
            manager.resume_all();
            ok(json!({"ok": true}))
        }
        ("POST", ["downloads-clear-completed"]) => {
            ok(json!({"removed": (manager.clear_completed() as u64)}))
        }

        // ---------------------------------------------------------- settings
        ("GET", ["settings"]) => ok(manager.settings().to_json()),

        ("PUT", ["settings"]) => match request.json() {
            Ok(value) => {
                let root = manager.settings().categories.root.clone();
                manager.set_settings(Settings::from_json(&value, &root));
                ok(manager.settings().to_json())
            }
            Err(e) => error(400, e),
        },

        ("GET", ["categories"]) => ok(manager.categories().to_json()),

        // ------------------------------------------------------------ queues
        ("GET", ["queues"]) => ok(json!({
            "queues": (Json::Arr(manager.queues().iter().map(Queue::to_json).collect()))
        })),

        ("PUT", ["queues", id]) => match request.json() {
            Ok(mut body) => {
                // The id in the path is authoritative, so a mismatched body
                // cannot rename or overwrite a different queue.
                body.insert("id", Json::Str((*id).to_string()));
                match Queue::from_json(&body) {
                    Some(queue) => match manager.put_queue(queue) {
                        Ok(()) => ok(json!({
                            "queues": (Json::Arr(
                                manager.queues().iter().map(Queue::to_json).collect()
                            ))
                        })),
                        Err(e) => error(400, e),
                    },
                    None => error(400, "malformed queue"),
                }
            }
            Err(e) => error(400, e),
        },

        ("DELETE", ["queues", id]) => match manager.remove_queue(id) {
            Ok(()) => ok(json!({"removed": (*id)})),
            Err(e) => error(400, e),
        },

        ("POST", ["queues", id, action]) => {
            let result = match *action {
                "pause" => manager.set_queue_paused(id, true),
                "resume" => manager.set_queue_paused(id, false),
                other => return error(404, format!("unknown action `{other}`")),
            };
            match result {
                Ok(()) => ok(json!({
                    "queues": (Json::Arr(manager.queues().iter().map(Queue::to_json).collect()))
                })),
                Err(e) => error(400, e),
            }
        }

        // ------------------------------------------------ batch and grabber
        ("POST", ["expand"]) => expand_pattern(request),

        ("POST", ["crawl"]) => run_crawl(request),

        // ----------------------------------------------------- media grabber
        ("POST", ["media", "probe"]) => probe_media(request),

        ("POST", ["media", "download"]) => add_media(manager, request),

        ("POST", ["downloads-batch"]) => add_batch(manager, request),

        ("GET", ["plugins"]) => ok(plugins::status()),

        ("GET", ["defaults"]) => ok(json!({
            "downloadDir": (platform::default_download_dir().to_string_lossy().into_owned()),
            "dataDir": (platform::data_dir().to_string_lossy().into_owned())
        })),

        _ => error(404, "unknown endpoint"),
    }
}

fn add_download(manager: &Arc<Manager>, request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let spec = match spec_from_body(&body) {
        Ok(spec) => spec,
        Err(failure) => return failure,
    };

    let category = body
        .get("category")
        .and_then(Json::as_str)
        .map(str::to_string);
    let autostart = body.bool_or("autostart", true);
    let id = manager.add(spec, category, autostart);

    match manager.snapshot_one(&id) {
        Some(value) => (201, value),
        None => (201, json!({"id": (id.as_str())})),
    }
}

/// Builds a download from a request body.
///
/// Shared by the plain and the media routes: a stream download takes every
/// option a file download takes — directory, headers, proxy, credentials,
/// speed limit — and differs only in carrying a selection alongside them.
fn spec_from_body(body: &Json) -> Result<DownloadSpec, (u16, Json)> {
    let Some(url) = body.get("url").and_then(Json::as_str) else {
        return Err(error(400, "a `url` is required"));
    };
    // Validate before storing, so a typo is reported now rather than surfacing
    // as a mysterious failure when the download eventually runs.
    let parsed = match hdm_net::url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(e) => return Err(error(400, format!("invalid URL: {e}"))),
    };
    if !matches!(parsed.scheme.as_str(), "http" | "https" | "ftp" | "ftps") {
        return Err(error(
            400,
            format!("unsupported scheme `{}`", parsed.scheme),
        ));
    }

    let directory = body
        .get("directory")
        .and_then(Json::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();

    let mut spec = DownloadSpec::new(url, directory);
    spec.filename = body
        .get("filename")
        .and_then(Json::as_str)
        .map(str::to_string);
    spec.connections = body.u64_or("connections", 0) as u8;
    spec.max_retries = body.u64_or("maxRetries", 0) as u32;
    spec.overwrite = body.bool_or("overwrite", false);
    spec.tls_insecure = body.bool_or("tlsInsecure", false);
    spec.speed_limit = body.u64_or("speedLimit", 0);
    spec.proxy = body.get("proxy").and_then(Json::as_str).map(str::to_string);
    spec.username = body
        .get("username")
        .and_then(Json::as_str)
        .map(str::to_string);
    spec.password = body
        .get("password")
        .and_then(Json::as_str)
        .map(str::to_string);
    spec.mirrors = body
        .get("mirrors")
        .and_then(Json::as_arr)
        .map(|items| {
            items
                .iter()
                .filter_map(|m| m.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    if let Some(digest) = body.get("checksum").and_then(Json::as_str) {
        let algo = body
            .get("checksumAlgo")
            .and_then(Json::as_str)
            .and_then(hdm_crypto::HashAlgo::parse)
            // A bare digest usually arrives with no algorithm named; its length
            // identifies it unambiguously.
            .or_else(|| hdm_crypto::HashAlgo::from_hex_len(digest));
        match algo {
            Some(algo) => spec.checksum = Some((algo, digest.trim().to_ascii_lowercase())),
            None => {
                return Err(error(
                    400,
                    "cannot tell which checksum algorithm that digest is",
                ))
            }
        }
    }

    // Headers the browser extension replays: cookies, referer, user-agent.
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(list) = body.get("headers").and_then(Json::as_arr) {
        for item in list {
            if let (Some(name), Some(value)) = (
                item.get("name").and_then(Json::as_str),
                item.get("value").and_then(Json::as_str),
            ) {
                headers.push((name.to_string(), value.to_string()));
            }
        }
    }
    for (key, header) in [
        ("cookies", "Cookie"),
        ("referer", "Referer"),
        ("userAgent", "User-Agent"),
    ] {
        if let Some(value) = body.get(key).and_then(Json::as_str) {
            if !value.is_empty() {
                headers.push((header.to_string(), value.to_string()));
            }
        }
    }
    // A header value containing CR or LF could inject further headers into
    // every request this download makes.
    headers.retain(|(_, value)| hdm_net::headers::is_safe_header_value(value));
    spec.headers = headers;
    Ok(spec)
}

/// Reads a streaming manifest and reports what it offers.
///
/// Separate from starting the download, for the same reason batch expansion is:
/// a manifest usually offers half a dozen qualities, and choosing is the whole
/// point.
fn probe_media(request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let spec = match spec_from_body(&body) {
        Ok(spec) => spec,
        Err(failure) => return failure,
    };
    match media::probe(&spec) {
        Ok(probe) => ok(probe.to_json()),
        Err(e) => error(400, e),
    }
}

/// Adds a stream as a download.
///
/// With no stream named, the best video is taken — and its audio too when the
/// manifest keeps them apart, which is what someone who just wants the video
/// means.
fn add_media(manager: &Arc<Manager>, request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let mut spec = match spec_from_body(&body) {
        Ok(spec) => spec,
        Err(failure) => return failure,
    };

    let remux = body.bool_or("remux", false);
    let selection = match body.get("streamId").and_then(Json::as_str) {
        // An explicit choice is taken as given, with no second fetch of the
        // manifest to second-guess it.
        Some(stream) => {
            let format = match body.get("format").and_then(Json::as_str) {
                Some(format) => format.to_string(),
                None => return error(400, "`format` is required alongside `streamId`"),
            };
            let mut selection = MediaSelection::new(format, spec.url.clone());
            selection.stream_id = Some(stream.to_string());
            selection.audio_url = body
                .get("audioUrl")
                .and_then(Json::as_str)
                .map(str::to_string);
            selection.audio_stream_id = body
                .get("audioStreamId")
                .and_then(Json::as_str)
                .map(str::to_string);
            selection.remux = remux;
            selection
        }
        None => {
            let probe = match media::probe(&spec) {
                Ok(probe) => probe,
                Err(e) => return error(400, e),
            };
            let Some(best) = probe.best() else {
                return error(400, "that manifest offers nothing to download");
            };
            let mut selection = MediaSelection::new(probe.format.clone(), best.url.clone());
            selection.stream_id = Some(best.id.clone());
            if let Some(audio) = probe.best_audio() {
                selection.audio_url = Some(audio.url.clone());
                selection.audio_stream_id = Some(audio.id.clone());
            }
            selection.remux = remux;
            selection
        }
    };
    spec.media = Some(selection);

    let category = body
        .get("category")
        .and_then(Json::as_str)
        .map(str::to_string);
    let autostart = body.bool_or("autostart", true);
    let id = manager.add(spec, category, autostart);

    match manager.snapshot_one(&id) {
        Some(value) => (201, value),
        None => (201, json!({"id": (id.as_str())})),
    }
}

/// Moves a download into a queue. A null or absent queue means the default.
fn set_queue(manager: &Arc<Manager>, id: &str, request: &HttpRequest) -> Result<(), String> {
    let body = request.json()?;
    let queue = body.get("queue").and_then(Json::as_str);
    manager.set_queue(id, queue)
}

fn set_limit(manager: &Arc<Manager>, id: &str, request: &HttpRequest) -> Result<(), String> {
    let body = request.json()?;
    let rate = body
        .get("bytesPerSecond")
        .and_then(Json::as_u64)
        .ok_or_else(|| "`bytesPerSecond` is required".to_string())?;
    manager.set_download_limit(id, rate)
}

/// Previews what a batch pattern stands for, without adding anything.
///
/// Separate from adding on purpose: a mistyped range can mean hundreds of
/// downloads, and seeing the list first is the difference between a useful
/// feature and a trap.
fn expand_pattern(request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let Some(pattern) = body.get("pattern").and_then(Json::as_str) else {
        return error(400, "a `pattern` is required");
    };
    match batch::expand(pattern) {
        Ok(urls) => ok(json!({
            "count": (urls.len() as u64),
            "urls": (Json::Arr(urls.into_iter().map(Json::Str).collect()))
        })),
        Err(e) => error(400, e.to_string()),
    }
}

/// Walks a site and reports the files on it.
fn run_crawl(request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let Some(url) = body.get("url").and_then(Json::as_str) else {
        return error(400, "a `url` is required");
    };
    let options = CrawlOptions::from_json(&body);
    match spider::crawl(url, &options) {
        Ok(result) => ok(result.to_json()),
        Err(e) => error(400, e),
    }
}

/// Adds a list of URLs in one call.
fn add_batch(manager: &Arc<Manager>, request: &HttpRequest) -> (u16, Json) {
    let body = match request.json() {
        Ok(value) => value,
        Err(e) => return error(400, e),
    };
    let Some(items) = body.get("urls").and_then(Json::as_arr) else {
        return error(400, "a `urls` array is required");
    };
    if items.len() > batch::MAX_EXPANSION {
        return error(
            400,
            format!("at most {} URLs at a time", batch::MAX_EXPANSION),
        );
    }

    let directory = body
        .get("directory")
        .and_then(Json::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    let connections = body.u64_or("connections", 0) as u8;
    let referer = body.get("referer").and_then(Json::as_str);

    let mut specs = Vec::with_capacity(items.len());
    let mut rejected = Vec::new();
    for item in items {
        // Entries may be bare URLs or objects carrying the page they came
        // from, which the site grabber supplies as a Referer.
        let (url, item_referer) = match item {
            Json::Str(url) => (url.as_str(), referer),
            other => (
                other.get("url").and_then(Json::as_str).unwrap_or(""),
                other.get("foundOn").and_then(Json::as_str).or(referer),
            ),
        };
        match hdm_net::url::Url::parse(url) {
            Ok(parsed) if matches!(parsed.scheme.as_str(), "http" | "https" | "ftp" | "ftps") => {
                let mut spec = DownloadSpec::new(url, directory.clone());
                spec.connections = connections;
                if let Some(referer) = item_referer {
                    if hdm_net::headers::is_safe_header_value(referer) {
                        spec.headers.push(("Referer".into(), referer.to_string()));
                    }
                }
                specs.push(spec);
            }
            // One bad entry in a few hundred should not lose the rest; report
            // which ones were skipped instead.
            _ => rejected.push(Json::Str(url.to_string())),
        }
    }

    let category = body
        .get("category")
        .and_then(Json::as_str)
        .map(str::to_string);
    let queue = body.get("queue").and_then(Json::as_str).map(str::to_string);
    let autostart = body.bool_or("autostart", true);
    let added = manager.add_many(specs, category, queue, autostart);

    (
        201,
        json!({
            "added": (added.len() as u64),
            "ids": (Json::Arr(added.into_iter().map(Json::Str).collect())),
            "rejected": (Json::Arr(rejected))
        }),
    )
}
