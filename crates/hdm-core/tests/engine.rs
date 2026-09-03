//! End-to-end tests of the download engine against a controllable origin.

use hdm_core::engine::{run, DownloadSpec, Outcome, Shared, Status};
use hdm_core::resume::{sidecar_path_for, ResumeState, SegmentRecord};
use hdm_core::throttle::Throttle;
use hdm_core::writer::part_path_for;
use hdm_crypto::{Digest, HashAlgo, Sha256};
use hdm_testserver::{test_data, tls::TempDir, ServerBuilder};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn spec(url: &str, dir: &std::path::Path, connections: u8) -> DownloadSpec {
    let mut s = DownloadSpec::new(url, dir.to_path_buf());
    s.connections = connections;
    s
}

fn download(spec: &DownloadSpec) -> (std::io::Result<Outcome>, Arc<Shared>) {
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    let outcome = run(spec, &shared, &throttle);
    (outcome, shared)
}

// ------------------------------------------------------- segmentation is exact

/// The central promise of a segmented downloader: however many connections it
/// uses, the bytes on disk must be identical to a single-stream fetch.
#[test]
fn every_connection_count_produces_identical_bytes() {
    // A size that divides unevenly, so the last segment is a different length.
    let data = test_data(700_003);
    let expected = Sha256::hex_digest(&data);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    for connections in [1u8, 2, 3, 4, 8, 16] {
        let dir = TempDir::new(&format!("hydra-seg-{connections}")).unwrap();
        let (outcome, shared) = download(&spec(&server.url("/f.bin"), dir.path(), connections));

        let Ok(Outcome::Completed { path, bytes }) = outcome else {
            panic!(
                "{connections} connections: {outcome:?} / {:?}",
                shared.error()
            );
        };
        assert_eq!(
            bytes,
            data.len() as u64,
            "byte count with {connections} connections"
        );
        let written = std::fs::read(&path).unwrap();
        assert_eq!(
            Sha256::hex_digest(&written),
            expected,
            "content differs with {connections} connections"
        );
        assert_eq!(shared.status(), Status::Completed);
    }
}

#[test]
fn downloads_a_file_of_a_single_byte() {
    let dir = TempDir::new("hydra-tiny").unwrap();
    let server = ServerBuilder::new().file("/one.bin", vec![0x42]).start();
    // More connections than bytes must not produce empty or duplicated segments.
    let (outcome, _) = download(&spec(&server.url("/one.bin"), dir.path(), 8));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(std::fs::read(&path).unwrap(), vec![0x42]);
}

