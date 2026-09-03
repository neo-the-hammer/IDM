//! The `.hdm` sidecar that lets an interrupted download continue.
//!
//! Resuming safely is the whole problem. Continuing to write into a file whose
//! remote content changed since the last run produces a file that is neither
//! the old version nor the new one, passes every length check, and fails only
//! when the user finally opens it. So the sidecar records the server's
//! validators alongside the progress, and the engine refuses to resume unless
//! they still match.

use hdm_json::{json, parse, Json};
use std::io;
use std::path::{Path, PathBuf};

/// Bumped if the on-disk shape ever changes incompatibly.
const FORMAT_VERSION: u64 = 1;

/// One contiguous span of the file and how much of it is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentRecord {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
    /// Bytes completed, counted from `start`.
    pub done: u64,
}

impl SegmentRecord {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start) + 1
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_complete(&self) -> bool {
        self.done >= self.len()
    }

    /// The next byte offset this segment needs.
    pub fn position(&self) -> u64 {
        self.start + self.done
    }
}

/// Everything needed to pick a download back up.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeState {
    pub url: String,
    pub total: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub segments: Vec<SegmentRecord>,
    /// Unix seconds, for reporting a stale partial file to the user.
    pub created_at: u64,
}

/// Why a resume attempt was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeVerdict {
    /// Safe to continue.
    Resume,
    /// The remote file changed; start over.
    Restart(String),
}

impl ResumeState {
    /// Total bytes already downloaded.
    pub fn downloaded(&self) -> u64 {
        self.segments.iter().map(|s| s.done.min(s.len())).sum()
    }

    pub fn is_complete(&self) -> bool {
        !self.segments.is_empty() && self.segments.iter().all(SegmentRecord::is_complete)
    }

    /// Decides whether the partial file on disk still corresponds to what the
    /// server is serving now.
    ///
    /// A strong `ETag` is the best evidence and is trusted on its own. A weak
    /// ETag (`W/"..."`) only promises semantic equivalence, not byte equality,
    /// so it is not enough to resume a byte-range download. `Last-Modified`
    /// plus an identical length is the accepted fallback, which is what every
    /// mainstream download manager does.
    pub fn validate(
        &self,
        remote_total: Option<u64>,
        remote_etag: Option<&str>,
        remote_last_modified: Option<&str>,
    ) -> ResumeVerdict {
        if let (Some(saved), Some(remote)) = (self.total, remote_total) {
            if saved != remote {
                return ResumeVerdict::Restart(format!(
                    "the file size changed from {saved} to {remote} bytes"
                ));
            }
        }

        match (&self.etag, remote_etag) {
            (Some(saved), Some(remote)) => {
                if saved != remote {
                    return ResumeVerdict::Restart(
                        "the server's ETag changed, so the file is not the one we started".into(),
                    );
                }
                if is_weak_etag(saved) {
                    // A weak validator cannot promise the bytes are identical,
                    // so fall through and demand Last-Modified agreement too.
                    return match (&self.last_modified, remote_last_modified) {
                        (Some(a), Some(b)) if a != b => {
                            ResumeVerdict::Restart("the file's modification time changed".into())
                        }
                        _ => ResumeVerdict::Resume,
                    };
                }
                ResumeVerdict::Resume
            }
            // The server used to send an ETag and no longer does, or vice
            // versa: something about the resource changed. Restarting costs
            // bandwidth; continuing risks a corrupt file.
            (Some(_), None) | (None, Some(_)) => {
                ResumeVerdict::Restart("the server's validators changed".into())
            }
            (None, None) => match (&self.last_modified, remote_last_modified) {
                (Some(saved), Some(remote)) if saved != remote => {
                    ResumeVerdict::Restart("the file's modification time changed".into())
                }
                // No validators at all. The size matching is the only evidence
                // available, and it is what we have already checked.
                _ => ResumeVerdict::Resume,
            },
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "version": FORMAT_VERSION,
            "url": (self.url.as_str()),
            "total": (self.total),
            "etag": (self.etag.clone()),
            "lastModified": (self.last_modified.clone()),
            "createdAt": (self.created_at),
            "segments": (Json::Arr(
                self.segments
                    .iter()
                    .map(|s| json!({"start": (s.start), "end": (s.end), "done": (s.done)}))
                    .collect(),
            ))
        })
    }

    pub fn from_json(value: &Json) -> Option<ResumeState> {
        if value.get("version").and_then(Json::as_u64) != Some(FORMAT_VERSION) {
            return None;
        }
        let segments = value
            .get("segments")?
            .as_arr()?
            .iter()
            .map(|s| {
                Some(SegmentRecord {
                    start: s.get("start")?.as_u64()?,
                    end: s.get("end")?.as_u64()?,
                    done: s.get("done")?.as_u64()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;

        Some(ResumeState {
            url: value.get("url")?.as_str()?.to_string(),
            total: value.get("total").and_then(Json::as_u64),
            etag: value.get("etag").and_then(Json::as_str).map(str::to_string),
            last_modified: value
                .get("lastModified")
                .and_then(Json::as_str)
                .map(str::to_string),
            segments,
            created_at: value.get("createdAt").and_then(Json::as_u64).unwrap_or(0),
        })
    }

    /// Writes the sidecar atomically.
    ///
    /// A torn sidecar is worse than none: it would either lose progress or,
    /// worse, claim bytes are present that are not. Writing to a temporary file
    /// and renaming makes the update all-or-nothing.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_extension("hdm.tmp");
        std::fs::write(&temp, self.to_json().to_string_pretty())?;
        std::fs::rename(&temp, path)
    }

    /// Reads a sidecar, returning `None` if it is missing or unusable.
    ///
    /// A damaged sidecar is treated as absent rather than as an error: the
    /// worst outcome is re-downloading, which is always safe.
    pub fn load(path: &Path) -> Option<ResumeState> {
        let text = std::fs::read_to_string(path).ok()?;
        let value = parse(&text).ok()?;
        let state = ResumeState::from_json(&value)?;
        // Reject a sidecar whose segments do not describe a sane file.
        if state
            .segments
            .iter()
            .any(|s| s.end < s.start || s.done > s.len())
        {
            return None;
        }
        Some(state)
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("hdm.tmp"));
    }
}

/// A weak validator, `W/"..."`, promises only semantic equivalence.
fn is_weak_etag(etag: &str) -> bool {
    etag.trim_start().starts_with("W/")
}

/// The sidecar path for a `.part` file.
pub fn sidecar_path_for(part_path: &Path) -> PathBuf {
    let mut name = part_path.as_os_str().to_os_string();
    name.push(".hdm");
    PathBuf::from(name)
}

/// Splits `total` bytes into `count` roughly equal segments.
///
/// The remainder goes to the last segment rather than being spread out, which
/// keeps every boundary except the final one on a round number — easier to read
/// in the UI and in a sidecar during debugging.
pub fn plan_segments(total: u64, count: u8) -> Vec<SegmentRecord> {
    let count = count.max(1) as u64;
    // No bytes means no segments. Returning a segment for "byte 0" here would
    // have the engine wait for a byte the file does not contain.
    if total == 0 {
        return Vec::new();
    }
    // Never create more segments than there are bytes.
    let count = count.min(total);
    let per = total / count;
    (0..count)
        .map(|i| {
            let start = i * per;
            let end = if i == count - 1 {
                total - 1
            } else {
                start + per - 1
            };
            SegmentRecord {
                start,
                end,
                done: 0,
            }
        })
        .collect()
}
