//! Owns the download list and runs transfers.
//!
//! Everything above this — the REST API, the CLI, the browser extension — goes
//! through the manager rather than touching the engine directly, so there is
//! exactly one place that decides what is running and one place that persists
//! state.

use crate::category::Categories;
use crate::engine::{self, DownloadSpec, Outcome, Shared, Status};
use crate::platform;
use crate::resume::sidecar_path_for;
use crate::store::{now_secs, DownloadRecord, Settings, Store};
use crate::throttle::Throttle;
use crate::writer::part_path_for;
use hdm_json::{json, Json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the manager reaps finished transfers and starts queued ones.
const TICK: Duration = Duration::from_millis(400);

struct Active {
    shared: Arc<Shared>,
    throttle: Arc<Throttle>,
    handle: JoinHandle<()>,
}

struct State {
    store: Store,
    active: HashMap<String, Active>,
}

pub struct Manager {
    state: Mutex<State>,
    /// The global bandwidth cap, shared by every download.
    global_throttle: Arc<Throttle>,
    state_path: PathBuf,
    running: AtomicBool,
}

impl Manager {
    /// Loads the saved state from `data_dir`.
    pub fn load(data_dir: &Path, download_root: PathBuf) -> Arc<Manager> {
        let state_path = data_dir.join("state.json");
        let store = Store::load(&state_path, download_root);
        let global_throttle = Arc::new(Throttle::new(store.settings.speed_limit));

        Arc::new(Manager {
            state: Mutex::new(State {
                store,
                active: HashMap::new(),
            }),
            global_throttle,
            state_path,
            running: AtomicBool::new(true),
        })
    }

    /// Starts the background loop that reaps and schedules transfers.
    pub fn spawn_scheduler(self: &Arc<Manager>) -> JoinHandle<()> {
        let manager = self.clone();
        std::thread::spawn(move || {
            while manager.running.load(Ordering::Acquire) {
                manager.tick();
                std::thread::sleep(TICK);
            }
        })
    }

    /// Adds a download and returns its id.
    ///
    /// The save directory comes from the download's category unless the caller
    /// named one explicitly, which is what makes files sort themselves.
    pub fn add(&self, mut spec: DownloadSpec, category: Option<String>, autostart: bool) -> String {
        let mut state = self.state.lock().unwrap();

        if spec.connections == 0 {
            spec.connections = state.store.settings.connections;
        }
        if spec.max_retries == 0 {
            spec.max_retries = state.store.settings.max_retries;
        }
        if spec.proxy.is_none() {
            spec.proxy = state.store.settings.proxy.clone();
        }

        // The filename is not known until the server is asked, so route on the
        // URL's last segment and let the engine refine it. Downloads whose
        // final name lands in a different category are moved on completion.
        let guess = hdm_net::url::Url::parse(&spec.url)
            .ok()
            .and_then(|u| u.filename())
            .unwrap_or_default();
        if spec.directory.as_os_str().is_empty() {
            spec.directory = state
                .store
                .settings
                .categories
                .directory_for(&guess, category.as_deref());
        }

        let mut record = DownloadRecord::new(spec);
        record.category = category;
        record.status = if autostart {
            Status::Queued
        } else {
            Status::Paused
        };
        let id = state.store.insert(record);
        let _ = state.store.save();
        id
    }

    /// Marks a download runnable. The scheduler picks it up on the next tick.
    pub fn start(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if state.active.contains_key(id) {
            return Ok(());
        }
        let record = state
            .store
            .get_mut(id)
            .ok_or_else(|| format!("no download {id}"))?;
        record.status = Status::Queued;
        record.error = None;
        let _ = state.store.save();
        Ok(())
    }

    pub fn pause(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.active.get(id) {
            active.shared.pause();
        }
        if let Some(record) = state.store.get_mut(id) {
            if record.status == Status::Queued {
                record.status = Status::Paused;
            }
        } else {
            return Err(format!("no download {id}"));
        }
        let _ = state.store.save();
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.active.get(id) {
            active.shared.cancel();
        }
        let record = state
            .store
            .get_mut(id)
            .ok_or_else(|| format!("no download {id}"))?;
        record.status = Status::Cancelled;
        let _ = state.store.save();
        Ok(())
    }

    /// Removes a download, optionally deleting whatever it produced.
    pub fn remove(&self, id: &str, delete_files: bool) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.active.get(id) {
            active.shared.cancel();
        }
        let record = state
            .store
            .remove(id)
            .ok_or_else(|| format!("no download {id}"))?;

        if delete_files {
            if let Some(path) = &record.output_path {
                let _ = std::fs::remove_file(path);
            }
            // The partial file and its sidecar go too, or a later download of
            // the same URL would silently resume into the deleted one.
            if !record.filename.is_empty() {
                let part = part_path_for(&record.spec.directory.join(&record.filename));
                let _ = std::fs::remove_file(&part);
                let _ = std::fs::remove_file(sidecar_path_for(&part));
            }
        }
        let _ = state.store.save();
        Ok(())
    }

    /// Restarts a download from the beginning, discarding any partial data.
    pub fn restart(&self, id: &str) -> Result<(), String> {
        {
            let state = self.state.lock().unwrap();
            if let Some(active) = state.active.get(id) {
                active.shared.cancel();
            }
        }
        let mut state = self.state.lock().unwrap();
        let record = state
            .store
            .get_mut(id)
            .ok_or_else(|| format!("no download {id}"))?;
        let part = part_path_for(&record.spec.directory.join(&record.filename));
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(sidecar_path_for(&part));
        record.downloaded = 0;
        record.error = None;
        record.status = Status::Queued;
        let _ = state.store.save();
        Ok(())
    }

    pub fn pause_all(&self) {
        let mut state = self.state.lock().unwrap();
        for active in state.active.values() {
            active.shared.pause();
        }
        for record in state.store.downloads.iter_mut() {
            if record.status == Status::Queued {
                record.status = Status::Paused;
            }
        }
        let _ = state.store.save();
    }

    pub fn resume_all(&self) {
        let mut state = self.state.lock().unwrap();
        for record in state.store.downloads.iter_mut() {
            if matches!(record.status, Status::Paused | Status::Failed) {
                record.status = Status::Queued;
                record.error = None;
            }
        }
        let _ = state.store.save();
    }

    pub fn clear_completed(&self) -> usize {
        let mut state = self.state.lock().unwrap();
        let removed = state.store.clear_completed();
        let _ = state.store.save();
        removed
    }

    /// Changes one download's speed cap while it is running.
    ///
    /// IDM lets a user throttle a single large transfer without touching the
    /// others, and holding the per-download limiter here is what makes that
    /// take effect immediately rather than on the next restart.
    pub fn set_download_limit(&self, id: &str, bytes_per_second: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.active.get(id) {
            active.throttle.set_rate(bytes_per_second);
        }
        let record = state
            .store
            .get_mut(id)
            .ok_or_else(|| format!("no download {id}"))?;
        record.spec.speed_limit = bytes_per_second;
        let _ = state.store.save();
        Ok(())
    }

    pub fn settings(&self) -> Settings {
        self.state.lock().unwrap().store.settings.clone()
    }

    /// Applies new settings, taking the global speed limit into effect at once.
    pub fn set_settings(&self, settings: Settings) {
        let mut state = self.state.lock().unwrap();
        self.global_throttle.set_rate(settings.speed_limit);
        state.store.settings = settings;
        let _ = state.store.save();
    }

    pub fn categories(&self) -> Categories {
        self.state.lock().unwrap().store.settings.categories.clone()
    }

    /// A live snapshot of every download, for the API and the UI.
    pub fn snapshot(&self) -> Vec<Json> {
        let state = self.state.lock().unwrap();
        state
            .store
            .downloads
            .iter()
            .map(|record| {
                let live = state.active.get(&record.id).map(|a| &a.shared);
                record_to_json(record, live)
            })
            .collect()
    }

    pub fn snapshot_one(&self, id: &str) -> Option<Json> {
        let state = self.state.lock().unwrap();
        let record = state.store.get(id)?;
        Some(record_to_json(
            record,
            state.active.get(id).map(|a| &a.shared),
        ))
    }

    /// Aggregate figures for the UI's status bar.
    pub fn totals(&self) -> Json {
        let state = self.state.lock().unwrap();
        let speed: u64 = state.active.values().map(|a| a.shared.speed()).sum();
        let counts = |status: Status| {
            state
                .store
                .downloads
                .iter()
                .filter(|d| d.status == status)
                .count() as u64
        };
        json!({
            "speed": speed,
            "active": (state.active.len() as u64),
            "queued": (counts(Status::Queued)),
            "paused": (counts(Status::Paused)),
            "completed": (counts(Status::Completed)),
            "failed": (counts(Status::Failed)),
            "total": (state.store.downloads.len() as u64),
            "speedLimit": (state.store.settings.speed_limit)
        })
    }

    /// Reaps finished transfers and starts queued ones.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap();
        self.reap(&mut state);
        self.launch_queued(&mut state);
    }

    /// Folds finished transfers back into the stored records.
    fn reap(&self, state: &mut State) {
        let finished: Vec<String> = state
            .active
            .iter()
            .filter(|(_, active)| active.handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        if finished.is_empty() {
            return;
        }

        for id in finished {
            let Some(active) = state.active.remove(&id) else {
                continue;
            };
            let shared = active.shared.clone();
            // The thread has already exited, so this cannot block.
            let _ = active.handle.join();

            let categories = state.store.settings.categories.clone();
            let Some(record) = state.store.get_mut(&id) else {
                continue;
            };
            record.status = shared.status();
            record.downloaded = shared.downloaded();
            record.total = shared.total();
            record.error = shared.error();
            if !shared.filename().is_empty() {
                record.filename = shared.filename();
            }
            if let Some(path) = shared.output_path() {
                record.output_path = Some(path);
                record.completed_at = Some(now_secs());
            }
            // A download routed by its URL may belong elsewhere once the real
            // filename is known, so re-check now that we have it.
            let _ = categories;
        }
        let _ = state.store.save();
    }

    /// Starts queued downloads up to the concurrency limit.
    fn launch_queued(&self, state: &mut State) {
        let limit = state.store.settings.max_concurrent as usize;
        if state.active.len() >= limit {
            return;
        }

        let ready: Vec<String> = state
            .store
            .downloads
            .iter()
            .filter(|d| d.status == Status::Queued && !state.active.contains_key(&d.id))
            .take(limit - state.active.len())
            .map(|d| d.id.clone())
            .collect();

        for id in ready {
            let Some(record) = state.store.get_mut(&id) else {
                continue;
            };
            let spec = record.spec.clone();
            record.status = Status::Downloading;
            record.error = None;

            let shared = Arc::new(Shared::new());
            // Each download's own limit nests inside the global one.
            let throttle = Arc::new(Throttle::with_parent(
                spec.speed_limit,
                self.global_throttle.clone(),
            ));

            let handle = {
                let shared = shared.clone();
                let throttle = throttle.clone();
                std::thread::Builder::new()
                    .name(format!("hydra-dl-{id}"))
                    .spawn(move || {
                        let _ = engine::run(&spec, &shared, &throttle);
                    })
                    .expect("cannot spawn a download thread")
            };
            state.active.insert(
                id,
                Active {
                    shared,
                    throttle,
                    handle,
                },
            );
        }
        let _ = state.store.save();
    }

    /// Pauses everything and persists, for a clean shutdown.
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::Release);
        let mut state = self.state.lock().unwrap();
        for active in state.active.values() {
            active.shared.pause();
        }
        // Give in-flight transfers a moment to checkpoint their sidecars.
        let handles: Vec<String> = state.active.keys().cloned().collect();
        for id in handles {
            if let Some(active) = state.active.remove(&id) {
                let _ = active.handle.join();
                if let Some(record) = state.store.get_mut(&id) {
                    record.downloaded = active.shared.downloaded();
                    record.status = match active.shared.status() {
                        Status::Completed => Status::Completed,
                        Status::Failed => Status::Failed,
                        _ => Status::Paused,
                    };
                }
            }
        }
        let _ = state.store.save();
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    /// Opens the folder containing a finished download.
    pub fn reveal(&self, id: &str) -> Result<(), String> {
        let state = self.state.lock().unwrap();
        let record = state
            .store
            .get(id)
            .ok_or_else(|| format!("no download {id}"))?;
        let path = record
            .output_path
            .clone()
            .ok_or_else(|| "this download has not finished yet".to_string())?;
        platform::reveal_in_file_manager(&path).map_err(|e| e.to_string())
    }
}

/// Merges a stored record with its live progress, if it is running.
fn record_to_json(record: &DownloadRecord, live: Option<&Arc<Shared>>) -> Json {
    let mut value = record.to_json();
    if let Some(shared) = live {
        value.insert("status", Json::Str(shared.status().as_str().to_string()));
        value.insert("downloaded", Json::from(shared.downloaded()));
        value.insert("speed", Json::from(shared.speed()));
        value.insert("total", Json::from(shared.total()));
        value.insert("eta", Json::from(shared.eta_seconds()));
        if !shared.filename().is_empty() {
            value.insert("filename", Json::Str(shared.filename()));
        }
        value.insert(
            "segments",
            Json::Arr(
                shared
                    .segment_progress()
                    .into_iter()
                    .map(|(start, end, done)| json!({"start": start, "end": end, "done": done}))
                    .collect(),
            ),
        );
    } else {
        value.insert("speed", Json::from(0u64));
    }
    value
}

/// Convenience for callers that just want a download run to completion.
pub fn download_now(spec: &DownloadSpec) -> std::io::Result<Outcome> {
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::new(spec.speed_limit));
    engine::run(spec, &shared, &throttle)
}