#[test]
fn downloads_an_empty_file() {
    let dir = TempDir::new("hydra-empty").unwrap();
    let server = ServerBuilder::new().file("/zero.bin", Vec::new()).start();
    let (outcome, _) = download(&spec(&server.url("/zero.bin"), dir.path(), 4));
    let Ok(Outcome::Completed { path, bytes }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(bytes, 0);
    assert!(path.exists());
    assert_eq!(std::fs::read(&path).unwrap().len(), 0);
}

/// A server that will not serve ranges must still produce a correct file, on a
/// single connection.
#[test]
fn falls_back_to_one_connection_without_range_support() {
    let data = test_data(120_000);
    let dir = TempDir::new("hydra-noranges").unwrap();
    let server = ServerBuilder::new()
        .file_with("/nr.bin", data.clone(), |f| f.accept_ranges = false)
        .start();

    let (outcome, _) = download(&spec(&server.url("/nr.bin"), dir.path(), 8));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(std::fs::read(&path).unwrap(), data);

    // Whatever it asked for, it must not have written eight overlapping copies.
    let ranged = server
        .requests()
        .iter()
        .filter(|r| r.header("Range").is_some())
        .count();
    assert!(
        ranged <= 1,
        "sent {ranged} range requests to a server that refuses them"
    );
}

/// A chunked response has no Content-Length, so the file cannot be preallocated
/// and must be trimmed to its true size at the end.
#[test]
fn handles_a_response_of_unknown_length() {
    let data = test_data(30_000);
    let dir = TempDir::new("hydra-chunked").unwrap();
    let server = ServerBuilder::new().chunked("/c.bin", data.clone()).start();
    let (outcome, _) = download(&spec(&server.url("/c.bin"), dir.path(), 4));
    let Ok(Outcome::Completed { path, bytes }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(bytes, data.len() as u64);
    assert_eq!(std::fs::read(&path).unwrap(), data);
}

// -------------------------------------------------------------------- naming

#[test]
fn uses_the_content_disposition_filename() {
    let dir = TempDir::new("hydra-name").unwrap();
    let server = ServerBuilder::new()
        .file_with("/download.php", test_data(100), |f| {
            f.content_disposition = Some("attachment; filename=\"ubuntu.iso\"".into());
        })
        .start();
    let (outcome, _) = download(&spec(&server.url("/download.php"), dir.path(), 2));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(path.file_name().unwrap(), "ubuntu.iso");
}

/// An existing file must never be silently replaced.
#[test]
fn avoids_overwriting_an_existing_file() {
    let dir = TempDir::new("hydra-clash").unwrap();
    std::fs::write(dir.path().join("f.bin"), b"do not lose me").unwrap();

    let data = test_data(500);
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();
    let (outcome, _) = download(&spec(&server.url("/f.bin"), dir.path(), 2));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };

    assert_eq!(path.file_name().unwrap(), "f (1).bin");
    assert_eq!(
        std::fs::read(dir.path().join("f.bin")).unwrap(),
        b"do not lose me"
    );
    assert_eq!(std::fs::read(&path).unwrap(), data);
}

/// A malicious filename must not escape the download directory.
#[test]
fn a_traversing_filename_cannot_escape_the_directory() {
    let dir = TempDir::new("hydra-escape").unwrap();
    let server = ServerBuilder::new()
        .file_with("/get", test_data(50), |f| {
            f.content_disposition = Some("attachment; filename=\"../../pwned.txt\"".into());
        })
        .start();
    let (outcome, _) = download(&spec(&server.url("/get"), dir.path(), 1));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(path.file_name().unwrap(), "pwned.txt");
    assert_eq!(
        path.parent().unwrap(),
        dir.path(),
        "the file escaped its directory"
    );
}

// -------------------------------------------------------------------- retries

/// A connection dropped mid-transfer must be retried, not accepted as a short
/// file.
#[test]
fn retries_a_dropped_connection_and_still_matches() {
    let data = test_data(200_000);
    let dir = TempDir::new("hydra-retry").unwrap();
    // Cut the first four responses after 8 KiB; later attempts succeed.
    let server = ServerBuilder::new()
        .file_with("/flaky.bin", data.clone(), |f| f.cut_for_next(8192, 4))
        .start();

    let mut s = spec(&server.url("/flaky.bin"), dir.path(), 4);
    s.max_retries = 10;
    let (outcome, _) = download(&s);

    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(
        std::fs::read(&path).unwrap(),
        data,
        "retried download differs"
    );
}

/// When retries run out the download must fail loudly and leave no file that
/// could be mistaken for complete.
#[test]
fn gives_up_after_the_retry_budget_and_leaves_no_finished_file() {
    let data = test_data(200_000);
    let dir = TempDir::new("hydra-giveup").unwrap();
    let server = ServerBuilder::new()
        .file_with("/broken.bin", data, |f| f.cut_after = Some(4096))
        .start();

    let mut s = spec(&server.url("/broken.bin"), dir.path(), 2);
    s.max_retries = 1;
    let (outcome, shared) = download(&s);

    assert!(outcome.is_err(), "a permanently broken transfer must fail");
    assert_eq!(shared.status(), Status::Failed);
    assert!(shared.error().is_some());
    assert!(
        !dir.path().join("broken.bin").exists(),
        "a partial file was presented as done"
    );
    // The partial data and its sidecar remain, so a later retry can resume.
    assert!(dir.path().join("broken.bin.part").exists());
}

// --------------------------------------------------------------------- resume

/// The headline feature: a download that stops partway continues rather than
/// starting over.
#[test]
fn resumes_from_a_partial_download() {
    let data = test_data(400_000);
    let dir = TempDir::new("hydra-resume").unwrap();
    // Slow enough that the first attempt is still running when we pause it.
    let server = ServerBuilder::new()
        .file_with("/f.bin", data.clone(), |f| {
            f.delay_per_chunk = Some(Duration::from_millis(12));
        })
        .start();

    let s = spec(&server.url("/f.bin"), dir.path(), 4);

    // First attempt: pause once some bytes have landed.
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    let paused = {
        let spec = s.clone();
        let shared = shared.clone();
        std::thread::spawn(move || run(&spec, &shared, &throttle))
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while shared.downloaded() < 40_000 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let progress_before_pause = shared.downloaded();
    assert!(progress_before_pause > 0, "the download never started");
    shared.pause();
    let outcome = paused.join().unwrap();
    assert!(matches!(outcome, Ok(Outcome::Paused)), "got {outcome:?}");

    // The sidecar must record real progress for the resume to build on.
    let part = part_path_for(&dir.path().join("f.bin"));
    let state = ResumeState::load(&sidecar_path_for(&part)).expect("no sidecar was written");
    assert!(state.downloaded() > 0, "the sidecar recorded no progress");
    assert_eq!(state.total, Some(data.len() as u64));

    // Second attempt: a fresh Shared, exactly as a restarted daemon would.
    let (outcome, shared2) = download(&s);
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?} / {:?}", shared2.error())
    };
    assert_eq!(
        std::fs::read(&path).unwrap(),
        data,
        "the resumed file is wrong"
    );

    // And it genuinely resumed: the second run asked for a non-zero offset.
    let resumed_from_offset = server.requests().iter().any(|r| {
        r.header("Range")
            .map(|v| v != "bytes=0-" && !v.starts_with("bytes=0-0"))
            .unwrap_or(false)
    });
    assert!(
        resumed_from_offset,
        "the second attempt restarted from the beginning"
    );
}

