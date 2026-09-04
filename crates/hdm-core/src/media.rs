//! The media grabber: streams saved as files.
//!
//! An `.m3u8` or `.mpd` is an index, not a video. Handing one to an ordinary
//! downloader produces a few kilobytes of text with a film's name on it, which
//! is the single most common way a download manager disappoints someone.
//!
//! The division of labour matches the site grabber's: **Rust fetches, Python
//! parses.** Every byte still travels through `hdm_net::Client`, so cookies,
//! authentication, proxies, TLS and the browser extension's replayed headers
//! behave exactly as they do for any other download.
//!
//! Two things make this different from the byte-range engine:
//!
//! * There is no `Range` to divide. Parallelism comes from fetching *different
//!   segments* at once, and the order is restored on disk afterwards.
//! * The total size is not knowable in advance. A playlist gives durations,
//!   never byte counts, so the reported total is an estimate that sharpens as
//!   segments land — and is replaced by the true figure at the end.

use crate::engine::{
    build_client, settle, DownloadSpec, Outcome, Shared, Status, CONTROL_CANCEL, CONTROL_RUN,
};
use crate::plugins::PluginHost;
use crate::throttle::Throttle;
use crate::writer::{part_path_for, unique_path, FileWriter};
use hdm_json::{json, Json};
use hdm_net::client::Client;
use hdm_net::http::Request;
use hdm_net::url::Url;
use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The largest manifest that will be read. Anything larger is not a manifest.
const MAX_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
/// The largest single segment that will be held in memory.
///
/// Segments are buffered whole because an encrypted one cannot be decrypted
/// until it is complete. A segment is typically two to ten seconds of video, so
/// this ceiling is orders of magnitude above any real one; it exists to stop a
/// hostile or misdescribed response from consuming memory without limit, not to
/// constrain ordinary streams.
const MAX_SEGMENT_BYTES: usize = 64 * 1024 * 1024;
/// Read buffer per segment fetch.
const CHUNK: usize = 64 * 1024;
/// Media downloads use fewer connections than a plain file: segments are small,
/// and a hundred parallel requests to one CDN looks like an attack.
const MAX_WORKERS: usize = 16;
/// A segment fetch is retried this many times before the download fails.
const SEGMENT_RETRIES: u32 = 4;

// ------------------------------------------------------------------ selection

/// What the user picked out of a manifest.
///
/// Stored with the download, so a media transfer survives a daemon restart and
/// resumes into the same segment directory rather than starting again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSelection {
    /// `hls` or `dash`.
    pub format: String,
    /// The playlist or manifest that lists the segments. For HLS this is the
    /// *variant* playlist, not the master.
    pub url: String,
    /// Which representation, for a DASH manifest that describes several.
    pub stream_id: Option<String>,
    /// A separate audio track to fetch alongside the video.
    ///
    /// DASH almost always keeps them apart, and HLS does whenever a stream has
    /// more than one language. Combining them needs ffmpeg; without it both
    /// files are kept and the fact is reported rather than hidden.
    pub audio_url: Option<String>,
    pub audio_stream_id: Option<String>,
    /// Convert the result to MP4 with ffmpeg when it is available.
    pub remux: bool,
}

impl MediaSelection {
    pub fn new(format: impl Into<String>, url: impl Into<String>) -> MediaSelection {
        MediaSelection {
            format: format.into(),
            url: url.into(),
            stream_id: None,
            audio_url: None,
            audio_stream_id: None,
            remux: false,
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "format": (self.format.as_str()),
            "url": (self.url.as_str()),
            "streamId": (self.stream_id.clone()),
            "audioUrl": (self.audio_url.clone()),
            "audioStreamId": (self.audio_stream_id.clone()),
            "remux": (self.remux)
        })
    }

    pub fn from_json(value: &Json) -> Option<MediaSelection> {
        Some(MediaSelection {
            format: value.get("format")?.as_str()?.to_string(),
            url: value.get("url")?.as_str()?.to_string(),
            stream_id: value
                .get("streamId")
                .and_then(Json::as_str)
                .map(str::to_string),
            audio_url: value
                .get("audioUrl")
                .and_then(Json::as_str)
                .map(str::to_string),
            audio_stream_id: value
                .get("audioStreamId")
                .and_then(Json::as_str)
                .map(str::to_string),
            remux: value.bool_or("remux", false),
        })
    }
}

// --------------------------------------------------------------------- probe

/// One choice a manifest offers.
#[derive(Debug, Clone, Default)]
pub struct MediaStream {
    pub id: String,
    /// The playlist to download this stream from.
    pub url: String,
    /// `video`, `audio` or `text`.
    pub kind: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub bandwidth: Option<u64>,
    pub codecs: String,
    pub language: String,
    /// Zero when the segment list has not been fetched yet, which is the case
    /// for every variant of an HLS master playlist.
    pub segments: usize,
    pub encrypted: bool,
}

