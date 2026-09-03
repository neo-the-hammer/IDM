//! The segmented download engine.
//!
//! One `std::thread` per connection. With at most 32 connections per file this
//! is simpler than an async runtime and costs nothing measurable — the threads
//! spend their lives blocked on a socket, and the work-stealing that makes
//! segmentation adaptive is far easier to reason about with real threads.

use crate::probe::{probe, Probe};
use crate::resume::{plan_segments, sidecar_path_for, ResumeState, ResumeVerdict, SegmentRecord};
use crate::throttle::Throttle;
use crate::writer::{part_path_for, FileWriter};
use hdm_crypto::{constant_time_eq, AnyHasher, HashAlgo};
use hdm_net::client::{Client, ClientConfig};
use hdm_net::http::{parse_content_range_span, Request};
use hdm_net::stream::ShutdownHandle;
use hdm_net::url::Url;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Read buffer per connection.
const CHUNK: usize = 64 * 1024;
/// A segment is only worth splitting if both halves stay above this, or the
/// overhead of a fresh connection and TLS handshake outweighs the gain.
const MIN_SPLIT_BYTES: u64 = 1024 * 1024;
/// How often progress is flushed to the sidecar.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);
/// Hard ceiling on connections, matching IDM's own limit.
pub const MAX_CONNECTIONS: u8 = 32;

// Control values.
const CONTROL_RUN: u8 = 0;
const CONTROL_PAUSE: u8 = 1;
const CONTROL_CANCEL: u8 = 2;

/// The lifecycle of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Queued,
    Probing,
    Downloading,
    Verifying,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl Status {
    fn code(self) -> u8 {
        self as u8
    }
    fn from_code(code: u8) -> Status {
        match code {
            0 => Status::Queued,
            1 => Status::Probing,
            2 => Status::Downloading,
            3 => Status::Verifying,
            4 => Status::Paused,
            5 => Status::Completed,
            6 => Status::Failed,
            _ => Status::Cancelled,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Completed | Status::Failed | Status::Cancelled)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Probing => "probing",
            Status::Downloading => "downloading",
            Status::Verifying => "verifying",
            Status::Paused => "paused",
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }
}

/// Everything needed to start a download.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadSpec {
    pub url: String,
    /// Alternate URLs for the same file, tried when the primary keeps failing.
    pub mirrors: Vec<String>,
    pub directory: PathBuf,
    /// Overrides the name the server suggests.
    pub filename: Option<String>,
    pub connections: u8,
    /// Extra request headers — the browser extension replays Referer, cookies
    /// and User-Agent through these.
    pub headers: Vec<(String, String)>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Expected checksum, verified before the file is renamed into place.
    pub checksum: Option<(HashAlgo, String)>,
    pub overwrite: bool,
    pub max_retries: u32,
    pub tls_insecure: bool,
    pub proxy: Option<String>,
    /// Per-download speed cap in bytes per second; 0 means unlimited.
    pub speed_limit: u64,
}

impl DownloadSpec {
    pub fn new(url: impl Into<String>, directory: PathBuf) -> DownloadSpec {
        DownloadSpec {
            url: url.into(),
            mirrors: Vec::new(),
            directory,
            filename: None,
            connections: 8,
            headers: Vec::new(),
            username: None,
            password: None,
            checksum: None,
            overwrite: false,
            max_retries: 5,
            tls_insecure: false,
            proxy: None,
            speed_limit: 0,
        }
    }

    fn all_urls(&self) -> Vec<String> {
        let mut urls = vec![self.url.clone()];
        urls.extend(self.mirrors.iter().cloned());
        urls
    }
}

/// One segment's live state.
///
/// `end` is atomic because work-stealing shrinks it: a worker that finishes
/// early takes the back half of the slowest remaining segment, and the donor
/// notices on its next chunk and stops at the new boundary.
struct LiveSegment {
    start: u64,
    end: AtomicU64,
    done: AtomicU64,
    /// Set while a worker owns this segment.
    claimed: AtomicBool,
}

