use hdm_core::category::{extension_of, Categories};
use hdm_core::engine::{DownloadSpec, Status};
use hdm_core::resume::{plan_segments, ResumeState, ResumeVerdict, SegmentRecord};
use hdm_core::store::{DownloadRecord, Settings, Store};
use hdm_core::throttle::Throttle;
use hdm_core::writer::{part_path_for, unique_path, FileWriter};
use hdm_testserver::tls::TempDir;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

// -------------------------------------------------------- segment planning

#[test]
fn segments_cover_the_file_exactly() {
    for (total, count) in [(100u64, 1u8), (100, 3), (1000, 7), (1, 8), (1_048_577, 16)] {
        let segments = plan_segments(total, count);
        assert!(!segments.is_empty(), "no segments for {total}/{count}");
        assert_eq!(segments[0].start, 0, "first segment must start at 0");
        assert_eq!(
            segments.last().unwrap().end,
            total - 1,
            "last segment must end at the final byte for {total}/{count}"
        );
        // Contiguous, with no gaps and no overlap.
        for pair in segments.windows(2) {
            assert_eq!(
                pair[1].start,
                pair[0].end + 1,
                "gap or overlap in {total}/{count}"
            );
        }
        let covered: u64 = segments.iter().map(SegmentRecord::len).sum();
        assert_eq!(covered, total, "coverage for {total}/{count}");
    }
}

#[test]
fn segment_count_never_exceeds_the_byte_count() {
    // Eight connections for a five-byte file would mean empty segments.
    assert_eq!(plan_segments(5, 8).len(), 5);
    assert_eq!(plan_segments(1, 32).len(), 1);
    assert!(
        plan_segments(0, 8).is_empty(),
        "a zero-length file has no segments"
    );
}

#[test]
fn segment_progress_arithmetic() {
    let s = SegmentRecord {
        start: 100,
        end: 199,
        done: 40,
    };
    assert_eq!(s.len(), 100);
    assert_eq!(s.position(), 140);
    assert!(!s.is_complete());
    assert!(SegmentRecord {
        start: 0,
        end: 9,
        done: 10
    }
    .is_complete());
}

// ------------------------------------------------------------ resume safety

fn state_with(etag: Option<&str>, modified: Option<&str>, total: Option<u64>) -> ResumeState {
    ResumeState {
        url: "https://example.com/f.bin".into(),
        total,
        etag: etag.map(str::to_string),
        last_modified: modified.map(str::to_string),
        segments: vec![SegmentRecord {
            start: 0,
            end: 99,
            done: 50,
        }],
        created_at: 0,
    }
}

#[test]
fn resume_is_allowed_when_the_validators_match() {
    let state = state_with(
        Some("\"abc\""),
        Some("Mon, 01 Jan 2024 00:00:00 GMT"),
        Some(100),
    );
    assert_eq!(
        state.validate(
            Some(100),
            Some("\"abc\""),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        ),
        ResumeVerdict::Resume
    );
}

/// Resuming into a file that changed would splice two versions together, and
/// the result would pass every length check while being silently corrupt.
#[test]
fn resume_is_refused_when_the_etag_changed() {
    let state = state_with(Some("\"v1\""), None, Some(100));
    assert!(matches!(
        state.validate(Some(100), Some("\"v2\""), None),
        ResumeVerdict::Restart(_)
    ));
}

#[test]
fn resume_is_refused_when_the_size_changed() {
    let state = state_with(Some("\"v1\""), None, Some(100));
    assert!(matches!(
        state.validate(Some(200), Some("\"v1\""), None),
        ResumeVerdict::Restart(_)
    ));
}

/// A weak ETag only promises semantic equivalence, not identical bytes, so it
/// is not sufficient evidence on its own for a byte-range resume.
#[test]
fn a_weak_etag_needs_the_modification_time_to_agree() {
    let state = state_with(
        Some("W/\"v1\""),
        Some("Mon, 01 Jan 2024 00:00:00 GMT"),
        Some(100),
    );
    assert_eq!(
        state.validate(
            Some(100),
            Some("W/\"v1\""),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        ),
        ResumeVerdict::Resume
    );
    assert!(
        matches!(
            state.validate(
                Some(100),
                Some("W/\"v1\""),
                Some("Tue, 02 Jan 2024 00:00:00 GMT")
            ),
            ResumeVerdict::Restart(_)
        ),
        "a weak ETag with a changed mtime must not resume"
    );
}