/// If the remote file changed, resuming would splice two different versions
/// together. The engine must start over instead.
#[test]
fn refuses_to_resume_into_a_changed_file() {
    let data = test_data(100_000);
    let dir = TempDir::new("hydra-changed").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    // Fabricate a partial download of a *different* file: the first half is
    // zeros, and the sidecar claims it is complete and valid under an old ETag.
    let target = dir.path().join("f.bin");
    let part = part_path_for(&target);
    let half = (data.len() / 2) as u64;
    let mut stale = vec![0u8; data.len()];
    stale[..half as usize].fill(0xAB);
    std::fs::write(&part, &stale).unwrap();

    ResumeState {
        url: server.url("/f.bin"),
        total: Some(data.len() as u64),
        etag: Some("\"an-older-version\"".into()),
        last_modified: None,
        segments: vec![
            SegmentRecord {
                start: 0,
                end: half - 1,
                done: half,
            },
            SegmentRecord {
                start: half,
                end: data.len() as u64 - 1,
                done: 0,
            },
        ],
        created_at: 0,
    }
    .save(&sidecar_path_for(&part))
    .unwrap();

    let (outcome, shared) = download(&spec(&server.url("/f.bin"), dir.path(), 4));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?} / {:?}", shared.error())
    };

    // Had it trusted the stale sidecar, the first half would still be 0xAB.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        data,
        "the engine resumed into a file that had changed underneath it"
    );
}

#[test]
fn a_corrupt_sidecar_is_ignored_rather_than_fatal() {
    let data = test_data(20_000);
    let dir = TempDir::new("hydra-badsidecar").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let part = part_path_for(&dir.path().join("f.bin"));
    std::fs::write(&part, vec![0u8; data.len()]).unwrap();
    std::fs::write(sidecar_path_for(&part), b"{ this is not json").unwrap();

    let (outcome, _) = download(&spec(&server.url("/f.bin"), dir.path(), 2));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(std::fs::read(&path).unwrap(), data);
}