impl LiveSegment {
    fn position(&self) -> u64 {
        self.start + self.done.load(Ordering::Acquire)
    }
    fn end(&self) -> u64 {
        self.end.load(Ordering::Acquire)
    }
    /// `u64::MAX` when the length is unknown, which is the sentinel `end`
    /// carries for a response with no Content-Length.
    fn len(&self) -> u64 {
        self.end().saturating_sub(self.start).saturating_add(1)
    }
    fn remaining(&self) -> u64 {
        let position = self.position();
        let end = self.end();
        if position > end {
            0
        } else {
            end - position + 1
        }
    }
    fn is_complete(&self) -> bool {
        self.position() > self.end()
    }
    /// Progress, clamped so a shrunken segment never reports more than it owns.
    fn accounted(&self) -> u64 {
        self.done.load(Ordering::Acquire).min(self.len())
    }
    fn record(&self) -> SegmentRecord {
        SegmentRecord {
            start: self.start,
            end: self.end(),
            done: self.done.load(Ordering::Acquire),
        }
    }
}

/// Live state, shared with whoever is watching the download.
pub struct Shared {
    status: AtomicU8,
    control: AtomicU8,
    downloaded: AtomicU64,
    /// Zero means "not yet known".
    total: AtomicU64,
    /// Bytes per second, smoothed.
    speed: AtomicU64,
    segments: Mutex<Vec<Arc<LiveSegment>>>,
    /// Live connections, so pause and cancel can interrupt a blocking read
    /// instead of waiting out a socket timeout.
    connections: Mutex<Vec<Arc<ShutdownHandle>>>,
    error: Mutex<Option<String>>,
    output_path: Mutex<Option<PathBuf>>,
    filename: Mutex<String>,
}

impl Default for Shared {
    fn default() -> Self {
        Shared::new()
    }
}

impl Shared {
    pub fn new() -> Shared {
        Shared {
            status: AtomicU8::new(Status::Queued.code()),
            control: AtomicU8::new(CONTROL_RUN),
            downloaded: AtomicU64::new(0),
            total: AtomicU64::new(0),
            speed: AtomicU64::new(0),
            segments: Mutex::new(Vec::new()),
            connections: Mutex::new(Vec::new()),
            error: Mutex::new(None),
            output_path: Mutex::new(None),
            filename: Mutex::new(String::new()),
        }
    }

    pub fn status(&self) -> Status {
        Status::from_code(self.status.load(Ordering::Acquire))
    }

    fn set_status(&self, status: Status) {
        self.status.store(status.code(), Ordering::Release);
    }

    pub fn downloaded(&self) -> u64 {
        self.downloaded.load(Ordering::Acquire)
    }

    pub fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Acquire) {
            0 => None,
            n => Some(n),
        }
    }

    pub fn speed(&self) -> u64 {
        self.speed.load(Ordering::Acquire)
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }

    pub fn output_path(&self) -> Option<PathBuf> {
        self.output_path.lock().unwrap().clone()
    }

    pub fn filename(&self) -> String {
        self.filename.lock().unwrap().clone()
    }

    /// Per-segment progress, for the UI's segment bars.
    pub fn segment_progress(&self) -> Vec<(u64, u64, u64)> {
        self.segments
            .lock()
            .unwrap()
            .iter()
            .map(|s| (s.start, s.end(), s.accounted()))
            .collect()
    }

    /// Fraction complete, when the size is known.
    pub fn fraction(&self) -> Option<f64> {
        let total = self.total()?;
        if total == 0 {
            return Some(1.0);
        }
        Some((self.downloaded() as f64 / total as f64).clamp(0.0, 1.0))
    }

    /// Seconds remaining at the current rate.
    pub fn eta_seconds(&self) -> Option<u64> {
        let total = self.total()?;
        let speed = self.speed();
        if speed == 0 {
            return None;
        }
        Some(total.saturating_sub(self.downloaded()) / speed)
    }

    /// Asks the download to stop and keep its progress.
    pub fn pause(&self) {
        self.control.store(CONTROL_PAUSE, Ordering::Release);
        self.interrupt_connections();
    }

    /// Asks the download to stop and discard its partial file.
    pub fn cancel(&self) {
        self.control.store(CONTROL_CANCEL, Ordering::Release);
        self.interrupt_connections();
    }

    fn control(&self) -> u8 {
        self.control.load(Ordering::Acquire)
    }

    fn should_stop(&self) -> bool {
        self.control() != CONTROL_RUN
    }

    /// Closes every open socket, so threads blocked in `read` return at once.
    fn interrupt_connections(&self) {
        for handle in self.connections.lock().unwrap().iter() {
            handle.shutdown();
        }
    }

    fn register_connection(&self, handle: Arc<ShutdownHandle>) {
        self.connections.lock().unwrap().push(handle);
    }

    fn unregister_connection(&self, handle: &Arc<ShutdownHandle>) {
        self.connections
            .lock()
            .unwrap()
            .retain(|h| !Arc::ptr_eq(h, handle));
    }

    fn recompute_downloaded(&self) {
        let total: u64 = self
            .segments
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.accounted())
            .sum();
        self.downloaded.store(total, Ordering::Release);
    }
}