#[test]
fn resume_is_refused_when_validators_appear_or_vanish() {
    assert!(matches!(
        state_with(Some("\"v1\""), None, Some(100)).validate(Some(100), None, None),
        ResumeVerdict::Restart(_)
    ));
    assert!(matches!(
        state_with(None, None, Some(100)).validate(Some(100), Some("\"v1\""), None),
        ResumeVerdict::Restart(_)
    ));
}

#[test]
fn resume_falls_back_to_last_modified_when_there_is_no_etag() {
    let state = state_with(None, Some("Mon, 01 Jan 2024 00:00:00 GMT"), Some(100));
    assert_eq!(
        state.validate(Some(100), None, Some("Mon, 01 Jan 2024 00:00:00 GMT")),
        ResumeVerdict::Resume
    );
    assert!(matches!(
        state.validate(Some(100), None, Some("Fri, 05 Jan 2024 00:00:00 GMT")),
        ResumeVerdict::Restart(_)
    ));
}

#[test]
fn resume_state_round_trips_through_json() {
    let dir = TempDir::new("hydra-sidecar").unwrap();
    let path = dir.path().join("f.part.hdm");
    let state = ResumeState {
        url: "https://example.com/a b/فایل.zip".into(),
        total: Some(4_294_967_296),
        etag: Some("\"quoted\\\"etag\"".into()),
        last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
        segments: vec![
            SegmentRecord {
                start: 0,
                end: 999,
                done: 1000,
            },
            SegmentRecord {
                start: 1000,
                end: 4_294_967_295,
                done: 17,
            },
        ],
        created_at: 1_700_000_000,
    };
    state.save(&path).unwrap();
    assert_eq!(ResumeState::load(&path).unwrap(), state);
    assert_eq!(state.downloaded(), 1017);
}

#[test]
fn an_incoherent_sidecar_is_rejected() {
    let dir = TempDir::new("hydra-badstate").unwrap();
    let path = dir.path().join("bad.hdm");

    // done greater than the segment length: something is badly wrong, and
    // trusting it would skip bytes that were never fetched.
    let bad = ResumeState {
        url: "u".into(),
        total: Some(10),
        etag: None,
        last_modified: None,
        segments: vec![SegmentRecord {
            start: 0,
            end: 9,
            done: 999,
        }],
        created_at: 0,
    };
    bad.save(&path).unwrap();
    assert!(ResumeState::load(&path).is_none());

    std::fs::write(&path, "not json at all").unwrap();
    assert!(ResumeState::load(&path).is_none());
}

// ---------------------------------------------------------------- throttle

#[test]
fn an_unlimited_throttle_never_blocks() {
    let throttle = Throttle::unlimited();
    let started = Instant::now();
    for _ in 0..1000 {
        assert_eq!(throttle.take(64 * 1024), 64 * 1024);
    }
    assert!(started.elapsed() < Duration::from_millis(200));
}

#[test]
fn a_limited_throttle_holds_its_rate() {
    // 40 KiB at 20 KiB/s, minus one second of burst credit, is about one second.
    let throttle = Throttle::new(20 * 1024);
    let started = Instant::now();
    let mut taken = 0usize;
    while taken < 40 * 1024 {
        taken += throttle.take(4096);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(700),
        "too fast: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(4), "too slow: {elapsed:?}");
}

#[test]
fn raising_the_rate_wakes_a_waiting_thread() {
    let throttle = Arc::new(Throttle::new(1024));
    let waiter = {
        let throttle = throttle.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut taken = 0usize;
            while taken < 512 * 1024 {
                taken += throttle.take(64 * 1024);
            }
            started.elapsed()
        })
    };
    std::thread::sleep(Duration::from_millis(200));
    throttle.set_rate(0);
    let elapsed = waiter.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(3),
        "the waiter slept through the change: {elapsed:?}"
    );
}

#[test]
fn closing_a_throttle_releases_waiters() {
    let throttle = Arc::new(Throttle::new(1));
    let waiter = {
        let throttle = throttle.clone();
        std::thread::spawn(move || throttle.take(1_000_000))
    };
    std::thread::sleep(Duration::from_millis(100));
    throttle.close();
    assert!(
        waiter.join().is_ok(),
        "close must not leave a thread parked forever"
    );
}

