//! A native window around the interface the daemon already serves.
//!
//! The shell owns no interface of its own. It finds the daemon, or starts one,
//! and points a window at it — so there is exactly one implementation of the
//! interface and a fix made for browser users is a fix here too.
//!
//! **Unverified.** Tauri could not be fetched in the environment Hydra was
//! developed in, so this is written against Tauri v2's documented API but has
//! never been compiled.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The daemon this shell started, if it started one.
///
/// A daemon that was already running belongs to the user and is left alone on
/// exit; one this process spawned is ours to clean up.
struct OwnedDaemon(Mutex<Option<Child>>);

fn main() {
    let daemon = OwnedDaemon(Mutex::new(None));

    let connection = match find_or_start_daemon(&daemon) {
        Ok(connection) => connection,
        Err(message) => {
            eprintln!("Hydra: {message}");
            // Without a daemon there is nothing to show, and a blank window
            // would be worse than a clear message.
            std::process::exit(1);
        }
    };

    tauri::Builder::default()
        .setup(move |app| {
            use tauri::WebviewUrl;
            let url = connection.parse().map_err(|e| format!("bad daemon URL: {e}"))?;
            tauri::WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Hydra Download Manager")
                .inner_size(1180.0, 780.0)
                .min_inner_size(720.0, 480.0)
                .resizable(true)
                .build()?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Stop the daemon only if this process started it.
                if let Some(state) = window.app_handle().try_state::<OwnedDaemon>() {
                    if let Ok(mut guard) = state.0.lock() {
                        if let Some(mut child) = guard.take() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                }
            }
        })
        .manage(daemon)
        .run(tauri::generate_context!())
        .expect("the Hydra window could not be created");
}

/// Returns the URL of a running daemon, starting one if necessary.
fn find_or_start_daemon(daemon: &OwnedDaemon) -> Result<String, String> {
    if let Some(url) = read_daemon_url() {
        return Ok(url);
    }

    let executable = find_daemon_binary()
        .ok_or("could not find hdmd. Install it, or put it next to this application.")?;
    let child = Command::new(&executable)
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", executable.display()))?;
    *daemon.0.lock().unwrap() = Some(child);

    // The daemon publishes its port once it is listening.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(url) = read_daemon_url() {
            return Ok(url);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err("hdmd was started but never reported a port".into())
}

fn read_daemon_url() -> Option<String> {
    let text = std::fs::read_to_string(data_dir().join("daemon.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("url")?.as_str().map(str::to_string)
}

fn find_daemon_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "hdmd.exe" } else { "hdmd" };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    candidates.push(PathBuf::from(name));
    candidates.into_iter().find(|path| path.exists() || path.as_os_str() == name)
}

/// Mirrors `hdm_core::platform::data_dir`.
fn data_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os("HYDRA_DATA_DIR") {
        return PathBuf::from(explicit);
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("Hydra"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/Hydra"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|base| base.join("hydra"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}
