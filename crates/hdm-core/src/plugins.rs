//! The bridge to the Python extraction plugins.
//!
//! Hydra's engine is Rust; parsing real-world HTML and delegating to yt-dlp are
//! jobs Python is better at. Running them in a separate process means a
//! malformed page or a crashing extractor cannot take the daemon down.
//!
//! Python is genuinely optional. Every core feature — downloading, resuming,
//! queues, batch patterns — works without it. The site grabber and media
//! extraction are what need it, and they say so plainly when it is missing
//! rather than failing in some obscure way.

use hdm_json::{json, parse, Json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// How long a single plugin call may take before it is killed.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Cap on a reply, so a runaway plugin cannot exhaust memory.
const MAX_REPLY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PluginHost {
    python: PathBuf,
    /// The directory that *contains* the `hdm_plugins` package.
    package_root: PathBuf,
}

/// Cached discovery, so every call does not re-probe the filesystem.
static DISCOVERED: OnceLock<Result<PluginHost, String>> = OnceLock::new();

impl PluginHost {
    /// Finds a usable Python and the plugin package.
    pub fn discover() -> Result<PluginHost, String> {
        DISCOVERED.get_or_init(discover_uncached).clone()
    }

    /// Builds a host with a chosen interpreter, bypassing discovery.
    ///
    /// Exists so the tests can point at a stand-in that hangs or crashes on
    /// purpose; discovery caches its result, so it cannot be redirected per test.
    #[doc(hidden)]
    pub fn for_test(python: PathBuf, package_root: PathBuf) -> PluginHost {
        PluginHost {
            python,
            package_root,
        }
    }

    pub fn python(&self) -> &std::path::Path {
        &self.python
    }

    pub fn package_root(&self) -> &std::path::Path {
        &self.package_root
    }

    /// Sends one request and returns the reply.
    ///
    /// A fresh process per call, rather than one kept alive. A crawl is
    /// network-bound, so Python's ~25ms startup disappears next to fetching the
    /// page — and in exchange a plugin that hangs, crashes or corrupts its own
    /// state cannot affect the next call. Robustness is worth more than the
    /// milliseconds here.
    pub fn request(&self, request: Json, timeout: Duration) -> Result<Json, String> {
        let mut child = Command::new(&self.python)
            .arg("-m")
            .arg("hdm_plugins")
            .env("PYTHONPATH", &self.package_root)
            // Unbuffered, or a small reply can sit in Python's stdout buffer
            // until the process exits.
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the plugin host: {e}"))?;

        let mut stdin = child.stdin.take().ok_or("the plugin host has no stdin")?;
        let line = format!("{}\n", request.to_string_compact());
        // A write failure here usually means the interpreter died on startup;
        // the exit status collected below explains why.
        let write_failed = stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .is_err();
        drop(stdin);

        let reply = read_reply_with_timeout(&mut child, timeout);
        match reply {
            Ok(Some(value)) => Ok(value),
            Ok(None) | Err(_) if write_failed => Err(self.explain_failure(child)),
            Ok(None) => Err(self.explain_failure(child)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(e)
            }
        }
    }

    /// Turns a dead plugin process into a message worth reading.
    fn explain_failure(&self, mut child: Child) -> String {
        let stderr = child
            .stderr
            .take()
            .map(|s| {
                let mut buffer = String::new();
                use std::io::Read;
                let _ = s.take(8192).read_to_string(&mut buffer);
                buffer
            })
            .unwrap_or_default();
        let _ = child.wait();

        let detail = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        if detail.is_empty() {
            "the plugin host exited without replying".to_string()
        } else {
            format!("the plugin host failed: {detail}")
        }
    }

    /// Asks what the plugin host can do, including whether yt-dlp is present.
    pub fn capabilities(&self) -> Result<Json, String> {
        self.request(json!({"action": "capabilities"}), Duration::from_secs(20))
    }

    /// Extracts every link from a page the daemon has already fetched.
    pub fn links(&self, url: &str, html: &str) -> Result<Json, String> {
        self.expect_ok(self.request(
            json!({"action": "links", "url": url, "html": html}),
            DEFAULT_TIMEOUT,
        )?)
    }

    /// Finds playable media on a page.
    pub fn media(&self, url: &str, html: &str) -> Result<Json, String> {
        self.expect_ok(self.request(
            json!({"action": "media", "url": url, "html": html}),
            DEFAULT_TIMEOUT,
        )?)
    }

    /// Asks yt-dlp where a page's media actually lives.
    pub fn ytdlp(&self, url: &str) -> Result<Json, String> {
        self.expect_ok(self.request(
            json!({"action": "ytdlp", "url": url, "timeout": 90}),
            Duration::from_secs(120),
        )?)
    }

    fn expect_ok(&self, reply: Json) -> Result<Json, String> {
        if reply.bool_or("ok", false) {
            Ok(reply)
        } else {
            Err(reply
                .str_or("error", "the plugin reported a failure")
                .to_string())
        }
    }
}

/// Reads one line of reply, killing the child if it takes too long.
///
/// The read happens on its own thread because a pipe read cannot be given a
/// deadline; the caller waits on a channel instead, and a plugin caught in an
/// infinite loop is killed rather than blocking the daemon forever.
fn read_reply_with_timeout(child: &mut Child, timeout: Duration) -> Result<Option<Json>, String> {
    let Some(stdout) = child.stdout.take() else {
        return Ok(None);
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));

    {
        let finished = finished.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let result = match reader.read_line(&mut line) {
                Ok(0) => Ok(None),
                Ok(_) if line.len() > MAX_REPLY_BYTES => {
                    Err("the plugin replied with more data than expected".to_string())
                }
                Ok(_) => match parse(line.trim()) {
                    Ok(value) => Ok(Some(value)),
                    Err(e) => Err(format!("the plugin replied with malformed JSON: {e}")),
                },
                Err(e) => Err(format!("cannot read from the plugin host: {e}")),
            };
            finished.store(true, Ordering::Release);
            let _ = sender.send(result);
        });
    }

    match receiver.recv_timeout(timeout) {
        Ok(result) => {
            let _ = child.wait();
            result
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = finished;
            Err(format!("the plugin host did not reply within {timeout:?}"))
        }
    }
}