/// How a download ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed { path: PathBuf, bytes: u64 },
    Paused,
    Cancelled,
}

/// Runs a download to completion, blocking the calling thread.
pub fn run(
    spec: &DownloadSpec,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
) -> io::Result<Outcome> {
    match run_inner(spec, shared, throttle) {
        Ok(outcome) => {
            match &outcome {
                Outcome::Completed { path, .. } => {
                    *shared.output_path.lock().unwrap() = Some(path.clone());
                    shared.set_status(Status::Completed);
                }
                Outcome::Paused => shared.set_status(Status::Paused),
                Outcome::Cancelled => shared.set_status(Status::Cancelled),
            }
            Ok(outcome)
        }
        Err(e) => {
            *shared.error.lock().unwrap() = Some(e.to_string());
            shared.set_status(Status::Failed);
            Err(e)
        }
    }
}

fn run_inner(
    spec: &DownloadSpec,
    shared: &Arc<Shared>,
    throttle: &Arc<Throttle>,
) -> io::Result<Outcome> {
    shared.set_status(Status::Probing);

    let client = Arc::new(build_client(spec)?);
    let url = Url::parse(&spec.url)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    let info = probe(&client, &url)?;

    if info.status >= 400 {
        return Err(io::Error::other(format!(
            "the server answered {} for {}",
            info.status, spec.url
        )));
    }

    let filename = spec
        .filename
        .clone()
        .map(|n| hdm_net::http::sanitize_filename(&n))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| info.filename.clone());
    *shared.filename.lock().unwrap() = filename.clone();

    let target = spec.directory.join(&filename);
    let part = part_path_for(&target);
    let sidecar = sidecar_path_for(&part);

    if let Some(total) = info.total {
        shared.total.store(total, Ordering::Release);
    }

    // Decide between resuming and starting over.
    // A zero-length file has nothing to transfer: create it and finish.
    if info.total == Some(0) {
        ResumeState::remove(&sidecar);
        let writer = FileWriter::create(&part, 0)?;
        return finish(spec, shared, writer, &target, &sidecar, &info);
    }

    let existing = ResumeState::load(&sidecar).filter(|_| part.exists());
    let segments = match existing {
        Some(state) => {
            match state.validate(
                info.total,
                info.etag.as_deref(),
                info.last_modified.as_deref(),
            ) {
                ResumeVerdict::Resume if info.supports_ranges => state.segments,
                // Without range support the server can only send the file from
                // the beginning, so a partial file is of no use.
                ResumeVerdict::Resume => {
                    ResumeState::remove(&sidecar);
                    let _ = std::fs::remove_file(&part);
                    fresh_segments(&info, spec)
                }
                ResumeVerdict::Restart(reason) => {
                    // Starting over is the safe choice: continuing would blend
                    // two different versions of the file into one.
                    ResumeState::remove(&sidecar);
                    let _ = std::fs::remove_file(&part);
                    let _ = reason;
                    fresh_segments(&info, spec)
                }
            }
        }
        None => fresh_segments(&info, spec),
    };

    let live: Vec<Arc<LiveSegment>> = segments
        .iter()
        .map(|s| {
            Arc::new(LiveSegment {
                start: s.start,
                end: AtomicU64::new(s.end),
                done: AtomicU64::new(s.done),
                claimed: AtomicBool::new(false),
            })
        })
        .collect();
    *shared.segments.lock().unwrap() = live.clone();
    shared.recompute_downloaded();

    let writer = Arc::new(FileWriter::create(&part, info.total.unwrap_or(0))?);

    // Already finished, e.g. the process died between the last byte and the
    // rename. Nothing to transfer.
    if live.iter().all(|s| s.is_complete()) && info.total.is_some() {
        let writer = unwrap_writer(writer)?;
        return finish(spec, shared, writer, &target, &sidecar, &info);
    }

    shared.set_status(Status::Downloading);
    let worker_count = live
        .len()
        .min(effective_connections(spec, &info) as usize)
        .max(1);
    let urls = spec.all_urls();

    let context = Arc::new(WorkerContext {
        client: client.clone(),
        urls,
        primary: info.final_url.clone(),
        shared: shared.clone(),
        writer: writer.clone(),
        throttle: throttle.clone(),
        max_retries: spec.max_retries,
        segmented: info.can_segment(),
        first_error: Mutex::new(None),
    });

    let active_workers = Arc::new(AtomicUsize::new(worker_count));
    std::thread::scope(|scope| {
        for index in 0..worker_count {
            let context = context.clone();
            let live = live.clone();
            let active = active_workers.clone();
            scope.spawn(move || {
                worker(index, &context, &live);
                active.fetch_sub(1, Ordering::AcqRel);
            });
        }
        // The calling thread becomes the monitor: it publishes progress and
        // checkpoints the sidecar while the workers transfer.
        monitor(shared, &active_workers, &sidecar, &info);
    });

    let first_error = context.first_error.lock().unwrap().take();
    // Every worker has exited, so releasing this reference leaves the engine
    // as the sole owner of the output file.
    drop(context);
    let writer = unwrap_writer(writer)?;

    if let Some(error) = first_error {
        // Persist whatever progress was made, so a retry does not start over.
        checkpoint(&sidecar, &info, shared);
        let _ = writer.sync();
        return Err(error);
    }

    match shared.control() {
        CONTROL_CANCEL => {
            let _ = writer.discard();
            ResumeState::remove(&sidecar);
            Ok(Outcome::Cancelled)
        }
        CONTROL_PAUSE => {
            checkpoint(&sidecar, &info, shared);
            let _ = writer.sync();
            Ok(Outcome::Paused)
        }
        _ => {
            checkpoint(&sidecar, &info, shared);
            finish(spec, shared, writer, &target, &sidecar, &info)
        }
    }
}