#[test]
fn the_sidecar_is_removed_once_the_download_completes() {
    let dir = TempDir::new("hydra-cleanup").unwrap();
    let server = ServerBuilder::new()
        .file("/f.bin", test_data(5_000))
        .start();
    let (outcome, _) = download(&spec(&server.url("/f.bin"), dir.path(), 2));
    assert!(outcome.is_ok());

    let part = part_path_for(&dir.path().join("f.bin"));
    assert!(!part.exists(), "the .part file was left behind");
    assert!(
        !sidecar_path_for(&part).exists(),
        "the sidecar was left behind"
    );
}

// ------------------------------------------------------------------- controls

#[test]
fn cancelling_removes_the_partial_file() {
    let data = test_data(400_000);
    let dir = TempDir::new("hydra-cancel").unwrap();
    let server = ServerBuilder::new()
        .file_with("/f.bin", data, |f| {
            f.delay_per_chunk = Some(Duration::from_millis(12))
        })
        .start();

    let s = spec(&server.url("/f.bin"), dir.path(), 2);
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    let handle = {
        let spec = s.clone();
        let shared = shared.clone();
        std::thread::spawn(move || run(&spec, &shared, &throttle))
    };

    let deadline = Instant::now() + Duration::from_secs(20);
    while shared.downloaded() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    shared.cancel();
    let outcome = handle.join().unwrap();

    assert!(matches!(outcome, Ok(Outcome::Cancelled)), "got {outcome:?}");
    assert_eq!(shared.status(), Status::Cancelled);
    let part = part_path_for(&dir.path().join("f.bin"));
    assert!(!part.exists(), "cancelling left a partial file");
    assert!(
        !sidecar_path_for(&part).exists(),
        "cancelling left a sidecar"
    );
}

/// Pausing must interrupt in-flight reads quickly, not wait out a socket
/// timeout. A user who presses pause expects the transfer to stop now.
#[test]
fn pausing_takes_effect_promptly() {
    let data = test_data(2_000_000);
    let dir = TempDir::new("hydra-pausefast").unwrap();
    let server = ServerBuilder::new()
        .file_with("/f.bin", data, |f| {
            f.delay_per_chunk = Some(Duration::from_millis(30))
        })
        .start();

    let s = spec(&server.url("/f.bin"), dir.path(), 4);
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    let handle = {
        let spec = s.clone();
        let shared = shared.clone();
        std::thread::spawn(move || run(&spec, &shared, &throttle))
    };

    let deadline = Instant::now() + Duration::from_secs(20);
    while shared.downloaded() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }

    let pressed = Instant::now();
    shared.pause();
    let outcome = handle.join().unwrap();
    let took = pressed.elapsed();

    assert!(matches!(outcome, Ok(Outcome::Paused)), "got {outcome:?}");
    assert!(took < Duration::from_secs(5), "pause took {took:?}");
}

// ------------------------------------------------------------------ checksums

#[test]
fn a_matching_checksum_passes() {
    let data = test_data(50_000);
    let dir = TempDir::new("hydra-sum-ok").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let mut s = spec(&server.url("/f.bin"), dir.path(), 4);
    s.checksum = Some((HashAlgo::Sha256, Sha256::hex_digest(&data)));
    let (outcome, _) = download(&s);
    assert!(
        matches!(outcome, Ok(Outcome::Completed { .. })),
        "{outcome:?}"
    );
}

/// A file that fails its checksum must not be delivered under its real name.
#[test]
fn a_failing_checksum_blocks_the_rename() {
    let data = test_data(50_000);
    let dir = TempDir::new("hydra-sum-bad").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data).start();

    let mut s = spec(&server.url("/f.bin"), dir.path(), 4);
    s.checksum = Some((HashAlgo::Sha256, "00".repeat(32)));
    let (outcome, shared) = download(&s);

    assert!(outcome.is_err(), "a bad checksum must fail the download");
    assert!(shared.error().unwrap().contains("checksum"));
    assert!(
        !dir.path().join("f.bin").exists(),
        "a corrupt file was delivered"
    );
}

