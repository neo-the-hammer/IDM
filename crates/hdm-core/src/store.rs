//! The persistent download list and settings.
//!
//! Plain JSON on disk rather than a database: the dataset is a few thousand
//! records at most, and a file a user can read, diff and hand-edit when
//! something goes wrong is worth more here than query power.

use crate::category::Categories;
use crate::engine::{DownloadSpec, Status, MAX_CONNECTIONS};
use crate::queue::{default_queues, Queue, MAIN_QUEUE};
use hdm_crypto::HashAlgo;
use hdm_json::{json, parse, Json};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Bumped if the on-disk shape ever changes incompatibly.
const FORMAT_VERSION: u64 = 1;

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A short, unguessable identifier for a download.
pub fn new_id() -> String {
    hdm_crypto::random_token(12).unwrap_or_else(|_| format!("id-{}", now_secs()))
}

/// One download as it is stored and shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadRecord {
    pub id: String,
    pub spec: DownloadSpec,
    pub status: Status,
    pub filename: String,
    pub output_path: Option<PathBuf>,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub error: Option<String>,
    pub created_at: u64,
    pub completed_at: Option<u64>,
    /// Explicit category, overriding classification by extension.
    pub category: Option<String>,
    /// The queue this belongs to; `None` means it starts immediately.
    pub queue: Option<String>,
    /// Where the link came from, shown in the UI and used as a Referer.
    pub source_page: Option<String>,
    pub content_type: Option<String>,
    pub added_by: Option<String>,
}

impl DownloadRecord {
    pub fn new(spec: DownloadSpec) -> DownloadRecord {
        DownloadRecord {
            id: new_id(),
            filename: String::new(),
            spec,
            status: Status::Queued,
            output_path: None,
            total: None,
            downloaded: 0,
            error: None,
            created_at: now_secs(),
            completed_at: None,
            category: None,
            queue: None,
            source_page: None,
            content_type: None,
            added_by: None,
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "id": (self.id.as_str()),
            "spec": (spec_to_json(&self.spec)),
            "status": (self.status.as_str()),
            "filename": (self.filename.as_str()),
            "outputPath": (self.output_path.as_ref().map(|p| p.to_string_lossy().into_owned())),
            "total": (self.total),
            "downloaded": (self.downloaded),
            "error": (self.error.clone()),
            "createdAt": (self.created_at),
            "completedAt": (self.completed_at),
            "category": (self.category.clone()),
            "queue": (self.queue.clone()),
            "sourcePage": (self.source_page.clone()),
            "contentType": (self.content_type.clone()),
            "addedBy": (self.added_by.clone())
        })
    }

    pub fn from_json(value: &Json) -> Option<DownloadRecord> {
        let status = match value
            .get("status")
            .and_then(Json::as_str)
            .unwrap_or("queued")
        {
            "completed" => Status::Completed,
            "failed" => Status::Failed,
            "cancelled" => Status::Cancelled,
            "paused" => Status::Paused,
            // Anything that was mid-flight when the daemon stopped comes back
            // as queued, never as "downloading": no transfer is running yet.
            _ => Status::Queued,
        };
        Some(DownloadRecord {
            id: value.get("id")?.as_str()?.to_string(),
            spec: spec_from_json(value.get("spec")?)?,
            status,
            filename: value.str_or("filename", "").to_string(),
            output_path: value
                .get("outputPath")
                .and_then(Json::as_str)
                .map(PathBuf::from),
            total: value.get("total").and_then(Json::as_u64),
            downloaded: value.u64_or("downloaded", 0),
            error: value
                .get("error")
                .and_then(Json::as_str)
                .map(str::to_string),
            created_at: value.u64_or("createdAt", now_secs()),
            completed_at: value.get("completedAt").and_then(Json::as_u64),
            category: value
                .get("category")
                .and_then(Json::as_str)
                .map(str::to_string),
            queue: value
                .get("queue")
                .and_then(Json::as_str)
                .map(str::to_string),
            source_page: value
                .get("sourcePage")
                .and_then(Json::as_str)
                .map(str::to_string),
            content_type: value
                .get("contentType")
                .and_then(Json::as_str)
                .map(str::to_string),
            added_by: value
                .get("addedBy")
                .and_then(Json::as_str)
                .map(str::to_string),
        })
    }
}