/// Takes sole ownership of the output file once the workers have finished.
fn unwrap_writer(writer: Arc<FileWriter>) -> io::Result<FileWriter> {
    Arc::try_unwrap(writer)
        .map_err(|_| io::Error::other("internal error: the output file is still in use"))
}

/// Segments for a download starting from nothing.
fn fresh_segments(info: &Probe, spec: &DownloadSpec) -> Vec<SegmentRecord> {
    match (info.can_segment(), info.total) {
        (true, Some(total)) => plan_segments(total, effective_connections(spec, info)),
        // No range support, or no known size: one connection, read to the end.
        (_, Some(total)) => vec![SegmentRecord {
            start: 0,
            end: total.saturating_sub(1),
            done: 0,
        }],
        (_, None) => vec![SegmentRecord {
            start: 0,
            end: u64::MAX,
            done: 0,
        }],
    }
}

fn effective_connections(spec: &DownloadSpec, info: &Probe) -> u8 {
    if !info.can_segment() {
        return 1;
    }
    spec.connections.clamp(1, MAX_CONNECTIONS)
}

fn build_client(spec: &DownloadSpec) -> io::Result<Client> {
    let mut config = ClientConfig::new();
    config.tls_insecure = spec.tls_insecure;
    #[cfg(unix)]
    {
        config.tls.insecure = spec.tls_insecure;
    }
    for (name, value) in &spec.headers {
        if hdm_net::headers::is_safe_header_value(value) {
            config.extra_headers.set(name.clone(), value.clone());
        }
    }
    if let Some(user) = &spec.username {
        config.credentials = Some(hdm_net::auth::Credentials {
            username: user.clone(),
            password: spec.password.clone().unwrap_or_default(),
        });
    }
    if let Some(spec_proxy) = &spec.proxy {
        config.proxy = Some(
            hdm_net::proxy::Proxy::parse(spec_proxy)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        );
    }
    Client::new(config)
}

