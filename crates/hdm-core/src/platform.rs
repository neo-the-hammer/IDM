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

/// The local wall-clock time, which is what a schedule is written in terms of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    /// 0 = Sunday, matching both `struct tm` and Windows' `SYSTEMTIME`.
    pub weekday: u8,
}

impl LocalTime {
    /// Minutes since local midnight, the unit schedules compare in.
    pub fn minutes(self) -> u16 {
        self.hour as u16 * 60 + self.minute as u16
    }
}

/// Reads the current local time.
///
/// Schedules are set in local time because that is how people think about
/// "start at 2am", and doing the conversion here rather than storing UTC means
/// a daylight-saving change moves the schedule with the clock, as expected.
pub fn local_time() -> LocalTime {
    imp_time::local_time()
}

#[cfg(unix)]
mod imp_time {
    use super::LocalTime;
    use std::ffi::{c_char, c_int, c_long};

    /// `struct tm`. Only the first nine fields are standard; the two glibc and
    /// BSD extensions are declared so the struct is large enough for
    /// `localtime_r` to fill safely on every platform we build for.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        gmtoff: c_long,
        zone: *const c_char,
    }

    extern "C" {
        fn localtime_r(clock: *const i64, result: *mut Tm) -> *mut Tm;
    }

    pub fn local_time() -> LocalTime {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // Zeroed rather than uninitialised: if localtime_r somehow fails, the
        // result is a defined midnight-Sunday rather than garbage.
        let mut tm = Tm {
            sec: 0,
            min: 0,
            hour: 0,
            mday: 1,
            mon: 0,
            year: 70,
            wday: 0,
            yday: 0,
            isdst: 0,
            gmtoff: 0,
            zone: std::ptr::null(),
        };
        unsafe {
            if localtime_r(&now, &mut tm).is_null() {
                return LocalTime {
                    hour: 0,
                    minute: 0,
                    weekday: 0,
                };
            }
        }
        LocalTime {
            hour: tm.hour.clamp(0, 23) as u8,
            minute: tm.min.clamp(0, 59) as u8,
            weekday: tm.wday.clamp(0, 6) as u8,
        }
    }
}

#[cfg(windows)]
mod imp_time {
    use super::LocalTime;

    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(time: *mut SystemTime);
    }

    pub fn local_time() -> LocalTime {
        let mut time = SystemTime::default();
        unsafe { GetLocalTime(&mut time) };
        LocalTime {
            hour: time.hour.min(23) as u8,
            minute: time.minute.min(59) as u8,
            weekday: time.day_of_week.min(6) as u8,
        }
    }
}

/// Shows a desktop notification, best effort.
///
/// Never fails the caller: a missing notification daemon is not a reason for a
/// finished download to report an error.
pub fn notify(title: &str, body: &str) {
    use std::process::Command;
    // Anything the server or a filename supplied could contain shell-hostile
    // characters, so every value is passed as a separate argument and never
    // interpolated into a command line.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("notify-send")
            .args(["--app-name=Hydra", "--icon=folder-download", title, body])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        // osascript takes one script string, so quotes have to be neutralised.
        let escape = |s: &str| s.replace('\\', "").replace('"', "'");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escape(body),
            escape(title)
        );
        let _ = Command::new("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(windows)]
    {
        // The toast API is only reachable through PowerShell without pulling in
        // WinRT bindings. Values are passed through the environment rather than
        // the script text so a filename cannot inject PowerShell.
        const SCRIPT: &str = "\
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] > $null; \
$t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent(1); \
$n = $t.GetElementsByTagName('text'); \
$n.Item(0).AppendChild($t.CreateTextNode($env:HYDRA_TITLE)) > $null; \
$n.Item(1).AppendChild($t.CreateTextNode($env:HYDRA_BODY)) > $null; \
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Hydra').Show([Windows.UI.Notifications.ToastNotification]::new($t))";
        let _ = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-WindowStyle",
                "Hidden",
                "-Command",
                SCRIPT,
            ])
            .env("HYDRA_TITLE", title)
            .env("HYDRA_BODY", body)
            .spawn();
    }
}

/// Runs a user-supplied program after a download or queue finishes.
///
/// The command is split on whitespace and executed directly rather than
/// through a shell, so a filename containing `;` or `&&` cannot become a
/// second command.
pub fn run_program(command: &str, argument: Option<&str>) -> io::Result<()> {
    let mut parts = command.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut process = std::process::Command::new(program);
    process.args(parts);
    if let Some(argument) = argument {
        process.arg(argument);
    }
    process.spawn().map(|_| ())
}

/// Reads the system clipboard as text, if a tool for it is available.
///
/// Shelling out rather than binding a clipboard API: the daemon often runs
/// headless, where there is no clipboard at all, and the failure mode of a
/// missing helper should be "this feature is off" rather than a link error at
/// build time.
pub fn read_clipboard() -> Option<String> {
    use std::process::Command;
    let run = |program: &str, args: &[&str]| -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Wayland first, then X11; a session may have either.
        run("wl-paste", &["--no-newline"])
            .or_else(|| run("xclip", &["-o", "-selection", "clipboard"]))
            .or_else(|| run("xsel", &["--clipboard", "--output"]))
    }
    #[cfg(target_os = "macos")]
    {
        run("pbpaste", &[])
    }
    #[cfg(windows)]
    {
        run(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", "Get-Clipboard"],
        )
    }
}