// ------------------------------------------------------------------ writer

#[test]
fn unique_path_avoids_clobbering() {
    let dir = TempDir::new("hydra-unique").unwrap();
    let target = dir.path().join("a.tar.gz");
    assert_eq!(
        unique_path(&target),
        target,
        "an unused name is returned as-is"
    );

    std::fs::write(&target, b"x").unwrap();
    // A multi-part extension must stay intact: "a (1).tar.gz", not "a.tar (1).gz".
    assert_eq!(unique_path(&target).file_name().unwrap(), "a (1).tar.gz");

    std::fs::write(dir.path().join("a (1).tar.gz"), b"x").unwrap();
    assert_eq!(unique_path(&target).file_name().unwrap(), "a (2).tar.gz");
}

#[test]
fn part_paths_are_derived_from_the_target() {
    assert_eq!(
        part_path_for(&PathBuf::from("/tmp/f.bin")),
        PathBuf::from("/tmp/f.bin.part")
    );
}

#[test]
fn the_writer_places_bytes_at_absolute_offsets() {
    let dir = TempDir::new("hydra-writer").unwrap();
    let path = dir.path().join("out.bin.part");
    let writer = FileWriter::create(&path, 10).unwrap();

    // Written out of order, exactly as parallel segments would.
    writer.write_at(5, b"world").unwrap();
    writer.write_at(0, b"hello").unwrap();
    writer.sync().unwrap();
    assert_eq!(writer.len().unwrap(), 10, "the file was preallocated");

    let target = dir.path().join("out.bin");
    let final_path = writer.finalize(&target, false).unwrap();
    assert_eq!(std::fs::read(&final_path).unwrap(), b"helloworld");
    assert!(
        !path.exists(),
        "the .part file should have been renamed away"
    );
}

#[test]
fn discarding_removes_the_partial_file() {
    let dir = TempDir::new("hydra-discard").unwrap();
    let path = dir.path().join("x.part");
    let writer = FileWriter::create(&path, 100).unwrap();
    assert!(path.exists());
    writer.discard().unwrap();
    assert!(!path.exists());
}

// -------------------------------------------------------------- categories

#[test]
fn extensions_are_extracted_and_normalized() {
    assert_eq!(extension_of("a.ZIP").as_deref(), Some("zip"));
    assert_eq!(extension_of("archive.tar.gz").as_deref(), Some("gz"));
    assert_eq!(extension_of("/path/to/movie.mkv").as_deref(), Some("mkv"));
    assert_eq!(extension_of("noextension"), None);
    assert_eq!(extension_of("trailing."), None);
}

#[test]
fn files_are_routed_to_category_folders() {
    let root = PathBuf::from("/downloads");
    let categories = Categories::new(root.clone());

    assert_eq!(
        categories.directory_for("movie.mkv", None),
        root.join("Video")
    );
    assert_eq!(
        categories.directory_for("song.flac", None),
        root.join("Music")
    );
    assert_eq!(
        categories.directory_for("book.pdf", None),
        root.join("Documents")
    );
    assert_eq!(
        categories.directory_for("app.msi", None),
        root.join("Programs")
    );
    assert_eq!(
        categories.directory_for("backup.7z", None),
        root.join("Compressed")
    );
    assert_eq!(
        categories.directory_for("photo.HEIC", None),
        root.join("Images")
    );
    // Unrecognized types stay at the root rather than vanishing into a folder.
    assert_eq!(categories.directory_for("mystery.qqq", None), root);
}

#[test]
fn an_explicit_category_overrides_the_extension() {
    let root = PathBuf::from("/downloads");
    let categories = Categories::new(root.clone());
    assert_eq!(
        categories.directory_for("movie.mkv", Some("documents")),
        root.join("Documents")
    );
}

#[test]
fn categorization_can_be_switched_off() {
    let root = PathBuf::from("/downloads");
    let mut categories = Categories::new(root.clone());
    categories.enabled = false;
    assert_eq!(categories.directory_for("movie.mkv", None), root);
}