// ------------------------------------------------------------------ throttling

#[test]
fn the_speed_limit_is_enforced() {
    // 200 KiB at 100 KiB/s should take about two seconds.
    let data = test_data(200 * 1024);
    let dir = TempDir::new("hydra-throttle").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::new(100 * 1024));
    let started = Instant::now();
    let outcome = run(
        &spec(&server.url("/f.bin"), dir.path(), 4),
        &shared,
        &throttle,
    );
    let elapsed = started.elapsed();

    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(std::fs::read(&path).unwrap(), data);
    // One second of burst credit is allowed, so the floor is ~1s, not 2s.
    assert!(
        elapsed >= Duration::from_millis(900),
        "finished too fast: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "throttling overshot: {elapsed:?}"
    );
}

#[test]
fn raising_the_limit_mid_download_takes_effect_immediately() {
    let data = test_data(400 * 1024);
    let dir = TempDir::new("hydra-unthrottle").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();

    let shared = Arc::new(Shared::new());
    // Start crawling: 400 KiB at 20 KiB/s would take twenty seconds.
    let throttle = Arc::new(Throttle::new(20 * 1024));
    let lifter = {
        let throttle = throttle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            throttle.set_rate(0);
        })
    };

    let started = Instant::now();
    let outcome = run(
        &spec(&server.url("/f.bin"), dir.path(), 4),
        &shared,
        &throttle,
    );
    let elapsed = started.elapsed();
    lifter.join().unwrap();

    assert!(
        matches!(outcome, Ok(Outcome::Completed { .. })),
        "{outcome:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "lifting the limit did not wake the waiting threads: {elapsed:?}"
    );
}

// -------------------------------------------------------------------- errors

#[test]
fn a_missing_file_fails_without_creating_anything() {
    let dir = TempDir::new("hydra-404").unwrap();
    let server = ServerBuilder::new()
        .file("/exists.bin", test_data(10))
        .start();
    let (outcome, shared) = download(&spec(&server.url("/missing.bin"), dir.path(), 4));

    assert!(outcome.is_err());
    assert_eq!(shared.status(), Status::Failed);
    assert!(
        shared.error().unwrap().contains("404"),
        "got {:?}",
        shared.error()
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "left files behind"
    );
}

#[test]
fn an_unreachable_host_fails_cleanly() {
    let dir = TempDir::new("hydra-unreachable").unwrap();
    // Port 1 on loopback refuses connections immediately.
    let (outcome, shared) = download(&spec("http://127.0.0.1:1/f.bin", dir.path(), 4));
    assert!(outcome.is_err());
    assert_eq!(shared.status(), Status::Failed);
}

// ------------------------------------------------------------------- progress

#[test]
fn progress_reaches_exactly_one_hundred_percent() {
    let data = test_data(150_000);
    let dir = TempDir::new("hydra-progress").unwrap();
    let server = ServerBuilder::new().file("/f.bin", data.clone()).start();
    let (outcome, shared) = download(&spec(&server.url("/f.bin"), dir.path(), 8));

    assert!(
        matches!(outcome, Ok(Outcome::Completed { .. })),
        "{outcome:?}"
    );
    assert_eq!(shared.downloaded(), data.len() as u64);
    assert_eq!(shared.total(), Some(data.len() as u64));
    assert_eq!(shared.fraction(), Some(1.0));

    // Segment bars must together account for the whole file, with no overlap
    // and no gap left by work-stealing.
    let segments = shared.segment_progress();
    let accounted: u64 = segments.iter().map(|(_, _, done)| *done).sum();
    assert_eq!(accounted, data.len() as u64, "segments: {segments:?}");
}