impl MediaStream {
    /// A human label such as `1080p · 4.2 Mbit/s · avc1.640028`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match (self.height, self.width) {
            (Some(h), _) if h > 0 => parts.push(format!("{h}p")),
            (_, Some(w)) if w > 0 => parts.push(format!("{w}px wide")),
            _ => {}
        }
        if let Some(bandwidth) = self.bandwidth.filter(|b| *b > 0) {
            // Audio tracks live around 128 kbit/s, where one decimal place of
            // Mbit/s rounds every one of them to the same "0.1".
            parts.push(if bandwidth < 1_000_000 {
                format!("{} kbit/s", bandwidth / 1_000)
            } else {
                format!("{:.1} Mbit/s", bandwidth as f64 / 1_000_000.0)
            });
        }
        if !self.language.is_empty() {
            parts.push(self.language.clone());
        }
        if !self.codecs.is_empty() {
            parts.push(self.codecs.clone());
        }
        if parts.is_empty() {
            parts.push(self.kind.clone());
        }
        parts.join(" · ")
    }

    pub fn to_json(&self) -> Json {
        json!({
            "id": (self.id.as_str()),
            "url": (self.url.as_str()),
            "kind": (self.kind.as_str()),
            "label": (self.label()),
            "width": (self.width),
            "height": (self.height),
            "bandwidth": (self.bandwidth),
            "codecs": (self.codecs.as_str()),
            "language": (self.language.as_str()),
            "segments": (self.segments as u64),
            "encrypted": (self.encrypted)
        })
    }
}

/// What a manifest turned out to contain.
#[derive(Debug, Clone, Default)]
pub struct MediaProbe {
    pub url: String,
    /// `hls` or `dash`.
    pub format: String,
    pub live: bool,
    pub duration: f64,
    pub streams: Vec<MediaStream>,
    /// True when the video streams carry no audio, so one must be chosen too.
    pub separate_audio: bool,
    /// Set when something about the stream limits what Hydra can do with it.
    pub warnings: Vec<String>,
}

impl MediaProbe {
    pub fn to_json(&self) -> Json {
        json!({
            "url": (self.url.as_str()),
            "format": (self.format.as_str()),
            "live": (self.live),
            "duration": (self.duration),
            "separateAudio": (self.separate_audio),
            "streams": (Json::Arr(self.streams.iter().map(MediaStream::to_json).collect())),
            "warnings": (Json::Arr(
                self.warnings.iter().map(|w| Json::Str(w.clone())).collect()
            )),
            "ffmpeg": (ffmpeg_path().is_some())
        })
    }

    /// The stream a caller gets if they express no preference: the best video,
    /// or failing that the best audio.
    pub fn best(&self) -> Option<&MediaStream> {
        self.streams
            .iter()
            .find(|s| s.kind == "video")
            .or_else(|| self.streams.first())
    }

    /// The audio track to pair with `video`, when the video carries none.
    pub fn best_audio(&self) -> Option<&MediaStream> {
        if !self.separate_audio {
            return None;
        }
        self.streams.iter().find(|s| s.kind == "audio")
    }
}

/// Reads a manifest and reports what can be downloaded from it.
pub fn probe(spec: &DownloadSpec) -> Result<MediaProbe, String> {
    let host = PluginHost::discover()
        .map_err(|e| format!("the media grabber needs Python to read manifests, but {e}"))?;
    let client = build_client(spec).map_err(|e| e.to_string())?;
    probe_with(&client, &host, &spec.url)
}

fn probe_with(client: &Client, host: &PluginHost, url: &str) -> Result<MediaProbe, String> {
    let text = fetch_text(client, url)?;
    let parsed = host.manifest(url, &text)?;

    match parsed.str_or("kind", "") {
        "master" => Ok(hls_master(url, &parsed)),
        "media" => Ok(hls_media(url, &parsed)),
        "dash" => Ok(dash(url, &parsed)),
        other => Err(format!("unrecognised manifest kind `{other}`")),
    }
}

fn hls_master(url: &str, parsed: &Json) -> MediaProbe {
    let mut streams = Vec::new();
    for (index, variant) in array(parsed, "variants").iter().enumerate() {
        streams.push(MediaStream {
            id: format!("v{index}"),
            url: variant.str_or("url", "").to_string(),
            kind: "video".into(),
            width: variant.get("width").and_then(Json::as_u64),
            height: variant.get("height").and_then(Json::as_u64),
            bandwidth: variant.get("bandwidth").and_then(Json::as_u64),
            codecs: variant.str_or("codecs", "").to_string(),
            ..MediaStream::default()
        });
    }
    let audio = array(parsed, "audio");
    for (index, rendition) in audio.iter().enumerate() {
        streams.push(MediaStream {
            id: format!("a{index}"),
            url: rendition.str_or("url", "").to_string(),
            kind: "audio".into(),
            language: rendition.str_or("language", "").to_string(),
            ..MediaStream::default()
        });
    }
    MediaProbe {
        url: url.to_string(),
        format: "hls".into(),
        // A master playlist does not say; the variant will.
        live: false,
        duration: 0.0,
        // HLS variants normally carry their own audio. A separate rendition is
        // an *alternative* — a second language, or descriptive audio — not a
        // missing track, so it is offered rather than required.
        separate_audio: false,
        streams,
        warnings: Vec::new(),
    }
}

