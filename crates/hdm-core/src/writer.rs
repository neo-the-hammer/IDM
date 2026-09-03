//! The output file.
//!
//! Downloads are written to `name.part` and renamed on success, so a partial
//! file is never mistaken for a finished one — by the user or by Hydra itself.
//! Every segment shares one handle and writes at absolute offsets, so there is
//! no cursor to contend over and no lock on the write path.

use crate::platform;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The suffix for an in-progress download.
pub const PART_SUFFIX: &str = ".part";

pub struct FileWriter {
    file: File,
    part_path: PathBuf,
}

impl FileWriter {
    /// Creates (or reopens) the `.part` file and reserves `size` bytes.
    pub fn create(part_path: &Path, size: u64) -> io::Result<FileWriter> {
        if let Some(parent) = part_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            // Never truncate: reopening is exactly what resume does.
            .truncate(false)
            .open(part_path)?;

        if size > 0 && file.metadata()?.len() != size {
            platform::preallocate(&file, size)?;
        }
        Ok(FileWriter {
            file,
            part_path: part_path.to_path_buf(),
        })
    }

    pub fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        platform::write_all_at(&self.file, offset, buf)
    }

    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        platform::read_at(&self.file, offset, buf)
    }

    pub fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Flushes to disk. Called at checkpoints so a power cut costs at most the
    /// work since the last one.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    pub fn part_path(&self) -> &Path {
        &self.part_path
    }

    /// Trims the file to its true length, for a download whose size was not
    /// known in advance and so could not be preallocated exactly.
    pub fn truncate(&self, size: u64) -> io::Result<()> {
        self.file.set_len(size)
    }

    /// Renames the finished `.part` to its real name.
    ///
    /// Returns the path actually used, which may differ from `target` when a
    /// file of that name already exists.
    pub fn finalize(self, target: &Path, overwrite: bool) -> io::Result<PathBuf> {
        self.file.sync_all()?;
        drop(self.file);

        let destination = if overwrite {
            target.to_path_buf()
        } else {
            unique_path(target)
        };
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&self.part_path, &destination).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot move {} to {}: {e}",
                    self.part_path.display(),
                    destination.display()
                ),
            )
        })?;
        Ok(destination)
    }

    /// Deletes the partial file, for a cancelled download.
    pub fn discard(self) -> io::Result<()> {
        drop(self.file);
        match std::fs::remove_file(&self.part_path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// Finds a free name by inserting ` (n)` before the extension, the way both
/// Windows Explorer and browsers do — never silently overwriting a file the
/// user already has.
pub fn unique_path(target: &Path) -> PathBuf {
    if !target.exists() {
        return target.to_path_buf();
    }
    let parent = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");

    // Split on the first dot of a multi-part extension so that "a.tar.gz"
    // becomes "a (1).tar.gz" rather than "a.tar (1).gz".
    let (stem, extension) = match name.find('.') {
        Some(0) | None => (name, ""),
        Some(i) => (&name[..i], &name[i..]),
    };

    for n in 1..10_000 {
        let candidate = parent.join(format!("{stem} ({n}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Astronomically unlikely; fall back to a timestamp rather than loop forever.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!("{stem} ({stamp}){extension}"))
}

/// The `.part` path for a target file.
pub fn part_path_for(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(PART_SUFFIX);
    PathBuf::from(name)
}
