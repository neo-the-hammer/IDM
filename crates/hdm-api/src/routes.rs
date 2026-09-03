//! The REST API.
//!
//! Every route returns JSON and a status code. Errors are `{"error": "..."}`
//! so the UI and the CLI can render them without guessing.

use crate::server::HttpRequest;
use hdm_core::engine::{DownloadSpec, MAX_CONNECTIONS};
use hdm_core::manager::Manager;
use hdm_core::platform;
use hdm_core::queue::Queue;
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

    let Some(url) = body.get("url").and_then(Json::as_str) else {
        return error(400, "a `url` is required");
    };
    // Validate before storing, so a typo is reported now rather than surfacing
    // as a mysterious failure when the download eventually runs.
    let parsed = match hdm_net::url::Url::parse(url) {
        Ok(parsed) => parsed,
        Err(e) => return error(400, format!("invalid URL: {e}")),
    };
    if !matches!(parsed.scheme.as_str(), "http" | "https" | "ftp" | "ftps") {
        return error(400, format!("unsupported scheme `{}`", parsed.scheme));
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
            None => return error(400, "cannot tell which checksum algorithm that digest is"),
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