fn hls_media(url: &str, parsed: &Json) -> MediaProbe {
    let live = parsed.bool_or("live", false);
    let mut warnings = Vec::new();
    for method in array(parsed, "encryptionMethods") {
        let method = method.as_str().unwrap_or_default();
        if method != "AES-128" && !method.is_empty() {
            warnings.push(format!(
                "This stream uses {method} encryption, which Hydra cannot decrypt."
            ));
        }
    }
    if live {
        warnings.push(
            "This is a live stream: it has no end, so only what has already \
             been published will be saved."
                .into(),
        );
    }
    MediaProbe {
        url: url.to_string(),
        format: "hls".into(),
        live,
        duration: parsed.get("duration").and_then(Json::as_f64).unwrap_or(0.0),
        separate_audio: false,
        streams: vec![MediaStream {
            id: "0".into(),
            url: url.to_string(),
            kind: "video".into(),
            segments: parsed.u64_or("count", 0) as usize,
            encrypted: parsed.bool_or("encrypted", false),
            ..MediaStream::default()
        }],
        warnings,
    }
}

fn dash(url: &str, parsed: &Json) -> MediaProbe {
    let mut streams = Vec::new();
    for stream in array(parsed, "streams") {
        streams.push(MediaStream {
            id: stream.str_or("id", "").to_string(),
            url: url.to_string(),
            kind: stream.str_or("contentType", "unknown").to_string(),
            width: stream.get("width").and_then(Json::as_u64),
            height: stream.get("height").and_then(Json::as_u64),
            bandwidth: stream.get("bandwidth").and_then(Json::as_u64),
            codecs: stream.str_or("codecs", "").to_string(),
            segments: stream.u64_or("count", 0) as usize,
            encrypted: stream.bool_or("encrypted", false),
            ..MediaStream::default()
        });
    }
    let mut warnings = Vec::new();
    if parsed.bool_or("encrypted", false) {
        warnings.push(
            "This manifest is protected by DRM; the segments can be fetched, \
             but the result will not play."
                .into(),
        );
    }
    // DASH keeps video and audio in separate representations essentially
    // always, so both have to be fetched and then combined.
    let separate_audio =
        streams.iter().any(|s| s.kind == "video") && streams.iter().any(|s| s.kind == "audio");
    if separate_audio && ffmpeg_path().is_none() {
        warnings.push(
            "This stream keeps video and audio apart and ffmpeg was not found, \
             so they will be saved as two files."
                .into(),
        );
    }
    MediaProbe {
        url: url.to_string(),
        format: "dash".into(),
        live: parsed.bool_or("live", false),
        duration: parsed.get("duration").and_then(Json::as_f64).unwrap_or(0.0),
        separate_audio,
        streams,
        warnings,
    }
}

// ------------------------------------------------------------------ segments

/// One thing to fetch.
#[derive(Debug, Clone)]
struct Segment {
    url: String,
    /// Set for a playlist that packs many segments into one file.
    byte_range: Option<(u64, u64)>,
    /// The media sequence number, which doubles as the implicit AES IV.
    sequence: u64,
    key: Option<KeyRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyRef {
    method: String,
    uri: String,
    /// The explicit IV, when the playlist gives one.
    iv: Option<Vec<u8>>,
}

/// A stream resolved down to the exact list of things to fetch.
#[derive(Debug, Clone, Default)]
struct Plan {
    /// The fMP4 header, which must precede every segment or nothing plays.
    init: Option<Segment>,
    segments: Vec<Segment>,
    /// `ts` or `mp4`, which decides both the extension and whether a remux is
    /// needed for the result to be widely playable.
    container: String,
    kind: String,
    /// True when the segment count was derived from a declared duration rather
    /// than listed, so the final segment may not exist.
    ///
    /// Real manifests declare durations like `PT16.016S`; whether that is four
    /// segments or five is genuinely ambiguous, and a downloader that fails the
    /// whole stream over the ambiguity is no use. The tail is allowed to be
    /// absent — but only the tail, and only for these.
    estimated_tail: bool,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.init.is_none()
    }
}

/// Turns a selection into the concrete segment list.
fn resolve(
    client: &Client,
    host: &PluginHost,
    format: &str,
    url: &str,
    stream_id: Option<&str>,
    kind: &str,
) -> Result<Plan, String> {
    let text = fetch_text(client, url)?;
    let parsed = host.manifest(url, &text)?;

    match parsed.str_or("kind", "") {
        "media" => Ok(plan_from_hls(&parsed, kind)),
        "master" => {
            // A master playlist was selected directly. Take its best variant
            // rather than failing on a technicality.
            let probe = hls_master(url, &parsed);
            let best = probe
                .best()
                .ok_or("the master playlist offers no streams")?
                .clone();
            let text = fetch_text(client, &best.url)?;
            let parsed = host.hls(&best.url, &text)?;
            Ok(plan_from_hls(&parsed, kind))
        }
        "dash" => plan_from_dash(&parsed, stream_id, kind),
        other => Err(format!(
            "{url} is a `{other}`, which is not something {format} can download"
        )),
    }
}

fn plan_from_hls(parsed: &Json, kind: &str) -> Plan {
    let init_url = parsed.get("initSegment").and_then(Json::as_str);
    let segments: Vec<Segment> = array(parsed, "segments")
        .iter()
        .map(|segment| Segment {
            url: segment.str_or("url", "").to_string(),
            byte_range: segment.get("byteRange").and_then(|range| {
                Some((
                    range.get("offset")?.as_u64()?,
                    range.get("length")?.as_u64()?,
                ))
            }),
            sequence: segment.u64_or("sequence", 0),
            key: segment.get("encryption").and_then(key_ref),
        })
        .filter(|segment| !segment.url.is_empty())
        .collect();

    Plan {
        init: init_url.map(|url| Segment {
            url: url.to_string(),
            byte_range: None,
            sequence: 0,
            key: None,
        }),
        // An init segment means fragmented MP4; without one, HLS is MPEG-TS.
        container: if init_url.is_some() { "mp4" } else { "ts" }.into(),
        segments,
        kind: kind.to_string(),
        estimated_tail: false,
    }
}