pub fn spec_to_json(spec: &DownloadSpec) -> Json {
    json!({
        "url": (spec.url.as_str()),
        "mirrors": (Json::Arr(spec.mirrors.iter().map(|m| Json::Str(m.clone())).collect())),
        "directory": (spec.directory.to_string_lossy().into_owned()),
        "filename": (spec.filename.clone()),
        "connections": (spec.connections),
        "headers": (Json::Arr(
            spec.headers
                .iter()
                .map(|(k, v)| json!({"name": (k.as_str()), "value": (v.as_str())}))
                .collect()
        )),
        "username": (spec.username.clone()),
        // The password is stored because a scheduled download has to be able to
        // authenticate itself hours later with nobody present. The state file
        // is created with owner-only permissions for exactly this reason.
        "password": (spec.password.clone()),
        "checksumAlgo": (spec.checksum.as_ref().map(|(a, _)| a.name().to_string())),
        "checksum": (spec.checksum.as_ref().map(|(_, v)| v.clone())),
        "overwrite": (spec.overwrite),
        "maxRetries": (spec.max_retries),
        "tlsInsecure": (spec.tls_insecure),
        "proxy": (spec.proxy.clone()),
        "speedLimit": (spec.speed_limit),
        "media": (spec.media.as_ref().map(|m| m.to_json()))
    })
}