struct WorkerContext {
    client: Arc<Client>,
    urls: Vec<String>,
    primary: Url,
    shared: Arc<Shared>,
    writer: Arc<FileWriter>,
    throttle: Arc<Throttle>,
    max_retries: u32,
    segmented: bool,
    /// The first hard failure, which becomes the download's error.
    first_error: Mutex<Option<io::Error>>,
}

impl WorkerContext {
    /// Cycles through mirrors as attempts fail, so a dead mirror is routed
    /// around rather than retried forever.
    fn url_for_attempt(&self, attempt: u32) -> Url {
        if attempt == 0 || self.urls.len() <= 1 {
            return self.primary.clone();
        }
        let index = (attempt as usize) % self.urls.len();
        Url::parse(&self.urls[index]).unwrap_or_else(|_| self.primary.clone())
    }

    fn report(&self, error: io::Error) {
        let mut slot = self.first_error.lock().unwrap();
        if slot.is_none() {
            *slot = Some(error);
        }
        // Stop the other workers; the download has failed.
        self.shared.control.store(CONTROL_CANCEL, Ordering::Release);
        self.shared.interrupt_connections();
    }
}

/// One connection: download its segment, then steal work from the slowest.
fn worker(index: usize, context: &WorkerContext, segments: &[Arc<LiveSegment>]) {
    let mut current = match segments.get(index) {
        Some(segment) if !segment.claimed.swap(true, Ordering::AcqRel) => segment.clone(),
        // More workers than segments at startup: go straight to stealing.
        _ => match steal(context) {
            Some(segment) => segment,
            None => return,
        },
    };

    loop {
        if context.shared.should_stop() {
            return;
        }
        if let Err(e) = transfer(context, &current) {
            if context.shared.should_stop() {
                return;
            }
            context.report(e);
            return;
        }
        match steal(context) {
            Some(next) => current = next,
            None => return,
        }
    }
}

/// Takes over the back half of whichever segment has the most left to do.
///
/// This is what makes segmentation adaptive: a connection that finishes early
/// because its part of the file came from a fast mirror does not sit idle while
/// one slow connection holds up the whole download.
fn steal(context: &WorkerContext) -> Option<Arc<LiveSegment>> {
    if !context.segmented || context.shared.should_stop() {
        return None;
    }
    let mut table = context.shared.segments.lock().unwrap();

    // An unclaimed segment is free to take outright.
    if let Some(free) = table
        .iter()
        .find(|s| !s.is_complete() && !s.claimed.swap(true, Ordering::AcqRel))
    {
        return Some(free.clone());
    }

    let donor = table
        .iter()
        .filter(|s| !s.is_complete())
        .max_by_key(|s| s.remaining())?
        .clone();

    let remaining = donor.remaining();
    if remaining < MIN_SPLIT_BYTES * 2 {
        return None;
    }

    // Split ahead of the donor's *current* position, never behind it, so its
    // recorded progress stays valid and the progress bar cannot go backwards.
    // The donor may advance a chunk or two while this lock is held; with a
    // half-megabyte margin that cannot reach the boundary, and even if it did
    // the overlap is re-fetched identical bytes, not corruption.
    let position = donor.position();
    let old_end = donor.end();
    let mid = position + remaining / 2;
    if mid <= position || mid > old_end {
        return None;
    }

    donor.end.store(mid - 1, Ordering::Release);
    let taken = Arc::new(LiveSegment {
        start: mid,
        end: AtomicU64::new(old_end),
        done: AtomicU64::new(0),
        claimed: AtomicBool::new(true),
    });
    table.push(taken.clone());
    Some(taken)
}