fn plan_from_dash(parsed: &Json, stream_id: Option<&str>, kind: &str) -> Result<Plan, String> {
    let streams = array(parsed, "streams");
    let chosen = match stream_id {
        Some(wanted) => streams
            .iter()
            .find(|s| s.str_or("id", "") == wanted)
            .ok_or_else(|| format!("the manifest has no stream `{wanted}`"))?,
        None => streams
            .iter()
            .find(|s| s.str_or("contentType", "") == kind)
            .or_else(|| streams.first())
            .ok_or("the manifest offers no streams")?,
    };

    let plain = |url: &str| Segment {
        url: url.to_string(),
        byte_range: None,
        sequence: 0,
        key: None,
    };
    Ok(Plan {
        init: chosen.get("initSegment").and_then(Json::as_str).map(&plain),
        segments: array(chosen, "segments")
            .iter()
            .filter_map(Json::as_str)
            .map(&plain)
            .collect(),
        // DASH is fragmented MP4 in all but vanishingly rare cases.
        container: "mp4".into(),
        kind: kind.to_string(),
        estimated_tail: chosen.bool_or("estimatedCount", false),
    })
}

fn key_ref(encryption: &Json) -> Option<KeyRef> {
    let method = encryption.get("method")?.as_str()?;
    if method == "NONE" {
        return None;
    }
    Some(KeyRef {
        method: method.to_string(),
        uri: encryption.get("uri")?.as_str()?.to_string(),
        iv: encryption
            .get("iv")
            .and_then(Json::as_str)
            .and_then(|iv| hdm_crypto::unhex(iv.trim_start_matches("0x").trim_start_matches("0X")))
            .filter(|iv| iv.len() == hdm_crypto::AES_BLOCK),
    })
}

// ------------------------------------------------------------------ download

/// Runs a media download to completion, blocking the calling thread.
///
/// Mirrors [`crate::engine::run`] exactly — same arguments, same outcome, same
/// pause and cancel signals — so the manager, the API and the UI need to know
/// nothing about how a stream differs from a file.
pub fn run(
    spec: &DownloadSpec,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
) -> io::Result<Outcome> {
    settle(shared, run_inner(spec, shared, throttle))
}

fn run_inner(
    spec: &DownloadSpec,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
) -> io::Result<Outcome> {
    let selection = spec
        .media
        .as_ref()
        .ok_or_else(|| invalid("this download carries no media selection"))?;

    shared.set_status(Status::Probing);
    let host = PluginHost::discover().map_err(|e| {
        invalid(format!(
            "the media grabber needs Python to read manifests, but {e}"
        ))
    })?;
    let client = Arc::new(build_client(spec)?);

    let video = resolve(
        &client,
        &host,
        &selection.format,
        &selection.url,
        selection.stream_id.as_deref(),
        "video",
    )
    .map_err(invalid)?;
    if video.is_empty() {
        return Err(invalid("the manifest lists no segments to download"));
    }

    let audio = match &selection.audio_url {
        Some(url) => {
            let plan = resolve(
                &client,
                &host,
                &selection.format,
                url,
                selection.audio_stream_id.as_deref(),
                "audio",
            )
            .map_err(invalid)?;
            (!plan.is_empty()).then_some(plan)
        }
        None => None,
    };

    let connections = (spec.connections as usize).clamp(1, MAX_WORKERS);
    let base_name = output_name(spec, selection, &video, audio.is_some());
    shared.set_filename(&base_name);
    let target = spec.directory.join(&base_name);

    let total_segments = video.count() + audio.as_ref().map(Plan::count).unwrap_or(0);
    let progress = Arc::new(Progress::new(total_segments));
    let monitor = spawn_monitor(shared.clone(), progress.clone());

    shared.set_status(Status::Downloading);
    let mut parts: Vec<(PathBuf, String)> = Vec::new();
    let mut result = Ok(());

    for (index, plan) in std::iter::once(&video).chain(audio.as_ref()).enumerate() {
        // The two tracks share one working directory, so a paused download
        // resumes both from where they stopped.
        let work = work_dir(&target, index);
        match fetch_plan(
            plan,
            &work,
            &client,
            connections,
            shared,
            throttle,
            &progress,
        ) {
            Ok(Fetched::Stopped(outcome)) => {
                monitor.stop();
                return finish_stopped(outcome, &target, audio.is_some());
            }
            Ok(Fetched::Done) => {}
            Err(e) => {
                result = Err(e);
                break;
            }
        }
        let assembled = work.join(format!("track.{}", plan.container));
        if let Err(e) = assemble(plan, &work, &assembled) {
            result = Err(e);
            break;
        }
        parts.push((assembled, plan.kind.clone()));
    }
    monitor.stop();
    result?;

    shared.set_status(Status::Verifying);
    let combined = combine(&parts, &target, selection, spec.overwrite)?;

    // Only now is the real size known; replacing the estimate keeps the record
    // and the file on disk in agreement.
    let bytes = std::fs::metadata(&combined).map(|m| m.len()).unwrap_or(0);
    shared.set_total(bytes);
    shared.set_downloaded(bytes);
    shared.set_filename(
        &combined
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(base_name),
    );

    for index in 0..parts.len() {
        let _ = std::fs::remove_dir_all(work_dir(&target, index));
    }
    Ok(Outcome::Completed {
        path: combined,
        bytes,
    })
}