pub fn spec_from_json(value: &Json) -> Option<DownloadSpec> {
    let checksum = match (
        value
            .get("checksumAlgo")
            .and_then(Json::as_str)
            .and_then(HashAlgo::parse),
        value.get("checksum").and_then(Json::as_str),
    ) {
        (Some(algo), Some(digest)) => Some((algo, digest.to_string())),
        _ => None,
    };
    Some(DownloadSpec {
        url: value.get("url")?.as_str()?.to_string(),
        mirrors: value
            .get("mirrors")
            .and_then(Json::as_arr)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| m.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        directory: PathBuf::from(value.get("directory")?.as_str()?),
        filename: value
            .get("filename")
            .and_then(Json::as_str)
            .map(str::to_string),
        connections: (value.u64_or("connections", 8) as u8).clamp(1, MAX_CONNECTIONS),
        headers: value
            .get("headers")
            .and_then(Json::as_arr)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|h| {
                        Some((
                            h.get("name")?.as_str()?.to_string(),
                            h.get("value")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
        username: value
            .get("username")
            .and_then(Json::as_str)
            .map(str::to_string),
        password: value
            .get("password")
            .and_then(Json::as_str)
            .map(str::to_string),
        checksum,
        overwrite: value.bool_or("overwrite", false),
        max_retries: value.u64_or("maxRetries", 5) as u32,
        tls_insecure: value.bool_or("tlsInsecure", false),
        proxy: value
            .get("proxy")
            .and_then(Json::as_str)
            .map(str::to_string),
        speed_limit: value.u64_or("speedLimit", 0),
        media: value
            .get("media")
            .and_then(crate::media::MediaSelection::from_json),
    })
}

/// Daemon-wide settings.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Global bandwidth cap in bytes per second; 0 means unlimited.
    pub speed_limit: u64,
    /// Default connections per download.
    pub connections: u8,
    /// How many downloads may transfer at once.
    pub max_concurrent: u8,
    pub max_retries: u32,
    pub categories: Categories,
    /// Watch the clipboard for copied links.
    pub clipboard_monitor: bool,
    /// Show a desktop notification when a download finishes.
    pub notifications: bool,
    /// UI language tag, e.g. `en` or `fa`.
    pub language: String,
    /// Selected theme id.
    pub theme: String,
    /// Proxy applied to downloads that do not specify one.
    pub proxy: Option<String>,
    /// Named queues, each with its own schedule and limits.
    pub queues: Vec<Queue>,
}

impl Settings {
    pub fn new(download_root: PathBuf) -> Settings {
        Settings {
            speed_limit: 0,
            connections: 8,
            max_concurrent: 4,
            max_retries: 5,
            categories: Categories::new(download_root),
            clipboard_monitor: false,
            notifications: true,
            language: "en".into(),
            theme: "hydra-dark".into(),
            proxy: None,
            queues: default_queues(),
        }
    }

    /// Looks up a queue, falling back to the main one so a download whose
    /// queue was deleted still runs instead of being stranded.
    pub fn queue_for(&self, id: Option<&str>) -> &Queue {
        let wanted = id.unwrap_or(MAIN_QUEUE);
        self.queues
            .iter()
            .find(|q| q.id == wanted)
            .or_else(|| self.queues.iter().find(|q| q.id == MAIN_QUEUE))
            .or_else(|| self.queues.first())
            .expect("there is always at least one queue")
    }

    pub fn to_json(&self) -> Json {
        json!({
            "speedLimit": (self.speed_limit),
            "connections": (self.connections),
            "maxConcurrent": (self.max_concurrent),
            "maxRetries": (self.max_retries),
            "categories": (self.categories.to_json()),
            "clipboardMonitor": (self.clipboard_monitor),
            "notifications": (self.notifications),
            "language": (self.language.as_str()),
            "theme": (self.theme.as_str()),
            "proxy": (self.proxy.clone()),
            "queues": (Json::Arr(self.queues.iter().map(Queue::to_json).collect()))
        })
    }

    pub fn from_json(value: &Json, fallback_root: &Path) -> Settings {
        let mut settings = Settings::new(fallback_root.to_path_buf());
        settings.speed_limit = value.u64_or("speedLimit", 0);
        settings.connections = (value.u64_or("connections", 8) as u8).clamp(1, MAX_CONNECTIONS);
        settings.max_concurrent = (value.u64_or("maxConcurrent", 4) as u8).clamp(1, 64);
        settings.max_retries = value.u64_or("maxRetries", 5) as u32;
        settings.clipboard_monitor = value.bool_or("clipboardMonitor", false);
        settings.notifications = value.bool_or("notifications", true);
        settings.language = value.str_or("language", "en").to_string();
        settings.theme = value.str_or("theme", "hydra-dark").to_string();
        settings.proxy = value
            .get("proxy")
            .and_then(Json::as_str)
            .map(str::to_string);
        if let Some(categories) = value.get("categories") {
            settings.categories = Categories::from_json(categories, fallback_root);
        }
        if let Some(queues) = value.get("queues").and_then(Json::as_arr) {
            let parsed: Vec<Queue> = queues.iter().filter_map(Queue::from_json).collect();
            // Never end up with no queues at all: a download with nowhere to
            // run would sit queued forever with no way to fix it from the UI.
            if !parsed.is_empty() {
                settings.queues = parsed;
            }
        }
        settings
    }
}

/// The on-disk state: downloads plus settings.
pub struct Store {
    path: PathBuf,
    pub downloads: Vec<DownloadRecord>,
    pub settings: Settings,
}

impl Store {
    /// Loads state, or starts fresh if there is none.
    ///
    /// A damaged state file is moved aside rather than deleted, so a user can
    /// recover their download list by hand if the parse failure was our fault.
    pub fn load(path: &Path, download_root: PathBuf) -> Store {
        let fallback = || Store {
            path: path.to_path_buf(),
            downloads: Vec::new(),
            settings: Settings::new(download_root.clone()),
        };

        let Ok(text) = std::fs::read_to_string(path) else {
            return fallback();
        };
        let Ok(value) = parse(&text) else {
            let _ = std::fs::rename(path, path.with_extension("json.corrupt"));
            return fallback();
        };
        if value.get("version").and_then(Json::as_u64) != Some(FORMAT_VERSION) {
            let _ = std::fs::rename(path, path.with_extension("json.old"));
            return fallback();
        }

        let downloads = value
            .get("downloads")
            .and_then(Json::as_arr)
            .map(|items| items.iter().filter_map(DownloadRecord::from_json).collect())
            .unwrap_or_default();
        let settings = value
            .get("settings")
            .map(|s| Settings::from_json(s, &download_root))
            .unwrap_or_else(|| Settings::new(download_root.clone()));

        Store {
            path: path.to_path_buf(),
            downloads,
            settings,
        }
    }

    /// Writes state atomically.
    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let document = json!({
            "version": FORMAT_VERSION,
            "savedAt": (now_secs()),
            "settings": (self.settings.to_json()),
            "downloads": (Json::Arr(self.downloads.iter().map(DownloadRecord::to_json).collect()))
        });

        // Temp file plus rename, so a crash mid-write cannot leave a truncated
        // list that loses every download.
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, document.to_string_pretty())?;
        restrict_permissions(&temp);
        std::fs::rename(&temp, &self.path)
    }

    pub fn get(&self, id: &str) -> Option<&DownloadRecord> {
        self.downloads.iter().find(|d| d.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut DownloadRecord> {
        self.downloads.iter_mut().find(|d| d.id == id)
    }

    pub fn insert(&mut self, record: DownloadRecord) -> String {
        let id = record.id.clone();
        self.downloads.push(record);
        id
    }

    pub fn remove(&mut self, id: &str) -> Option<DownloadRecord> {
        let index = self.downloads.iter().position(|d| d.id == id)?;
        Some(self.downloads.remove(index))
    }

    /// Drops finished entries, for the UI's "clear completed" action.
    pub fn clear_completed(&mut self) -> usize {
        let before = self.downloads.len();
        self.downloads.retain(|d| d.status != Status::Completed);
        before - self.downloads.len()
    }
}

/// Restricts a state file to its owner.
///
/// It holds site passwords and the API token, so a world-readable file on a
/// shared machine would hand both to any local user.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        // On Windows the file inherits the user profile directory's ACL, which
        // is already owner-only.
        let _ = path;
    }
}
