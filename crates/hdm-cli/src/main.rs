//! `hdm` — the Hydra command-line client.
//!
//! Talks to a running daemon over the local API, and can also download a file
//! directly with `hdm get`, which needs no daemon at all — useful in scripts
//! and on servers.

use hdm_core::engine::{DownloadSpec, Outcome, Shared, Status};
use hdm_core::platform;
use hdm_core::throttle::Throttle;
use hdm_json::{json, parse, Json};
use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::Request;
use hdm_net::url::Url;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            print_help();
            Ok(())
        }
        Some("-V") | Some("--version") => {
            println!("hdm {VERSION}");
            Ok(())
        }
        Some("get") => command_get(&args[1..]),
        Some("add") => command_add(&args[1..]),
        Some("list") | Some("ls") => command_list(&args[1..]),
        Some("pause") => command_action(&args[1..], "pause"),
        Some("resume") | Some("start") => command_action(&args[1..], "resume"),
        Some("cancel") => command_action(&args[1..], "cancel"),
        Some("restart") => command_action(&args[1..], "restart"),
        Some("remove") | Some("rm") => command_remove(&args[1..]),
        Some("settings") => command_settings(&args[1..]),
        Some("status") => command_status(),
        Some(other) => Err(format!("unknown command `{other}`. Try `hdm --help`.")),
    };

    if let Err(message) = result {
        eprintln!("hdm: {message}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------- daemon client

/// A thin client for the local daemon.
struct Daemon {
    base: String,
    token: String,
    client: Client,
}

impl Daemon {
    /// Finds the running daemon via the file it publishes on startup.
    fn connect() -> Result<Daemon, String> {
        let path = platform::data_dir().join("daemon.json");
        let text = std::fs::read_to_string(&path).map_err(|_| {
            format!(
                "no running daemon found (looked for {}).\nStart one with `hdmd`.",
                path.display()
            )
        })?;
        let value = parse(&text).map_err(|e| format!("{} is unreadable: {e}", path.display()))?;
        let port = value
            .get("port")
            .and_then(Json::as_u64)
            .ok_or_else(|| format!("{} has no port", path.display()))?;
        let token = value
            .get("token")
            .and_then(Json::as_str)
            .ok_or_else(|| format!("{} has no token", path.display()))?;

        Ok(Daemon {
            base: format!("http://127.0.0.1:{port}"),
            token: token.to_string(),
            client: Client::new(ClientConfig::new()).map_err(|e| e.to_string())?,
        })
    }

    fn request(&self, method: &str, path: &str, body: Option<Json>) -> Result<Json, String> {
        let url =
            Url::parse(&format!("{}{path}", self.base)).map_err(|e| format!("bad URL: {e}"))?;
        let mut request = Request::get(url);
        request.method = method.to_string();
        request
            .headers
            .set("Authorization", format!("Bearer {}", self.token));
        if let Some(body) = body {
            request.headers.set("Content-Type", "application/json");
            request.body = Some(body.to_string_compact().into_bytes());
        }

        let mut fetch = self
            .client
            .send(request)
            .map_err(|e| format!("cannot reach the daemon at {}: {e}", self.base))?;
        let text = fetch
            .response
            .read_to_string(8 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let value = parse(&text).map_err(|e| format!("the daemon sent invalid JSON: {e}"))?;

        if fetch.response.status >= 400 {
            let message = value.str_or("error", "the daemon rejected the request");
            return Err(message.to_string());
        }
        Ok(value)
    }
}

// ------------------------------------------------------------- commands

/// Downloads a file directly, with no daemon involved.
fn command_get(args: &[String]) -> Result<(), String> {
    let mut options = AddOptions::parse(args)?;
    let url = options.url.take().ok_or("`hdm get` needs a URL")?;

    let directory = options
        .directory
        .clone()
        .unwrap_or_else(platform::default_download_dir);
    let mut spec = DownloadSpec::new(&url, directory);
    options.apply(&mut spec)?;

    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::new(spec.speed_limit));

    // A reporter thread draws the progress bar while the transfer runs.
    let reporter = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut drawn = false;
            while !shared.status().is_terminal() {
                if shared.status() == Status::Downloading {
                    draw_progress(&shared);
                    drawn = true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if drawn {
                eprintln!();
            }
        })
    };

    let outcome = hdm_core::engine::run(&spec, &shared, &throttle);
    let _ = reporter.join();

    match outcome {
        Ok(Outcome::Completed { path, bytes }) => {
            println!("{} ({})", path.display(), human_bytes(bytes));
            Ok(())
        }
        Ok(Outcome::Paused) => Err("the download was paused".into()),
        Ok(Outcome::Cancelled) => Err("the download was cancelled".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn command_add(args: &[String]) -> Result<(), String> {
    let mut options = AddOptions::parse(args)?;
    let url = options.url.take().ok_or("`hdm add` needs a URL")?;
    let daemon = Daemon::connect()?;

    let mut body = json!({ "url": (url.as_str()) });
    if let Some(directory) = &options.directory {
        body.insert(
            "directory",
            Json::Str(directory.to_string_lossy().into_owned()),
        );
    }
    if let Some(filename) = &options.filename {
        body.insert("filename", Json::Str(filename.clone()));
    }
    if let Some(connections) = options.connections {
        body.insert("connections", Json::from(connections));
    }
    if let Some(limit) = options.speed_limit {
        body.insert("speedLimit", Json::from(limit));
    }
    if let Some(checksum) = &options.checksum {
        body.insert("checksum", Json::Str(checksum.clone()));
    }
    body.insert("autostart", Json::Bool(!options.paused));

    let created = daemon.request("POST", "/api/v1/downloads", Some(body))?;
    println!(
        "{}  {}",
        created.str_or("id", "?"),
        created.str_or("filename", url.as_str())
    );
    Ok(())
}

fn command_list(args: &[String]) -> Result<(), String> {
    let as_json = args.iter().any(|a| a == "--json");
    let daemon = Daemon::connect()?;
    let response = daemon.request("GET", "/api/v1/downloads", None)?;

    if as_json {
        println!("{}", response.to_string_pretty());
        return Ok(());
    }

    let downloads = response
        .get("downloads")
        .and_then(Json::as_arr)
        .unwrap_or(&[]);
    if downloads.is_empty() {
        println!("No downloads.");
        return Ok(());
    }

    println!(
        "{:<14} {:<11} {:>9} {:>10} NAME",
        "ID", "STATUS", "PROGRESS", "SPEED"
    );
    for download in downloads {
        let id = download.str_or("id", "?");
        let status = download.str_or("status", "?");
        let name = download.str_or("filename", "");
        let downloaded = download.u64_or("downloaded", 0);
        let total = download.get("total").and_then(Json::as_u64);
        let speed = download.u64_or("speed", 0);

        let progress = match total {
            Some(total) if total > 0 => {
                format!("{:.1}%", downloaded as f64 / total as f64 * 100.0)
            }
            _ => human_bytes(downloaded),
        };
        let speed_text = if speed > 0 {
            format!("{}/s", human_bytes(speed))
        } else {
            "-".into()
        };
        println!("{id:<14} {status:<11} {progress:>9} {speed_text:>10} {name}");
    }

    let totals = response.get("totals").cloned().unwrap_or(Json::Null);
    let speed = totals.u64_or("speed", 0);
    if speed > 0 {
        println!(
            "\n{} active at {}/s",
            totals.u64_or("active", 0),
            human_bytes(speed)
        );
    }
    Ok(())
}

fn command_action(args: &[String], action: &str) -> Result<(), String> {
    let daemon = Daemon::connect()?;
    if args.iter().any(|a| a == "--all") {
        let path = match action {
            "pause" => "/api/v1/downloads-pause-all",
            "resume" => "/api/v1/downloads-resume-all",
            _ => return Err(format!("`--all` is not supported for `{action}`")),
        };
        daemon.request("POST", path, None)?;
        println!("Done.");
        return Ok(());
    }

    let id = args
        .first()
        .ok_or_else(|| format!("`hdm {action}` needs a download id"))?;
    daemon.request("POST", &format!("/api/v1/downloads/{id}/{action}"), None)?;
    println!("{id}: {action}");
    Ok(())
}

fn command_remove(args: &[String]) -> Result<(), String> {
    let delete_files = args.iter().any(|a| a == "--delete-files");
    let id = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .ok_or("`hdm remove` needs a download id")?;
    let daemon = Daemon::connect()?;
    let query = if delete_files {
        "?deleteFiles=true"
    } else {
        ""
    };
    daemon.request("DELETE", &format!("/api/v1/downloads/{id}{query}"), None)?;
    println!("{id}: removed");
    Ok(())
}

fn command_settings(args: &[String]) -> Result<(), String> {
    let daemon = Daemon::connect()?;
    if args.is_empty() {
        println!(
            "{}",
            daemon
                .request("GET", "/api/v1/settings", None)?
                .to_string_pretty()
        );
        return Ok(());
    }

    let mut settings = daemon.request("GET", "/api/v1/settings", None)?;
    let mut index = 0;
    while index < args.len() {
        let key = args[index].trim_start_matches("--");
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("`--{key}` needs a value"))?;
        match key {
            "speed-limit" => settings.insert("speedLimit", Json::from(parse_rate(value)?)),
            "connections" => settings.insert(
                "connections",
                Json::from(
                    value
                        .parse::<u64>()
                        .map_err(|_| "connections must be a number")?,
                ),
            ),
            "max-concurrent" => settings.insert(
                "maxConcurrent",
                Json::from(
                    value
                        .parse::<u64>()
                        .map_err(|_| "max-concurrent must be a number")?,
                ),
            ),
            "language" => settings.insert("language", Json::Str(value.clone())),
            "theme" => settings.insert("theme", Json::Str(value.clone())),
            other => return Err(format!("unknown setting `--{other}`")),
        }
        index += 2;
    }
    println!(
        "{}",
        daemon
            .request("PUT", "/api/v1/settings", Some(settings))?
            .to_string_pretty()
    );
    Ok(())
}

fn command_status() -> Result<(), String> {
    let daemon = Daemon::connect()?;
    let health = daemon.request("GET", "/api/v1/health", None)?;
    let totals = daemon.request("GET", "/api/v1/totals", None)?;
    println!(
        "Daemon   {} ({})",
        health.str_or("status", "?"),
        daemon.base
    );
    println!("Version  {}", health.str_or("version", "?"));
    println!(
        "Downloads {} total, {} active, {} queued, {} completed, {} failed",
        totals.u64_or("total", 0),
        totals.u64_or("active", 0),
        totals.u64_or("queued", 0),
        totals.u64_or("completed", 0),
        totals.u64_or("failed", 0)
    );
    let speed = totals.u64_or("speed", 0);
    println!("Speed    {}/s", human_bytes(speed));
    let limit = totals.u64_or("speedLimit", 0);
    println!(
        "Limit    {}",
        if limit == 0 {
            "unlimited".to_string()
        } else {
            format!("{}/s", human_bytes(limit))
        }
    );
    Ok(())
}

// -------------------------------------------------------------- arguments

#[derive(Default)]
struct AddOptions {
    url: Option<String>,
    directory: Option<PathBuf>,
    filename: Option<String>,
    connections: Option<u64>,
    speed_limit: Option<u64>,
    checksum: Option<String>,
    referer: Option<String>,
    user_agent: Option<String>,
    headers: Vec<(String, String)>,
    username: Option<String>,
    password: Option<String>,
    insecure: bool,
    paused: bool,
    overwrite: bool,
    proxy: Option<String>,
}

impl AddOptions {
    fn parse(args: &[String]) -> Result<AddOptions, String> {
        let mut options = AddOptions::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            let take_value = |index: &mut usize| -> Result<String, String> {
                *index += 1;
                args.get(*index)
                    .cloned()
                    .ok_or_else(|| format!("`{arg}` needs a value"))
            };
            match arg.as_str() {
                "-o" | "--output" => options.filename = Some(take_value(&mut index)?),
                "-d" | "--dir" => options.directory = Some(PathBuf::from(take_value(&mut index)?)),
                "-n" | "--connections" => {
                    options.connections = Some(
                        take_value(&mut index)?
                            .parse()
                            .map_err(|_| "--connections needs a number".to_string())?,
                    )
                }
                "-l" | "--limit" => {
                    options.speed_limit = Some(parse_rate(&take_value(&mut index)?)?)
                }
                "--checksum" => options.checksum = Some(take_value(&mut index)?),
                "--referer" => options.referer = Some(take_value(&mut index)?),
                "--user-agent" => options.user_agent = Some(take_value(&mut index)?),
                "-H" | "--header" => {
                    let raw = take_value(&mut index)?;
                    let (name, value) = raw
                        .split_once(':')
                        .ok_or_else(|| format!("`{raw}` is not `Name: value`"))?;
                    options
                        .headers
                        .push((name.trim().to_string(), value.trim().to_string()));
                }
                "-u" | "--user" => {
                    let raw = take_value(&mut index)?;
                    match raw.split_once(':') {
                        Some((user, password)) => {
                            options.username = Some(user.to_string());
                            options.password = Some(password.to_string());
                        }
                        None => options.username = Some(raw),
                    }
                }
                "--proxy" => options.proxy = Some(take_value(&mut index)?),
                "-k" | "--insecure" => options.insecure = true,
                "--paused" => options.paused = true,
                "--overwrite" => options.overwrite = true,
                "--json" => {}
                other if other.starts_with('-') => return Err(format!("unknown option `{other}`")),
                other => options.url = Some(other.to_string()),
            }
            index += 1;
        }
        Ok(options)
    }

    fn apply(&self, spec: &mut DownloadSpec) -> Result<(), String> {
        if let Some(filename) = &self.filename {
            spec.filename = Some(filename.clone());
        }
        if let Some(connections) = self.connections {
            spec.connections = connections.clamp(1, 32) as u8;
        }
        if let Some(limit) = self.speed_limit {
            spec.speed_limit = limit;
        }
        spec.tls_insecure = self.insecure;
        spec.overwrite = self.overwrite;
        spec.proxy = self.proxy.clone();
        spec.username = self.username.clone();
        spec.password = self.password.clone();
        spec.headers = self.headers.clone();
        if let Some(referer) = &self.referer {
            spec.headers.push(("Referer".into(), referer.clone()));
        }
        if let Some(agent) = &self.user_agent {
            spec.headers.push(("User-Agent".into(), agent.clone()));
        }
        if let Some(digest) = &self.checksum {
            let (algo, value) = match digest.split_once(':') {
                Some((name, value)) => (
                    hdm_crypto::HashAlgo::parse(name)
                        .ok_or_else(|| format!("unknown checksum algorithm `{name}`"))?,
                    value.to_string(),
                ),
                None => (
                    hdm_crypto::HashAlgo::from_hex_len(digest)
                        .ok_or("cannot tell which algorithm that digest is; use `sha256:...`")?,
                    digest.clone(),
                ),
            };
            spec.checksum = Some((algo, value.to_ascii_lowercase()));
        }
        Ok(())
    }
}

/// Parses a rate such as `500k`, `2M`, or a plain byte count.
fn parse_rate(value: &str) -> Result<u64, String> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("none") || trimmed == "0" {
        return Ok(0);
    }
    let (digits, multiplier) = match trimmed.chars().last() {
        Some('k') | Some('K') => (&trimmed[..trimmed.len() - 1], 1024),
        Some('m') | Some('M') => (&trimmed[..trimmed.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&trimmed[..trimmed.len() - 1], 1024 * 1024 * 1024),
        _ => (trimmed, 1),
    };
    let number: f64 = digits
        .trim_end_matches(['b', 'B'])
        .trim()
        .parse()
        .map_err(|_| format!("`{value}` is not a rate; try 500k or 2M"))?;
    Ok((number * multiplier as f64) as u64)
}

// -------------------------------------------------------------- rendering

fn draw_progress(shared: &Shared) {
    let downloaded = shared.downloaded();
    let speed = shared.speed();
    let line = match (shared.total(), shared.fraction()) {
        (Some(total), Some(fraction)) => {
            let width = 30usize;
            let filled = (fraction * width as f64).round() as usize;
            let bar: String = "█".repeat(filled) + &"░".repeat(width - filled);
            let eta = shared
                .eta_seconds()
                .map(format_duration)
                .unwrap_or_else(|| "--".into());
            format!(
                "\r{bar} {:>5.1}%  {} / {}  {}/s  ETA {eta}   ",
                fraction * 100.0,
                human_bytes(downloaded),
                human_bytes(total),
                human_bytes(speed)
            )
        }
        _ => format!("\r{}  {}/s   ", human_bytes(downloaded), human_bytes(speed)),
    };
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.flush();
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_duration(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn print_help() {
    println!(
        "hdm {VERSION} — Hydra Download Manager

USAGE:
    hdm <COMMAND> [OPTIONS]

COMMANDS:
    get <URL>          Download a file now, without a daemon
    add <URL>          Queue a download in the running daemon
    list               Show the download list
    status             Show daemon and transfer totals
    pause <ID>         Pause a download (or --all)
    resume <ID>        Resume a download (or --all)
    cancel <ID>        Cancel a download
    restart <ID>       Start a download again from the beginning
    remove <ID>        Remove from the list (--delete-files to erase it too)
    settings           Show settings, or change them with --key value

DOWNLOAD OPTIONS:
    -o, --output <NAME>       Save under this filename
    -d, --dir <DIR>           Save into this directory
    -n, --connections <N>     Parallel connections (1-32)
    -l, --limit <RATE>        Speed limit, e.g. 500k or 2M
        --checksum <DIGEST>   Verify against sha256:..., or a bare digest
        --referer <URL>       Send this Referer
        --user-agent <UA>     Send this User-Agent
    -H, --header 'N: V'       Add a request header (repeatable)
    -u, --user <USER[:PASS]>  HTTP or FTP credentials
        --proxy <URL>         http://host:port or socks5://host:port
    -k, --insecure            Do not verify TLS certificates
        --paused              Add without starting (add only)
        --overwrite           Replace an existing file

EXAMPLES:
    hdm get https://example.com/ubuntu.iso -n 16
    hdm add https://example.com/f.zip --limit 2M
    hdm settings --speed-limit 500k
    hdm pause --all"
    );
}