impl Plan {
    fn count(&self) -> usize {
        self.segments.len() + usize::from(self.init.is_some())
    }
}

/// A paused download keeps its segment directories; a cancelled one does not.
fn finish_stopped(outcome: Outcome, target: &Path, has_audio: bool) -> io::Result<Outcome> {
    if outcome == Outcome::Cancelled {
        for index in 0..(1 + usize::from(has_audio)) {
            let _ = std::fs::remove_dir_all(work_dir(target, index));
        }
        let _ = std::fs::remove_file(part_path_for(target));
    }
    Ok(outcome)
}

enum Fetched {
    Done,
    Stopped(Outcome),
}

/// Shared counters, so the monitor thread can report progress without holding
/// anything the workers need.
struct Progress {
    total_segments: usize,
    done: AtomicUsize,
    /// Bytes belonging to segments that are safely on disk.
    bytes: AtomicUsize,
    /// Bytes read by segments still in flight.
    ///
    /// Counted separately because they are not yet part of the result and must
    /// come back off if the fetch fails. Without them a download of ten-second
    /// segments on a slow link reads as motionless for seconds at a time,
    /// which looks exactly like a stall.
    in_flight: AtomicUsize,
}

impl Progress {
    fn new(total_segments: usize) -> Progress {
        Progress {
            total_segments,
            done: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            in_flight: AtomicUsize::new(0),
        }
    }

    /// Everything fetched so far, finished or not.
    fn transferred(&self) -> u64 {
        (self.bytes.load(Ordering::Acquire) + self.in_flight.load(Ordering::Acquire)) as u64
    }

    /// The projected final size, from the average segment seen so far.
    ///
    /// Deliberately an estimate rather than nothing: a progress bar that says
    /// "1.2 GB of unknown" for twenty minutes is worse than one that is within
    /// a few percent and converges.
    fn estimated_total(&self) -> u64 {
        let done = self.done.load(Ordering::Acquire);
        let bytes = self.bytes.load(Ordering::Acquire) as u64;
        if done == 0 || self.total_segments == 0 {
            return 0;
        }
        bytes / done as u64 * self.total_segments as u64
    }
}

struct Monitor {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Monitor {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_monitor(shared: Arc<Shared>, progress: Arc<Progress>) -> Monitor {
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        std::thread::Builder::new()
            .name("hydra-media-monitor".into())
            .spawn(move || {
                let mut previous = 0u64;
                let mut last = Instant::now();
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(500));
                    let now = progress.transferred();
                    let elapsed = last.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        let speed = ((now.saturating_sub(previous)) as f64 / elapsed) as u64;
                        // Smoothed the same way the engine smooths its own, so
                        // the two read alike in the UI.
                        let smoothed = (shared.speed() * 2 + speed) / 3;
                        shared.set_speed(smoothed);
                    }
                    previous = now;
                    last = Instant::now();
                    shared.set_downloaded(now);
                    shared.set_total(progress.estimated_total());
                }
                shared.set_speed(0);
            })
            .expect("cannot spawn the media monitor thread")
    };
    Monitor {
        stop,
        handle: Some(handle),
    }
}

/// Fetches every segment of one plan into `work`, in parallel.
#[allow(clippy::too_many_arguments)]
fn fetch_plan(
    plan: &Plan,
    work: &Path,
    client: &Arc<Client>,
    connections: usize,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
    progress: &Arc<Progress>,
) -> io::Result<Fetched> {
    std::fs::create_dir_all(work)?;

    // The init segment is fetched first and alone: every other segment is
    // meaningless without it, so there is no point starting them in parallel
    // with something that might fail.
    let mut queue: Vec<(usize, Segment)> = Vec::with_capacity(plan.count());
    let mut optional_positions: Vec<usize> = Vec::new();
    if let Some(init) = &plan.init {
        queue.push((0, init.clone()));
    }
    let offset = usize::from(plan.init.is_some());
    let last = plan.segments.len().saturating_sub(1);
    for (index, segment) in plan.segments.iter().enumerate() {
        let optional = plan.estimated_tail && index == last;
        queue.push((index + offset, segment.clone()));
        if optional {
            optional_positions.push(index + offset);
        }
    }

    let queue = Arc::new(queue);
    let optional_positions = Arc::new(optional_positions);
    let next = Arc::new(AtomicUsize::new(0));
    let keys: KeyCache = Arc::new(Mutex::new(HashMap::new()));
    let failure: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // One worker per segment at most: spinning up sixteen threads for a
    // four-segment clip helps nobody.
    let worker_count = queue.len().clamp(1, connections);

    let mut handles = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let queue = queue.clone();
        let optional_positions = optional_positions.clone();
        let next = next.clone();
        let keys = keys.clone();
        let failure = failure.clone();
        let client = Arc::new(client.share());
        let shared = shared.clone();
        let throttle = throttle.clone();
        let progress = progress.clone();
        let work = work.to_path_buf();

        handles.push(
            std::thread::Builder::new()
                .name("hydra-media".into())
                .spawn(move || loop {
                    if shared.should_stop() || failure.lock().unwrap().is_some() {
                        return;
                    }
                    let index = next.fetch_add(1, Ordering::AcqRel);
                    let Some((position, segment)) = queue.get(index) else {
                        return;
                    };
                    let optional = optional_positions.contains(position);
                    match one_segment(
                        &client, &work, *position, segment, optional, &keys, &shared, &throttle,
                        &progress,
                    ) {
                        Ok(()) => {}
                        Err(e) => {
                            let mut slot = failure.lock().unwrap();
                            if slot.is_none() {
                                *slot = Some(e.to_string());
                            }
                            return;
                        }
                    }
                })
                .expect("cannot spawn a media worker thread"),
        );
    }
    for handle in handles {
        let _ = handle.join();
    }

    if let Some(error) = failure.lock().unwrap().take() {
        // A stop request interrupts sockets, which surfaces as an I/O error;
        // that is a pause, not a failure.
        if !shared.should_stop() {
            return Err(io::Error::other(error));
        }
    }
    match shared.control() {
        CONTROL_RUN => Ok(Fetched::Done),
        CONTROL_CANCEL => Ok(Fetched::Stopped(Outcome::Cancelled)),
        _ => Ok(Fetched::Stopped(Outcome::Paused)),
    }
}