fn discover_uncached() -> Result<PluginHost, String> {
    let package_root = find_package_root().ok_or_else(|| {
        "could not find the hdm_plugins package. Set HYDRA_PLUGIN_DIR to the \
         directory containing it."
            .to_string()
    })?;

    let mut tried = Vec::new();
    for candidate in python_candidates() {
        // Prove the interpreter runs *and* can import the package, so a broken
        // setup is reported now rather than on the first crawl.
        let output = Command::new(&candidate)
            .arg("-c")
            .arg("import hdm_plugins, sys; print(hdm_plugins.__version__)")
            .env("PYTHONPATH", &package_root)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::null())
            .output();

        match output {
            Ok(output) if output.status.success() => {
                return Ok(PluginHost {
                    python: candidate,
                    package_root,
                });
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let last = stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("");
                tried.push(format!("{}: {last}", candidate.display()));
            }
            Err(e) => tried.push(format!("{}: {e}", candidate.display())),
        }
    }

    Err(format!(
        "no usable Python 3 was found for the extraction plugins. Tried: {}",
        if tried.is_empty() {
            "nothing".to_string()
        } else {
            tried.join("; ")
        }
    ))
}

fn python_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    // An explicit choice always wins, which is also how the tests simulate a
    // machine with no Python.
    if let Some(explicit) = std::env::var_os("HYDRA_PYTHON") {
        candidates.push(PathBuf::from(explicit));
        return candidates;
    }
    for name in ["python3", "python"] {
        candidates.push(PathBuf::from(name));
    }
    #[cfg(windows)]
    {
        // The Windows launcher is often the only thing on PATH.
        candidates.push(PathBuf::from("py"));
    }
    candidates
}

/// Finds the directory containing the `hdm_plugins` package.
///
/// Walks up from the executable and from the working directory rather than
/// checking a fixed list of relative paths. The same binary has to find the
/// package from an installed prefix, from `target/debug`, and from
/// `target/debug/deps` where test binaries live -- and hard-coding one depth
/// works for exactly one of those.
fn find_package_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("HYDRA_PLUGIN_DIR") {
        let root = PathBuf::from(explicit);
        if holds_package(&root) {
            return Some(root);
        }
    }

    let mut starting_points: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starting_points.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starting_points.push(cwd);
    }

    for start in starting_points {
        // Six levels covers target/debug/deps back to a repository root, and
        // any sane installation prefix.
        for ancestor in start.ancestors().take(7) {
            for relative in ["python", "share/hydra/python"] {
                let candidate = ancestor.join(relative);
                if holds_package(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn holds_package(root: &std::path::Path) -> bool {
    root.join("hdm_plugins").join("__init__.py").is_file()
}

/// A one-line description of the plugin layer's state, for diagnostics.
pub fn status() -> Json {
    match PluginHost::discover() {
        Ok(host) => match host.capabilities() {
            Ok(capabilities) => json!({
                "available": true,
                "python": (host.python().to_string_lossy().into_owned()),
                "packageRoot": (host.package_root().to_string_lossy().into_owned()),
                "capabilities": capabilities
            }),
            Err(e) => json!({"available": false, "error": (e)}),
        },
        Err(e) => json!({"available": false, "error": (e)}),
    }
}

/// A [`Mutex`]-free handle callers can share.
pub type SharedHost = Arc<Mutex<Option<PluginHost>>>;
