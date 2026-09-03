//! `hdmd` — the Hydra background service.
//!
//! Owns the download list, runs transfers, and serves the local API and web UI.
//! Everything else — the CLI, the browser extension, the desktop shell — is a
//! client of this process.

use hdm_api::ApiServer;
use hdm_core::manager::Manager;
use hdm_core::platform::{self, InstanceLock};
use hdm_json::{json, parse, Json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The default port. Chosen from the dynamic range to avoid colliding with
/// anything well known; the actual port is published in `daemon.json`.
const DEFAULT_PORT: u16 = 47_113;

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

struct Options {
    port: u16,
    ui_dir: Option<PathBuf>,
    data_dir: PathBuf,
    download_dir: PathBuf,
    foreground: bool,
    print_token: bool,
}

fn main() {
    let options = match parse_args() {
        Ok(Some(options)) => options,
        Ok(None) => return,
        Err(message) => {
            eprintln!("hdmd: {message}");
            eprintln!("Try `hdmd --help`.");
            std::process::exit(2);
        }
    };

    if let Err(e) = run(options) {
        eprintln!("hdmd: {e}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), String> {
    std::fs::create_dir_all(&options.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", options.data_dir.display()))?;

    // One daemon per user. Two would fight over the same state file and
    // double-start every queued download.
    let _lock = InstanceLock::acquire(&options.data_dir.join("hydra.lock"))
        .map_err(|e| format!("{e}. Is another Hydra already running?"))?;

    let token = load_or_create_token(&options.data_dir)?;
    let manager = Manager::load(&options.data_dir, options.download_dir.clone());

    let server = ApiServer {
        manager: manager.clone(),
        token: token.clone(),
        ui_dir: options.ui_dir.clone(),
        extra_origins: Vec::new(),
        version: VERSION.to_string(),
    };
    let (bound, _accept_thread) = server
        .start(options.port)
        .map_err(|e| format!("cannot listen on port {}: {e}", options.port))?;

    // Publish where we are and how to authenticate, so the CLI, the browser
    // extension's native host and the desktop shell can all find us without
    // being told.
    write_daemon_file(&options.data_dir, bound.addr.port(), &token)?;

    let scheduler = manager.spawn_scheduler();
    install_signal_handlers();

    let url = format!("http://127.0.0.1:{}/", bound.addr.port());
    println!("Hydra Download Manager {VERSION}");
    println!("  interface  {url}");
    println!("  state      {}", manager.state_path().display());
    println!("  downloads  {}", options.download_dir.display());
    if options.print_token {
        println!("  token      {token}");
    } else {
        println!(
            "  token      {}",
            options.data_dir.join("daemon.json").display()
        );
    }
    if options.ui_dir.is_none() {
        println!("  note       no web UI directory found; the API is still available");
    }
    println!("Press Ctrl+C to stop.");

    let _ = options.foreground;
    let clipboard = manager.spawn_clipboard_monitor();

    // Stop on a signal, or when a queue's completion action asked the daemon
    // to exit once its downloads finished.
    while !SHUTDOWN.load(Ordering::Acquire) && manager.is_running() {
        std::thread::sleep(Duration::from_millis(200));
    }

    println!("\nStopping; pausing transfers and saving state...");
    bound.stop();
    manager.shutdown();
    let _ = scheduler.join();
    let _ = clipboard.join();
    let _ = std::fs::remove_file(options.data_dir.join("daemon.json"));
    println!("Stopped.");
    Ok(())
}

/// Loads the API token, creating one on first run.
fn load_or_create_token(data_dir: &Path) -> Result<String, String> {
    let path = data_dir.join("token");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if existing.len() >= 32 {
            return Ok(existing);
        }
    }
    let token =
        hdm_crypto::random_token(32).map_err(|e| format!("cannot generate an API token: {e}"))?;
    std::fs::write(&path, &token).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    restrict(&path);
    Ok(token)
}

/// Publishes the port and token for local clients.
fn write_daemon_file(data_dir: &Path, port: u16, token: &str) -> Result<(), String> {
    let path = data_dir.join("daemon.json");
    let document = json!({
        "port": port,
        "token": token,
        "pid": (std::process::id()),
        "version": VERSION,
        "url": (format!("http://127.0.0.1:{port}"))
    });
    std::fs::write(&path, document.to_string_pretty())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    // It contains the API token, which is the key to the whole daemon.
    restrict(&path);
    Ok(())
}

fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Asks for a clean shutdown on Ctrl+C or SIGTERM.
///
/// Progress is checkpointed every couple of seconds anyway, so an abrupt kill
/// costs little — but stopping cleanly means paused transfers, a saved list and
/// no stale `daemon.json` pointing at a dead process.
fn install_signal_handlers() {
    #[cfg(unix)]
    {
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        extern "C" fn handler(_signum: i32) {
            // Only an atomic store: nothing else here is async-signal-safe.
            SHUTDOWN.store(true, Ordering::Release);
        }
        unsafe {
            signal(SIGINT, handler as *const () as usize);
            signal(SIGTERM, handler as *const () as usize);
        }
    }
    #[cfg(windows)]
    {
        const CTRL_C_EVENT: u32 = 0;
        const CTRL_BREAK_EVENT: u32 = 1;
        const CTRL_CLOSE_EVENT: u32 = 2;
        const CTRL_SHUTDOWN_EVENT: u32 = 6;

        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(
                handler: Option<extern "system" fn(u32) -> i32>,
                add: i32,
            ) -> i32;
        }
        extern "system" fn handler(event: u32) -> i32 {
            match event {
                CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT => {
                    SHUTDOWN.store(true, Ordering::Release);
                    1
                }
                _ => 0,
            }
        }
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut options = Options {
        port: DEFAULT_PORT,
        ui_dir: find_ui_dir(),
        data_dir: platform::data_dir(),
        download_dir: platform::default_download_dir(),
        foreground: true,
        print_token: false,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("`{arg}` needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("hdmd {VERSION}");
                return Ok(None);
            }
            "-p" | "--port" => {
                options.port = value()?
                    .parse()
                    .map_err(|_| "--port needs a number".to_string())?;
            }
            "--ui" => options.ui_dir = Some(PathBuf::from(value()?)),
            "--no-ui" => options.ui_dir = None,
            "--data-dir" => options.data_dir = PathBuf::from(value()?),
            "--download-dir" => options.download_dir = PathBuf::from(value()?),
            "--print-token" => options.print_token = true,
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(Some(options))
}

/// Looks for the web UI next to the binary, then in the source tree.
fn find_ui_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("ui"));
            // An installed layout: bin/hdmd next to share/hydra/ui.
            if let Some(prefix) = dir.parent() {
                candidates.push(prefix.join("share/hydra/ui"));
            }
            // A cargo target directory: target/debug/hdmd -> repo/ui.
            candidates.push(dir.join("../../ui"));
        }
    }
    candidates.push(PathBuf::from("ui"));
    candidates
        .into_iter()
        .find(|c| c.join("index.html").is_file())
}

fn print_help() {
    println!(
        "hdmd {VERSION} — Hydra Download Manager daemon

USAGE:
    hdmd [OPTIONS]

OPTIONS:
    -p, --port <PORT>          Port to listen on (default {DEFAULT_PORT}; 0 picks a free one)
        --ui <DIR>             Directory holding the built web UI
        --no-ui                Serve the API only
        --data-dir <DIR>       Where to keep state (default: the platform data directory)
        --download-dir <DIR>   Default save location
        --print-token          Print the API token on startup
    -h, --help                 Show this help
    -V, --version              Show the version

The daemon listens on 127.0.0.1 only and requires a bearer token, which is
written to `daemon.json` in the data directory for local clients to read."
    );
}

/// Reads a running daemon's connection details, for clients.
pub fn read_daemon_file(data_dir: &Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(data_dir.join("daemon.json")).ok()?;
    let value: Json = parse(&text).ok()?;
    Some((
        value.get("port")?.as_u64()? as u16,
        value.get("token")?.as_str()?.to_string(),
    ))
}