/// Fetches, decrypts and stores one segment.
///
/// The file is written under a temporary name and renamed on success, so a
/// `.seg` that exists is a `.seg` that is complete — which is the whole of the
/// resume logic for a media download.
#[allow(clippy::too_many_arguments)]
fn one_segment(
    client: &Arc<Client>,
    work: &Path,
    position: usize,
    segment: &Segment,
    optional: bool,
    keys: &KeyCache,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
    progress: &Arc<Progress>,
) -> io::Result<()> {
    let final_path = work.join(format!("{position:06}.seg"));
    if let Ok(metadata) = std::fs::metadata(&final_path) {
        // Already fetched by an earlier run. Count it so the progress bar
        // reflects a resumed download rather than restarting at zero.
        progress.done.fetch_add(1, Ordering::AcqRel);
        progress
            .bytes
            .fetch_add(metadata.len() as usize, Ordering::AcqRel);
        return Ok(());
    }

    let mut last_error = None;
    for attempt in 0..=SEGMENT_RETRIES {
        if shared.should_stop() {
            return Ok(());
        }
        if attempt > 0 {
            // Back off, but stay responsive to a pause.
            for _ in 0..(attempt * 4) {
                if shared.should_stop() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        match fetch_segment(client, segment, shared, throttle, progress) {
            Ok(body) => {
                let plain = decrypt(body, segment, client, keys)?;
                write_segment(work, position, &final_path, &plain)?;
                progress.done.fetch_add(1, Ordering::AcqRel);
                progress.bytes.fetch_add(plain.len(), Ordering::AcqRel);
                return Ok(());
            }
            Err(e) => {
                if e.permanent && optional {
                    // A derived count that ran one past the end. An empty
                    // segment contributes nothing to the concatenation, which
                    // is exactly right.
                    write_segment(work, position, &final_path, &[])?;
                    progress.done.fetch_add(1, Ordering::AcqRel);
                    return Ok(());
                }
                let permanent = e.permanent;
                last_error = Some(e.error);
                // A 404 will still be a 404 in four seconds; retrying it only
                // makes the failure slower to report.
                if permanent {
                    break;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("the segment could not be fetched")))
}

/// A segment fetch that failed, and whether trying again could possibly help.
struct SegmentError {
    error: io::Error,
    permanent: bool,
}

impl From<io::Error> for SegmentError {
    fn from(error: io::Error) -> SegmentError {
        SegmentError {
            error,
            permanent: false,
        }
    }
}

/// Writes a segment under a temporary name and renames it into place, so a
/// `.seg` that exists is a `.seg` that is complete. That single property is
/// the whole of the resume logic for a media download.
fn write_segment(work: &Path, position: usize, final_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = work.join(format!("{position:06}.part"));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, final_path)
}

fn fetch_segment(
    client: &Arc<Client>,
    segment: &Segment,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
    progress: &Arc<Progress>,
) -> Result<Vec<u8>, SegmentError> {
    let url = Url::parse(&segment.url).map_err(|e| SegmentError {
        error: invalid(e),
        permanent: true,
    })?;
    let mut request = Request::get(url);
    if let Some((offset, length)) = segment.byte_range {
        request = request.with_range(offset, Some(offset + length - 1));
    }

    let mut fetch = client.send(request)?;
    let status = fetch.response.status;
    if status >= 400 {
        fetch.response.shutdown();
        return Err(SegmentError {
            error: io::Error::other(format!("{} answered {status}", segment.url)),
            // 408 and 429 are the client errors that say "later", not "never".
            permanent: (400..500).contains(&status) && !matches!(status, 408 | 429),
        });
    }

    // Register the socket so pause and cancel interrupt a blocking read rather
    // than waiting out the timeout, exactly as the byte-range engine does.
    let handle = fetch.response.shutdown_handle().ok().map(Arc::new);
    if let Some(handle) = &handle {
        shared.register_connection(handle.clone());
    }

    let mut body = Vec::new();
    let mut buffer = vec![0u8; CHUNK];
    let result = (|| -> io::Result<()> {
        loop {
            if shared.should_stop() {
                return Err(io::Error::other("stopped"));
            }
            // Ask the throttle first, so a limited download is limited on the
            // way in rather than after the bytes have already arrived.
            let allowed = throttle.take(buffer.len()).max(1).min(buffer.len());
            let read = fetch.response.body.read(&mut buffer[..allowed])?;
            if read == 0 {
                return Ok(());
            }
            if body.len() + read > MAX_SEGMENT_BYTES {
                return Err(io::Error::other(format!(
                    "{} is larger than a media segment can be",
                    segment.url
                )));
            }
            body.extend_from_slice(&buffer[..read]);
            progress.in_flight.fetch_add(read, Ordering::AcqRel);
        }
    })();

    if let Some(handle) = &handle {
        shared.unregister_connection(handle);
    }
    // Hand the segment's bytes back: on success the caller counts them as
    // finished, and on failure a retry would otherwise count them twice.
    progress.in_flight.fetch_sub(body.len(), Ordering::AcqRel);
    result?;

    if let Some(expected) = fetch.response.content_length() {
        if body.len() as u64 != expected {
            return Err(SegmentError::from(io::Error::other(format!(
                "{} was cut short at {} of {expected} bytes",
                segment.url,
                body.len()
            ))));
        }
    }
    Ok(body)
}

/// Decryption keys, one entry per key URI.
///
/// The inner mutex is what makes a key fetched *once* rather than once per
/// worker: a plain map would let every thread miss the cache simultaneously and
/// all fetch, which on a stream with sixteen workers means sixteen requests to
/// a key server that is very often rate-limited. Holding the entry's lock
/// across the fetch serialises only the workers waiting on that same key;
/// a different key still fetches in parallel.
type KeyCache = Arc<Mutex<HashMap<String, Arc<Mutex<Option<Vec<u8>>>>>>>;

fn decrypt(
    body: Vec<u8>,
    segment: &Segment,
    client: &Arc<Client>,
    keys: &KeyCache,
) -> io::Result<Vec<u8>> {
    let Some(key_ref) = &segment.key else {
        return Ok(body);
    };
    if key_ref.method != "AES-128" {
        return Err(invalid(format!(
            "this stream uses {} encryption, which Hydra cannot decrypt",
            key_ref.method
        )));
    }

    let entry = keys
        .lock()
        .unwrap()
        .entry(key_ref.uri.clone())
        .or_default()
        .clone();
    let mut slot = entry.lock().unwrap();
    let key = match slot.as_ref() {
        Some(key) => key.clone(),
        None => {
            let key = fetch_key(client, &key_ref.uri)?;
            *slot = Some(key.clone());
            key
        }
    };
    drop(slot);

    // With no explicit IV the sequence number *is* the IV, as a big-endian
    // 128-bit integer. Getting this wrong produces plausible-looking noise
    // rather than an error, which is why it is worth spelling out.
    let iv = match &key_ref.iv {
        Some(iv) => iv.clone(),
        None => {
            let mut iv = vec![0u8; hdm_crypto::AES_BLOCK];
            iv[8..].copy_from_slice(&segment.sequence.to_be_bytes());
            iv
        }
    };
    hdm_crypto::cbc_decrypt(&key, &iv, &body).map_err(io::Error::other)
}

fn fetch_key(client: &Arc<Client>, uri: &str) -> io::Result<Vec<u8>> {
    let url = Url::parse(uri).map_err(invalid)?;
    let mut fetch = client.send(Request::get(url))?;
    if fetch.response.status != 200 {
        fetch.response.shutdown();
        return Err(io::Error::other(format!(
            "the decryption key at {uri} answered {}",
            fetch.response.status
        )));
    }
    let key = fetch.response.read_to_vec(1024)?;
    if key.len() != 16 {
        return Err(io::Error::other(format!(
            "the decryption key at {uri} is {} bytes, not 16",
            key.len()
        )));
    }
    Ok(key)
}

// ------------------------------------------------------------------ assembly

/// Concatenates the fetched segments into one file, in playlist order.
fn assemble(plan: &Plan, work: &Path, target: &Path) -> io::Result<()> {
    let count = plan.count();
    let mut total = 0u64;
    for position in 0..count {
        let path = work.join(format!("{position:06}.seg"));
        total += std::fs::metadata(&path)
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("segment {position} is missing after the download: {e}"),
                )
            })?
            .len();
    }

    let part = part_path_for(target);
    let writer = FileWriter::create(&part, total)?;
    let mut offset = 0u64;
    for position in 0..count {
        let bytes = std::fs::read(work.join(format!("{position:06}.seg")))?;
        writer.write_at(offset, &bytes)?;
        offset += bytes.len() as u64;
    }
    writer.finalize(target, true)?;

    // The individual segments are dead weight once concatenated, and a
    // multi-gigabyte stream would otherwise occupy twice its size.
    for position in 0..count {
        let _ = std::fs::remove_file(work.join(format!("{position:06}.seg")));
    }
    Ok(())
}