#[test]
fn categories_survive_a_json_round_trip() {
    let categories = Categories::new(PathBuf::from("/downloads"));
    let restored = Categories::from_json(&categories.to_json(), &PathBuf::from("/fallback"));
    assert_eq!(restored.root, categories.root);
    assert_eq!(restored.all().len(), categories.all().len());
    assert_eq!(
        restored.directory_for("a.mkv", None),
        categories.directory_for("a.mkv", None)
    );
}

// ------------------------------------------------------------------- store

#[test]
fn the_store_round_trips_downloads_and_settings() {
    let dir = TempDir::new("hydra-store").unwrap();
    let path = dir.path().join("state.json");
    let root = dir.path().to_path_buf();

    let mut store = Store::load(&path, root.clone());
    assert!(store.downloads.is_empty());

    let mut spec = DownloadSpec::new("https://example.com/f.iso", root.clone());
    spec.connections = 16;
    spec.headers = vec![("Referer".into(), "https://example.com/page".into())];
    spec.checksum = Some((hdm_crypto::HashAlgo::Sha256, "ab".repeat(32)));

    let mut record = DownloadRecord::new(spec.clone());
    record.filename = "f.iso".into();
    record.total = Some(1234);
    record.downloaded = 500;
    record.queue = Some("main".into());
    let id = store.insert(record);
    store.settings.speed_limit = 1024 * 500;
    store.settings.language = "fa".into();
    store.save().unwrap();

    let reloaded = Store::load(&path, PathBuf::from("/unused"));
    assert_eq!(reloaded.downloads.len(), 1);
    let restored = reloaded.get(&id).unwrap();
    assert_eq!(restored.spec, spec);
    assert_eq!(restored.total, Some(1234));
    assert_eq!(restored.queue.as_deref(), Some("main"));
    assert_eq!(reloaded.settings.speed_limit, 1024 * 500);
    assert_eq!(reloaded.settings.language, "fa");
}

/// Nothing can be "downloading" the instant the daemon starts, so an entry that
/// was in flight when it stopped must come back as queued and be restartable.
#[test]
fn an_in_flight_download_reloads_as_queued() {
    let dir = TempDir::new("hydra-inflight").unwrap();
    let path = dir.path().join("state.json");
    let root = dir.path().to_path_buf();

    let mut store = Store::load(&path, root.clone());
    let mut record = DownloadRecord::new(DownloadSpec::new("https://example.com/f", root.clone()));
    record.status = Status::Downloading;
    let id = store.insert(record);
    store.save().unwrap();

    let reloaded = Store::load(&path, root);
    assert_eq!(reloaded.get(&id).unwrap().status, Status::Queued);
}

/// Losing the download list because one byte got corrupted would be a terrible
/// failure mode, so the damaged file is preserved for recovery.
#[test]
fn a_corrupt_state_file_is_set_aside_not_deleted() {
    let dir = TempDir::new("hydra-corrupt").unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, "{ truncated").unwrap();

    let store = Store::load(&path, dir.path().to_path_buf());
    assert!(store.downloads.is_empty(), "a fresh store is used");
    assert!(
        dir.path().join("state.json.corrupt").exists(),
        "the damaged file must be kept for recovery"
    );
}

#[test]
fn clearing_completed_keeps_everything_else() {
    let dir = TempDir::new("hydra-clear").unwrap();
    let root = dir.path().to_path_buf();
    let mut store = Store::load(&dir.path().join("s.json"), root.clone());

    for status in [
        Status::Completed,
        Status::Failed,
        Status::Queued,
        Status::Completed,
    ] {
        let mut record = DownloadRecord::new(DownloadSpec::new("https://e.com/f", root.clone()));
        record.status = status;
        store.insert(record);
    }
    assert_eq!(store.clear_completed(), 2);
    assert_eq!(store.downloads.len(), 2);
    assert!(store
        .downloads
        .iter()
        .all(|d| d.status != Status::Completed));
}

#[test]
fn settings_defaults_are_sane() {
    let settings = Settings::new(PathBuf::from("/downloads"));
    assert_eq!(settings.speed_limit, 0, "unlimited by default");
    assert!(settings.connections >= 1 && settings.connections <= 32);
    assert!(settings.max_concurrent >= 1);
}

/// The state file holds site passwords and the API token.
#[cfg(unix)]
#[test]
fn the_state_file_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("hydra-perms").unwrap();
    let path = dir.path().join("state.json");
    Store::load(&path, dir.path().to_path_buf()).save().unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "state file mode is {mode:o}");
}