/// Downloads one segment, retrying with backoff.
fn transfer(context: &WorkerContext, segment: &Arc<LiveSegment>) -> io::Result<()> {
    let mut attempt = 0u32;
    loop {
        if context.shared.should_stop() || segment.is_complete() {
            return Ok(());
        }
        match transfer_once(context, segment) {
            Ok(()) => return Ok(()),
            Err(e) => {
                if context.shared.should_stop() {
                    return Ok(());
                }
                attempt += 1;
                if attempt > context.max_retries {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("giving up after {attempt} attempts: {e}"),
                    ));
                }
                // Exponential backoff, capped, so a flapping server is not
                // hammered but a transient blip costs almost nothing.
                let wait = Duration::from_millis((250u64 << attempt.min(6)).min(15_000));
                std::thread::sleep(wait);
            }
        }
    }
}

fn transfer_once(context: &WorkerContext, segment: &Arc<LiveSegment>) -> io::Result<()> {
    let attempt_url = context.url_for_attempt(0);
    let position = segment.position();
    let end = segment.end();
    if position > end {
        return Ok(());
    }

    let mut request = Request::get(attempt_url);
    if context.segmented {
        let bounded_end = (end != u64::MAX).then_some(end);
        request = request.with_range(position, bounded_end);
    } else if position > 0 {
        // Not segmented but resuming: ask anyway. If the server ignores it we
        // detect the 200 below and restart from zero.
        request = request.with_range(position, None);
    }

    let mut fetch = context.client.send(request)?;
    let response = &mut fetch.response;

    // Where the server actually started sending from.
    let mut write_offset = position;
    if context.segmented || position > 0 {
        if response.is_partial() {
            // A server that answers a range request with the wrong range would
            // silently corrupt the file, so the offset is verified rather than
            // assumed.
            let Some(range) = response.headers.get("Content-Range") else {
                return Err(io::Error::other("a 206 response with no Content-Range"));
            };
            let Some((got_start, _)) = parse_content_range_span(range) else {
                return Err(io::Error::other(format!(
                    "unparseable Content-Range: {range}"
                )));
            };
            if got_start != position {
                return Err(io::Error::other(format!(
                    "asked for byte {position} but the server sent from {got_start}"
                )));
            }
        } else if response.status == 200 {
            if context.segmented {
                return Err(io::Error::other(
                    "the server stopped honouring range requests mid-download",
                ));
            }
            // Whole file from the start: rewind our accounting to match.
            write_offset = 0;
            segment.done.store(0, Ordering::Release);
            context.shared.recompute_downloaded();
        } else {
            return Err(io::Error::other(format!(
                "unexpected status {} for a range request",
                response.status
            )));
        }
    } else if response.status >= 300 {
        return Err(io::Error::other(format!(
            "the server answered {}",
            response.status
        )));
    }

    // Publish the socket so pause and cancel can interrupt a blocked read.
    let handle = Arc::new(response.shutdown_handle()?);
    context.shared.register_connection(handle.clone());
    let result = stream_body(context, segment, response, write_offset);
    context.shared.unregister_connection(&handle);
    result
}

fn stream_body(
    context: &WorkerContext,
    segment: &Arc<LiveSegment>,
    response: &mut hdm_net::http::Response,
    mut offset: u64,
) -> io::Result<()> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        if context.shared.should_stop() {
            return Ok(());
        }
        // The end may have moved if another worker stole the tail.
        let end = segment.end();
        if offset > end {
            return Ok(());
        }
        let allowed = if end == u64::MAX {
            buffer.len()
        } else {
            buffer.len().min((end - offset + 1) as usize)
        };
        // Wait for bandwidth budget before reading, so the limit is enforced at
        // the socket rather than after the bytes have already arrived.
        let granted = context.throttle.take(allowed);
        if granted == 0 {
            continue;
        }

        let read = match response.body.read(&mut buffer[..granted]) {
            Ok(0) => {
                // Clean end of stream. For a bounded segment that has not
                // reached its end, this is a truncated transfer worth retrying.
                if end != u64::MAX && offset <= end {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!("the connection closed with {} bytes left", end - offset + 1),
                    ));
                }
                return Ok(());
            }
            Ok(n) => n,
            Err(e) => {
                if context.shared.should_stop() {
                    // The socket was closed by pause or cancel, not by a fault.
                    return Ok(());
                }
                return Err(e);
            }
        };

        context.writer.write_at(offset, &buffer[..read])?;
        offset += read as u64;
        segment
            .done
            .store(offset - segment.start, Ordering::Release);
        context.shared.recompute_downloaded();
    }
}