/// Produces the final file, muxing or remuxing with ffmpeg where it helps.
fn combine(
    parts: &[(PathBuf, String)],
    target: &Path,
    selection: &MediaSelection,
    overwrite: bool,
) -> io::Result<PathBuf> {
    let destination = if overwrite {
        target.to_path_buf()
    } else {
        unique_path(target)
    };
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match (parts.len(), ffmpeg_path()) {
        // Two tracks and ffmpeg: one file with both, which is what was asked
        // for.
        (2, Some(ffmpeg)) => {
            run_ffmpeg(
                &ffmpeg,
                &[
                    "-i".as_ref(),
                    parts[0].0.as_os_str(),
                    "-i".as_ref(),
                    parts[1].0.as_os_str(),
                    "-c".as_ref(),
                    "copy".as_ref(),
                    "-movflags".as_ref(),
                    "+faststart".as_ref(),
                    destination.as_os_str(),
                ],
            )?;
            Ok(destination)
        }
        // Two tracks and no ffmpeg: both are kept, named so it is obvious what
        // they are. Silently discarding the audio would be worse.
        (2, None) => {
            let video = with_suffix(&destination, ".video");
            let audio = with_suffix(&destination, ".audio");
            std::fs::rename(&parts[0].0, &video)?;
            std::fs::rename(&parts[1].0, &audio)?;
            Ok(video)
        }
        (1, ffmpeg) => {
            let source = &parts[0].0;
            match (selection.remux, ffmpeg) {
                (true, Some(ffmpeg)) => {
                    run_ffmpeg(
                        &ffmpeg,
                        &[
                            "-i".as_ref(),
                            source.as_os_str(),
                            "-c".as_ref(),
                            "copy".as_ref(),
                            // MPEG-TS carries AAC in ADTS frames, which MP4
                            // cannot hold; this converts the headers without
                            // re-encoding.
                            "-bsf:a".as_ref(),
                            "aac_adtstoasc".as_ref(),
                            "-movflags".as_ref(),
                            "+faststart".as_ref(),
                            destination.as_os_str(),
                        ],
                    )?;
                    Ok(destination)
                }
                _ => {
                    std::fs::rename(source, &destination)?;
                    Ok(destination)
                }
            }
        }
        _ => Err(io::Error::other("the download produced no tracks")),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    let extension = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    path.with_file_name(format!("{stem}{suffix}{extension}"))
}

/// Where ffmpeg is, if it is anywhere.
///
/// Entirely optional. Everything works without it; what it adds is combining
/// separate video and audio into one file, and converting MPEG-TS to MP4.
pub fn ffmpeg_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("HYDRA_FFMPEG") {
        let path = PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn run_ffmpeg(ffmpeg: &Path, args: &[&std::ffi::OsStr]) -> io::Result<()> {
    let output = std::process::Command::new(ffmpeg)
        .arg("-nostdin")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no detail given");
    Err(io::Error::other(format!("ffmpeg failed: {detail}")))
}

// -------------------------------------------------------------------- naming

/// Where the segments for track `index` of `target` live while downloading.
fn work_dir(target: &Path, index: usize) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "media".into());
    target.with_file_name(format!(".{name}.track{index}.hdm"))
}

