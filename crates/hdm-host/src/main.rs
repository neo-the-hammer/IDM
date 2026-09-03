//! `hdm-host` — the native-messaging bridge between a browser and the daemon.
//!
//! Its whole job is pairing. The extension needs the daemon's port and API
//! token, and asking a user to find `daemon.json` and paste a token into an
//! options page is a miserable first run. A native-messaging host can read
//! that file directly, and the browser only launches it for the extension IDs
//! named in its manifest, so the token never has to be handled by a person.
//!
//! The protocol is Chrome's: a little-endian `u32` length followed by that many
//! bytes of JSON, in both directions, over stdin and stdout.

use hdm_json::{json, parse, Json};
use std::io::{self, Read, Write};
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Chrome refuses messages above 1 MB; nothing here approaches that, and a
/// bound stops a confused caller making us allocate wildly.
const MAX_MESSAGE: u32 = 1024 * 1024;

fn main() {
    // The browser passes the calling extension's origin as the first argument.
    // It has already checked that origin against this host's manifest, so this
    // is only useful for diagnostics.
    let caller = std::env::args().nth(1).unwrap_or_default();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();

    loop {
        let message = match read_message(&mut input) {
            Ok(Some(message)) => message,
            // The browser closed the pipe: a normal shutdown.
            Ok(None) => return,
            Err(e) => {
                let _ = write_message(&mut output, &json!({"ok": false, "error": (e.to_string())}));
                return;
            }
        };

        let reply = handle(&message, &caller);
        if write_message(&mut output, &reply).is_err() {
            return;
        }
    }
}

fn handle(message: &Json, caller: &str) -> Json {
    match message.str_or("type", "") {
        "ping" => json!({"ok": true, "version": VERSION, "caller": caller}),
        "getConnection" => match read_daemon_file() {
            Some((port, token)) => json!({
                "ok": true,
                "port": port,
                "token": (token.as_str()),
                "url": (format!("http://127.0.0.1:{port}")),
                "version": VERSION
            }),
            None => json!({
                "ok": false,
                "error": "Hydra is not running. Start hdmd and try again.",
                "dataDir": (data_dir().to_string_lossy().into_owned())
            }),
        },
        other => json!({"ok": false, "error": (format!("unknown request `{other}`"))}),
    }
}

/// Reads the port and token the daemon publishes on startup.
fn read_daemon_file() -> Option<(u64, String)> {
    let text = std::fs::read_to_string(data_dir().join("daemon.json")).ok()?;
    let value = parse(&text).ok()?;
    Some((
        value.get("port")?.as_u64()?,
        value.get("token")?.as_str()?.to_string(),
    ))
}

/// Mirrors `hdm_core::platform::data_dir` without taking the dependency, so
/// this stays a small standalone binary.
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

/// Reads one length-prefixed message. `Ok(None)` means the stream ended.
fn read_message(input: &mut impl Read) -> io::Result<Option<Json>> {
    let mut length = [0u8; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let length = u32::from_le_bytes(length);
    if length == 0 {
        return Ok(Some(Json::Null));
    }
    if length > MAX_MESSAGE {
        return Err(io::Error::other(format!(
            "message of {length} bytes is too large"
        )));
    }

    let mut body = vec![0u8; length as usize];
    input.read_exact(&mut body)?;
    let text =
        String::from_utf8(body).map_err(|_| io::Error::other("message is not valid UTF-8"))?;
    parse(&text)
        .map(Some)
        .map_err(|e| io::Error::other(format!("malformed message: {e}")))
}

fn write_message(output: &mut impl Write, value: &Json) -> io::Result<()> {
    let body = value.to_string_compact();
    let length = u32::try_from(body.len()).map_err(|_| io::Error::other("reply is too large"))?;
    output.write_all(&length.to_le_bytes())?;
    output.write_all(body.as_bytes())?;
    output.flush()
}