/// Publishes progress and checkpoints the sidecar until the workers finish.
fn monitor(
    shared: &Arc<Shared>,
    active_workers: &AtomicUsize,
    sidecar: &std::path::Path,
    info: &Probe,
) {
    let mut last_checkpoint = Instant::now();
    let mut last_sample = (Instant::now(), shared.downloaded());
    // A smoothed rate: instantaneous speed on a segmented download swings far
    // too much to be readable.
    let mut smoothed = 0f64;

    loop {
        std::thread::sleep(Duration::from_millis(250));

        let now = Instant::now();
        let downloaded = shared.downloaded();
        let elapsed = now.duration_since(last_sample.0).as_secs_f64();
        if elapsed > 0.0 {
            let instant = (downloaded.saturating_sub(last_sample.1)) as f64 / elapsed;
            smoothed = if smoothed == 0.0 {
                instant
            } else {
                smoothed * 0.7 + instant * 0.3
            };
            shared.speed.store(smoothed as u64, Ordering::Release);
            last_sample = (now, downloaded);
        }

        if now.duration_since(last_checkpoint) >= CHECKPOINT_INTERVAL {
            checkpoint(sidecar, info, shared);
            last_checkpoint = now;
        }

        // Exit when the workers have exited, not when the segments this
        // function was handed look complete: work-stealing adds segments the
        // caller never saw, and a download of unknown length has no end offset
        // to compare against at all.
        if active_workers.load(Ordering::Acquire) == 0 {
            return;
        }
    }
}

fn checkpoint(sidecar: &std::path::Path, info: &Probe, shared: &Arc<Shared>) {
    // Read the live table rather than a snapshot, so segments produced by
    // work-stealing are persisted too.
    let segments: Vec<SegmentRecord> = shared
        .segments
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.record())
        .collect();
    let state = ResumeState {
        url: info.final_url.to_string_safe(),
        total: info.total,
        etag: info.etag.clone(),
        last_modified: info.last_modified.clone(),
        segments,
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let _ = state.save(sidecar);
    shared.recompute_downloaded();
}

/// Verifies and renames a completed download.
fn finish(
    spec: &DownloadSpec,
    shared: &Arc<Shared>,
    writer: FileWriter,
    target: &std::path::Path,
    sidecar: &std::path::Path,
    info: &Probe,
) -> io::Result<Outcome> {
    let downloaded = shared.downloaded();

    // A size mismatch means something went wrong that the per-segment checks
    // missed. Refusing here is the last chance to catch it before the file is
    // presented to the user as complete.
    if let Some(expected) = info.total {
        if downloaded != expected {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("expected {expected} bytes but wrote {downloaded}"),
            ));
        }
    } else {
        // Size was unknown, so the file was never preallocated to the right
        // length; trim any slack.
        writer.truncate(downloaded)?;
    }

    if let Some((algo, expected)) = &spec.checksum {
        shared.set_status(Status::Verifying);
        let actual = hash_file(&writer, downloaded, *algo)?;
        if !constant_time_eq(
            actual.as_bytes(),
            expected.trim().to_ascii_lowercase().as_bytes(),
        ) {
            return Err(io::Error::other(format!(
                "checksum mismatch: expected {} {}, got {actual}",
                algo.name(),
                expected.trim().to_ascii_lowercase()
            )));
        }
    }

    writer.sync()?;
    let path = writer.finalize(target, spec.overwrite)?;
    ResumeState::remove(sidecar);
    Ok(Outcome::Completed {
        path,
        bytes: downloaded,
    })
}

/// Hashes the finished file straight off disk.
fn hash_file(writer: &FileWriter, len: u64, algo: HashAlgo) -> io::Result<String> {
    let mut hasher = AnyHasher::new(algo);
    let mut buffer = vec![0u8; 256 * 1024];
    let mut offset = 0u64;
    while offset < len {
        let want = buffer.len().min((len - offset) as usize);
        let read = writer.read_at(offset, &mut buffer[..want])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the file shrank while it was being verified",
            ));
        }
        hasher.update(&buffer[..read]);
        offset += read as u64;
    }
    Ok(hasher.hex())
}
