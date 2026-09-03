//! Everything that differs between operating systems, in one place.
//!
//! Keeping this module small and self-contained is deliberate: Windows is
//! Hydra's priority target but is not buildable in the environment the engine
//! was developed in, so the amount of unverifiable code is held to a minimum
//! and confined behind `#[cfg]` where it cannot affect the tested paths.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;

/// Where Hydra keeps its state: the download list, settings, and logs.
pub fn data_dir() -> PathBuf {
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
        home()
            .map(|h| h.join("Library/Application Support/Hydra"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".local/share")))
            .map(|base| base.join("hydra"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// The user's Downloads folder, where files land unless a category says otherwise.
pub fn default_download_dir() -> PathBuf {
    if let Some(explicit) = std::env::var_os("HYDRA_DOWNLOAD_DIR") {
        return PathBuf::from(explicit);
    }
    #[cfg(windows)]
    {
        // USERPROFILE\Downloads is right on every supported Windows version.
        // Reading the Known Folder ID would be more correct for a relocated
        // folder, but needs COM; the setting can be overridden in the UI.
        std::env::var_os("USERPROFILE")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(unix)]
    {
        // Respect a localized XDG Downloads folder when one is configured.
        if let Some(dir) = xdg_download_dir() {
            return dir;
        }
        home()
            .map(|h| h.join("Downloads"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

#[cfg(unix)]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Reads `XDG_DOWNLOAD_DIR` out of `user-dirs.dirs`, which is how desktop Linux
/// records a renamed or relocated Downloads folder.
#[cfg(unix)]
fn xdg_download_dir() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))?;
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some(value) = line.strip_prefix("XDG_DOWNLOAD_DIR=") else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        let expanded = match value.strip_prefix("$HOME/") {
            Some(rest) => home()?.join(rest),
            None => PathBuf::from(value),
        };
        return Some(expanded);
    }
    None
}

/// Reserves `size` bytes for `file`.
///
/// Segments write all over the file at once, so growing it lazily would leave
/// the result badly fragmented and can fail with ENOSPC halfway through a large
/// download. Reserving up front turns "disk full" into an error before any time
/// is spent transferring.
pub fn preallocate(file: &File, size: u64) -> io::Result<()> {
    if size == 0 {
        return Ok(());
    }
    file.set_len(size)
}

/// Writes `buf` at an absolute offset without moving the file cursor.
///
/// This is what lets every segment thread share one `File` handle with no lock
/// and no seeking: `pwrite` on Unix, `seek_write` on Windows.
pub fn write_all_at(file: &File, mut offset: u64, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let written = {
            #[cfg(unix)]
            {
                file.write_at(buf, offset)?
            }
            #[cfg(windows)]
            {
                file.seek_write(buf, offset)?
            }
        };
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the filesystem accepted no bytes",
            ));
        }
        buf = &buf[written..];
        offset += written as u64;
    }
    Ok(())
}

/// Reads at an absolute offset. Used to re-hash a partially downloaded file.
pub fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    {
        file.read_at(buf, offset)
    }
    #[cfg(windows)]
    {
        file.seek_read(buf, offset)
    }
}

/// What to do once a queue drains. IDM offers the same set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Sleep,
    Hibernate,
}

/// Requests a power state change.
///
/// Spawned rather than waited on, so a confirmation dialog or a slow shutdown
/// cannot wedge the daemon.
pub fn power_action(action: &PowerAction) -> io::Result<()> {
    use std::process::Command;
    #[cfg(windows)]
    {
        let mut command = match action {
            // A minute's grace so the user can cancel from the toast.
            PowerAction::Shutdown => {
                let mut c = Command::new("shutdown");
                c.args([
                    "/s",
                    "/t",
                    "60",
                    "/c",
                    "Hydra Download Manager: downloads finished",
                ]);
                c
            }
            PowerAction::Sleep => {
                let mut c = Command::new("rundll32.exe");
                c.args(["powrprof.dll,SetSuspendState", "0,1,0"]);
                c
            }
            PowerAction::Hibernate => {
                let mut c = Command::new("shutdown");
                c.args(["/h"]);
                c
            }
        };
        command.spawn().map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        let script = match action {
            PowerAction::Shutdown => "tell application \"System Events\" to shut down",
            PowerAction::Sleep | PowerAction::Hibernate => {
                "tell application \"System Events\" to sleep"
            }
        };
        Command::new("osascript")
            .args(["-e", script])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // systemctl works without root for these on any logind system, which
        // is every mainstream desktop distribution.
        let verb = match action {
            PowerAction::Shutdown => "poweroff",
            PowerAction::Sleep => "suspend",
            PowerAction::Hibernate => "hibernate",
        };
        Command::new("systemctl").arg(verb).spawn().map(|_| ())
    }
}

/// Opens a folder in the system file manager, selecting `path` where possible.
pub fn reveal_in_file_manager(path: &Path) -> io::Result<()> {
    use std::process::Command;
    #[cfg(windows)]
    {
        Command::new("explorer")
            .arg(format!("/select,{}", path.display()))
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .args(["-R".as_ref(), path.as_os_str()])
            .spawn()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let folder = path.parent().unwrap_or(path);
        Command::new("xdg-open").arg(folder).spawn().map(|_| ())
    }
}

/// Holds an exclusive lock for as long as it lives, so a second daemon refuses
/// to start rather than two of them fighting over the same state file.
pub struct InstanceLock {
    #[allow(dead_code)]
    file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Acquires the lock, or reports that another instance holds it.
    pub fn acquire(path: &Path) -> io::Result<InstanceLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            // An advisory lock is released automatically if the process dies,
            // so a crash never leaves a stale lock behind.
            if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "another Hydra daemon is already running",
                ));
            }
        }
        #[cfg(windows)]
        {
            // On Windows the open handle itself is the lock: the file is opened
            // without FILE_SHARE_WRITE, so a second instance cannot open it.
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_SHARE_READ: u32 = 0x0000_0001;
            drop(file);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .share_mode(FILE_SHARE_READ)
                .open(path)
                .map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another Hydra daemon is already running",
                    )
                })?;
            return Ok(InstanceLock {
                file,
                path: path.to_path_buf(),
            });
        }
        #[cfg(unix)]
        Ok(InstanceLock {
            file,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