/// Deletes the segment directories belonging to `target`.
///
/// A media download's partial state is a directory of segments rather than one
/// `.part` file, so removing or restarting one has to clear that too — or a
/// restart would silently reuse every segment it was meant to discard.
pub fn discard_partial(target: &Path) {
    // Two is the most any download uses (video and audio); a few extra
    // attempts cost nothing and cover any future third track.
    for index in 0..4 {
        let _ = std::fs::remove_dir_all(work_dir(target, index));
    }
}

/// The name to save under.
fn output_name(
    spec: &DownloadSpec,
    selection: &MediaSelection,
    video: &Plan,
    has_audio: bool,
) -> String {
    if let Some(name) = &spec.filename {
        if !name.trim().is_empty() {
            return hdm_net::http::sanitize_filename(name);
        }
    }
    // A playlist's own filename is nearly always `index`, `master` or
    // `playlist`, which tells nobody anything; the page or the directory above
    // it usually carries the real title.
    let stem = Url::parse(&selection.url)
        .ok()
        .map(|url| {
            let segments: Vec<&str> = url.path.split('/').filter(|s| !s.is_empty()).collect();
            let last = segments.last().copied().unwrap_or("");
            let base = last.rsplit_once('.').map(|(b, _)| b).unwrap_or(last);
            if base.is_empty() || matches!(base, "index" | "master" | "playlist" | "manifest") {
                segments
                    .len()
                    .checked_sub(2)
                    .and_then(|i| segments.get(i))
                    .copied()
                    .unwrap_or("video")
                    .to_string()
            } else {
                base.to_string()
            }
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "video".into());

    // Two tracks always end up muxed into MP4 when ffmpeg is there, and the
    // MP4 name is still the right one to show when it is not.
    let extension = if has_audio || selection.remux {
        "mp4"
    } else {
        &video.container
    };
    hdm_net::http::sanitize_filename(&format!("{stem}.{extension}"))
}

// ------------------------------------------------------------------ plumbing

fn fetch_text(client: &Client, url: &str) -> Result<String, String> {
    let parsed = Url::parse(url).map_err(|e| format!("invalid URL {url}: {e}"))?;
    let mut fetch = client
        .send(Request::get(parsed))
        .map_err(|e| format!("cannot fetch {url}: {e}"))?;
    if fetch.response.status >= 400 {
        fetch.response.shutdown();
        return Err(format!("{url} answered {}", fetch.response.status));
    }
    fetch
        .response
        .read_to_string(MAX_MANIFEST_BYTES)
        .map_err(|e| format!("cannot read {url}: {e}"))
}

fn array<'a>(value: &'a Json, key: &str) -> &'a [Json] {
    value.get(key).and_then(Json::as_arr).unwrap_or(&[])
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
