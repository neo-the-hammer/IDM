//! The media grabber, against a local HLS server.
//!
//! These exercise the whole path — fetch the manifest, parse it through the
//! Python plugin host, fetch every segment in parallel, decrypt, concatenate —
//! and check the bytes on disk, because a media download that produces a file
//! of the right *size* and the wrong *contents* is the failure that matters.

use hdm_core::engine::{DownloadSpec, Outcome, Shared, Status};
use hdm_core::media::{self, MediaSelection};
use hdm_core::throttle::Throttle;
use hdm_testserver::{Route, ServerBuilder, TestServer};
use std::sync::Arc;

fn playlist(path: &str, body: String) -> Route {
    let mut file = hdm_testserver::FileRoute::new(body.into_bytes());
    file.content_type = Some("application/vnd.apple.mpegurl".into());
    let _ = path;
    Route::File(std::sync::Arc::new(file))
}

/// Deterministic, distinguishable segment bodies: if the assembler puts them
/// back in the wrong order the comparison fails loudly rather than subtly.
fn segment_body(index: usize) -> Vec<u8> {
    (0..4096u32)
        .map(|i| (i.wrapping_mul(31).wrapping_add(index as u32 * 7)) as u8)
        .collect()
}

fn expected(count: usize) -> Vec<u8> {
    (0..count).flat_map(segment_body).collect()
}

/// A master playlist, two variants, and a five-segment media playlist.
fn hls_site() -> TestServer {
    let mut builder = ServerBuilder::new().route(
        "/master.m3u8",
        playlist(
            "/master.m3u8",
            "#EXTM3U\n\
             #EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360,CODECS=\"avc1.42c01e,mp4a.40.2\"\n\
             low.m3u8\n\
             #EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\"\n\
             high.m3u8\n"
                .to_string(),
        ),
    );

    for name in ["low", "high"] {
        let mut body = String::from("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n");
        for index in 0..5 {
            body.push_str(&format!("#EXTINF:4.0,\n{name}-{index}.ts\n"));
        }
        body.push_str("#EXT-X-ENDLIST\n");
        builder = builder.route(&format!("/{name}.m3u8"), playlist("", body));
        for index in 0..5 {
            builder = builder.file(&format!("/{name}-{index}.ts"), segment_body(index));
        }
    }
    builder.start()
}

fn download(spec: DownloadSpec) -> (std::io::Result<Outcome>, Arc<Shared>) {
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    let outcome = media::run(&spec, &shared, &throttle);
    (outcome, shared)
}

fn spec_for(url: &str, directory: &std::path::Path, selection: MediaSelection) -> DownloadSpec {
    let mut spec = DownloadSpec::new(url, directory.to_path_buf());
    spec.connections = 4;
    spec.media = Some(selection);
    spec
}

#[test]
fn probing_a_master_playlist_lists_its_variants_best_first() {
    let server = hls_site();
    let spec = DownloadSpec::new(server.url("/master.m3u8"), std::env::temp_dir());

    let probe = media::probe(&spec).expect("the master playlist should parse");
    assert_eq!(probe.format, "hls");
    assert_eq!(probe.streams.len(), 2);
    assert_eq!(probe.streams[0].height, Some(1080));
    assert_eq!(probe.streams[1].height, Some(360));
    // The codec list contains a comma of its own, which naive attribute
    // splitting truncates.
    assert_eq!(probe.streams[0].codecs, "avc1.640028,mp4a.40.2");
    assert!(probe.streams[0].label().starts_with("1080p"));
    assert!(!probe.separate_audio);
}

#[test]
fn probing_a_media_playlist_counts_its_segments() {
    let server = hls_site();
    let spec = DownloadSpec::new(server.url("/high.m3u8"), std::env::temp_dir());

    let probe = media::probe(&spec).expect("the media playlist should parse");
    assert_eq!(probe.streams.len(), 1);
    assert_eq!(probe.streams[0].segments, 5);
    assert!(!probe.live);
    assert_eq!(probe.duration, 20.0);
    assert!(probe.warnings.is_empty());
}

#[test]
fn a_stream_is_saved_as_its_segments_concatenated_in_order() {
    let server = hls_site();
    let directory = tempdir("hls-plain");
    let spec = spec_for(
        &server.url("/high.m3u8"),
        &directory,
        MediaSelection::new("hls", server.url("/high.m3u8")),
    );

    let (outcome, shared) = download(spec);
    let Ok(Outcome::Completed { path, bytes }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };

    assert_eq!(std::fs::read(&path).unwrap(), expected(5));
    assert_eq!(bytes, 5 * 4096);
    assert_eq!(shared.status(), Status::Completed);
    // The estimate is replaced by the truth once the file exists.
    assert_eq!(shared.total(), Some(5 * 4096));
    assert_eq!(shared.downloaded(), 5 * 4096);
    // MPEG-TS, so no remux was needed and the extension says so.
    assert_eq!(path.extension().unwrap(), "ts");
    // Nothing is left behind.
    assert!(!working_directories(&directory).any(|_| true));
}

#[test]
fn a_master_playlist_downloads_its_best_variant() {
    let server = hls_site();
    let directory = tempdir("hls-master");
    let spec = spec_for(
        &server.url("/master.m3u8"),
        &directory,
        MediaSelection::new("hls", server.url("/master.m3u8")),
    );

    let (outcome, _) = download(spec);
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };
    assert_eq!(std::fs::read(&path).unwrap(), expected(5));
    // The high variant, not the low one: every request should have been for it.
    assert!(server
        .requests()
        .iter()
        .any(|r| r.target.contains("high-0.ts")));
    assert!(!server
        .requests()
        .iter()
        .any(|r| r.target.contains("low-0.ts")));
}

#[test]
fn an_encrypted_stream_is_decrypted_with_the_playlist_key() {
    let key: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(17)).collect();
    let mut builder = ServerBuilder::new().file("/enc.key", key.clone());

    let mut body = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:0\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"enc.key\"\n",
    );
    for index in 0..4 {
        body.push_str(&format!("#EXTINF:4.0,\ns{index}.ts\n"));
        // No IV in the playlist, so the IV is the segment's sequence number as
        // a big-endian 128-bit integer. This is the part that silently
        // produces noise when it is wrong.
        let mut iv = vec![0u8; 16];
        iv[8..].copy_from_slice(&(index as u64).to_be_bytes());
        let ciphertext = hdm_crypto::cbc_encrypt(&key, &iv, &segment_body(index)).unwrap();
        builder = builder.file(&format!("/s{index}.ts"), ciphertext);
    }
    body.push_str("#EXT-X-ENDLIST\n");
    let server = builder.route("/enc.m3u8", playlist("", body)).start();

    let directory = tempdir("hls-encrypted");
    let spec = spec_for(
        &server.url("/enc.m3u8"),
        &directory,
        MediaSelection::new("hls", server.url("/enc.m3u8")),
    );

    let (outcome, _) = download(spec);
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };
    assert_eq!(std::fs::read(&path).unwrap(), expected(4));

    // The key is fetched once and reused, not re-fetched per segment.
    let key_requests = server
        .requests()
        .iter()
        .filter(|r| r.target.contains("enc.key"))
        .count();
    assert_eq!(key_requests, 1, "the key should be fetched exactly once");
}

#[test]
fn a_fragmented_mp4_stream_keeps_its_init_segment_first() {
    let init = b"INITIALISATION-SEGMENT".to_vec();
    let mut builder = ServerBuilder::new().file("/init.mp4", init.clone());
    let mut body = String::from(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:4\n\
         #EXT-X-MAP:URI=\"init.mp4\"\n",
    );
    for index in 0..3 {
        body.push_str(&format!("#EXTINF:4.0,\nf{index}.m4s\n"));
        builder = builder.file(&format!("/f{index}.m4s"), segment_body(index));
    }
    body.push_str("#EXT-X-ENDLIST\n");
    let server = builder.route("/fmp4.m3u8", playlist("", body)).start();

    let directory = tempdir("hls-fmp4");
    let spec = spec_for(
        &server.url("/fmp4.m3u8"),
        &directory,
        MediaSelection::new("hls", server.url("/fmp4.m3u8")),
    );

    let (outcome, _) = download(spec);
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };
    let mut want = init;
    want.extend(expected(3));
    assert_eq!(std::fs::read(&path).unwrap(), want);
    // An init segment means fMP4, which takes the .mp4 extension.
    assert_eq!(path.extension().unwrap(), "mp4");
}

#[test]
fn a_paused_download_keeps_its_segments_and_resumes_into_them() {
    let server = hls_site();
    let directory = tempdir("hls-resume");
    let url = server.url("/high.m3u8");
    let spec = spec_for(&url, &directory, MediaSelection::new("hls", url.clone()));

    // Pause almost immediately, so only some of the five segments land.
    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            shared.pause();
        });
    }
    let outcome = media::run(&spec, &shared, &throttle);
    assert!(
        matches!(outcome, Ok(Outcome::Paused)),
        "expected a pause, got {outcome:?}"
    );
    assert_eq!(shared.status(), Status::Paused);

    let before = server.request_count();
    // Resuming must not re-fetch what is already on disk.
    let (outcome, _) = download(spec_for(
        &url,
        &directory,
        MediaSelection::new("hls", url.clone()),
    ));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };
    assert_eq!(std::fs::read(&path).unwrap(), expected(5));
    let after = server.request_count() - before;
    // One playlist fetch plus at most the five segments; a restart from
    // scratch would be six every time even when segments survived.
    assert!(after <= 6, "resume re-fetched too much: {after} requests");
}

#[test]
fn a_cancelled_download_leaves_nothing_behind() {
    let server = hls_site();
    let directory = tempdir("hls-cancel");
    let url = server.url("/high.m3u8");
    let spec = spec_for(&url, &directory, MediaSelection::new("hls", url.clone()));

    let shared = Arc::new(Shared::new());
    let throttle = Arc::new(Throttle::unlimited());
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            shared.cancel();
        });
    }
    let outcome = media::run(&spec, &shared, &throttle);
    assert!(
        matches!(outcome, Ok(Outcome::Cancelled)),
        "expected a cancellation, got {outcome:?}"
    );
    assert!(
        !working_directories(&directory).any(|_| true),
        "a cancelled media download must not leave its segments behind"
    );
}

#[test]
fn a_missing_segment_fails_the_download_rather_than_producing_a_short_file() {
    let mut body = String::from("#EXTM3U\n#EXT-X-TARGETDURATION:4\n");
    let mut builder = ServerBuilder::new();
    for index in 0..3 {
        body.push_str(&format!("#EXTINF:4.0,\ng{index}.ts\n"));
        if index != 1 {
            builder = builder.file(&format!("/g{index}.ts"), segment_body(index));
        }
    }
    body.push_str("#EXT-X-ENDLIST\n");
    // The middle segment is simply absent.
    let server = builder
        .status("/g1.ts", 404)
        .route("/gap.m3u8", playlist("", body))
        .start();

    let directory = tempdir("hls-gap");
    let url = server.url("/gap.m3u8");
    let spec = spec_for(&url, &directory, MediaSelection::new("hls", url.clone()));
    let (outcome, shared) = download(spec);

    let error = outcome.expect_err("a missing segment must fail the download");
    assert!(error.to_string().contains("404"), "{error}");
    assert_eq!(shared.status(), Status::Failed);
    // Nothing playable should have been produced from an incomplete stream.
    assert!(!directory.join("gap.ts").exists());
}

#[test]
fn a_manifest_that_is_not_a_manifest_is_rejected_clearly() {
    let server = ServerBuilder::new()
        .file("/not-a-playlist.m3u8", b"<html>nope</html>".to_vec())
        .start();
    let spec = DownloadSpec::new(server.url("/not-a-playlist.m3u8"), std::env::temp_dir());

    let error = media::probe(&spec).expect_err("plain HTML is not a manifest");
    assert!(
        error.contains("HLS") || error.contains("DASH"),
        "the reason should name what was expected: {error}"
    );
}

#[test]
fn a_dash_manifest_expands_its_segment_template() {
    let mut builder = ServerBuilder::new();
    builder = builder.file("/v/init.mp4", b"DASH-INIT".to_vec());
    for index in 1..=4 {
        builder = builder.file(&format!("/v/seg-{index:05}.m4s"), segment_body(index - 1));
    }
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="v/init.mp4" media="v/seg-$Number%05d$.m4s"
                       duration="4" timescale="1" startNumber="1"/>
      <Representation id="v0" bandwidth="1200000" width="1280" height="720" codecs="avc1.4d401f"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;
    let server = builder
        .route("/stream.mpd", {
            let mut file = hdm_testserver::FileRoute::new(mpd.as_bytes().to_vec());
            file.content_type = Some("application/dash+xml".into());
            Route::File(std::sync::Arc::new(file))
        })
        .start();

    let spec = DownloadSpec::new(server.url("/stream.mpd"), std::env::temp_dir());
    let probe = media::probe(&spec).expect("the MPD should parse");
    assert_eq!(probe.format, "dash");
    assert_eq!(probe.streams.len(), 1);
    assert_eq!(probe.streams[0].height, Some(720));
    // 16 seconds of 4-second segments is exactly four, not five: rounding a
    // whole division up would ask for a segment that does not exist.
    assert_eq!(probe.streams[0].segments, 4);
    assert!(!probe.separate_audio);

    // Downloading it exercises the same template expansion end to end.
    let directory = tempdir("dash-template");
    let url = server.url("/stream.mpd");
    let mut selection = MediaSelection::new("dash", url.clone());
    selection.stream_id = Some("v0".into());
    let (outcome, _) = download(spec_for(&url, &directory, selection));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("expected a completed download, got {outcome:?}");
    };
    let mut want = b"DASH-INIT".to_vec();
    want.extend(expected(4));
    assert_eq!(std::fs::read(&path).unwrap(), want);
}

#[test]
fn a_derived_segment_count_may_overshoot_by_one_without_failing() {
    // A manifest whose declared duration is a hair over four whole segments,
    // which is what real encoders produce. The fifth segment it implies does
    // not exist, and that must not fail the download.
    let mut builder = ServerBuilder::new().file("/v/init.mp4", b"INIT".to_vec());
    for index in 1..=4 {
        builder = builder.file(&format!("/v/seg-{index:05}.m4s"), segment_body(index - 1));
    }
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16.016S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="v/init.mp4" media="v/seg-$Number%05d$.m4s"
                       duration="4" timescale="1" startNumber="1"/>
      <Representation id="v0" bandwidth="1200000" width="1280" height="720" codecs="avc1.4d401f"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;
    let server = builder
        .status("/v/seg-00005.m4s", 404)
        .route("/rough.mpd", {
            let mut file = hdm_testserver::FileRoute::new(mpd.as_bytes().to_vec());
            file.content_type = Some("application/dash+xml".into());
            Route::File(std::sync::Arc::new(file))
        })
        .start();

    let spec = DownloadSpec::new(server.url("/rough.mpd"), std::env::temp_dir());
    let probe = media::probe(&spec).expect("the MPD should parse");
    assert_eq!(probe.streams[0].segments, 5, "the derived count overshoots");

    let directory = tempdir("dash-overshoot");
    let url = server.url("/rough.mpd");
    let mut selection = MediaSelection::new("dash", url.clone());
    selection.stream_id = Some("v0".into());
    let (outcome, _) = download(spec_for(&url, &directory, selection));
    let Ok(Outcome::Completed { path, .. }) = outcome else {
        panic!("a missing tail segment must not fail the download: {outcome:?}");
    };
    let mut want = b"INIT".to_vec();
    want.extend(expected(4));
    assert_eq!(std::fs::read(&path).unwrap(), want);
}

#[test]
fn a_missing_segment_that_is_not_the_tail_still_fails() {
    // The same tolerance must not extend to a gap in the middle, which would
    // silently produce a video with a hole in it.
    let mut builder = ServerBuilder::new().file("/v/init.mp4", b"INIT".to_vec());
    for index in [1, 3, 4] {
        builder = builder.file(&format!("/v/seg-{index:05}.m4s"), segment_body(index - 1));
    }
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT16S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <SegmentTemplate initialization="v/init.mp4" media="v/seg-$Number%05d$.m4s"
                       duration="4" timescale="1" startNumber="1"/>
      <Representation id="v0" bandwidth="1200000" width="1280" height="720" codecs="avc1.4d401f"/>
    </AdaptationSet>
  </Period>
</MPD>
"#;
    let server = builder
        .status("/v/seg-00002.m4s", 404)
        .route("/hole.mpd", {
            let mut file = hdm_testserver::FileRoute::new(mpd.as_bytes().to_vec());
            file.content_type = Some("application/dash+xml".into());
            Route::File(std::sync::Arc::new(file))
        })
        .start();

    let directory = tempdir("dash-hole");
    let url = server.url("/hole.mpd");
    let mut selection = MediaSelection::new("dash", url.clone());
    selection.stream_id = Some("v0".into());
    let (outcome, _) = download(spec_for(&url, &directory, selection));
    assert!(
        outcome.is_err(),
        "a gap in the middle must fail: {outcome:?}"
    );
}

#[test]
fn dash_video_and_audio_are_recognised_as_separate_tracks() {
    let mpd = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT8S">
  <Period>
    <AdaptationSet contentType="video" mimeType="video/mp4">
      <Representation id="v0" bandwidth="2000000" width="1920" height="1080" codecs="avc1.640028">
        <SegmentList>
          <Initialization sourceURL="v/init.mp4"/>
          <SegmentURL media="v/1.m4s"/>
          <SegmentURL media="v/2.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
    <AdaptationSet contentType="audio" mimeType="audio/mp4">
      <Representation id="a0" bandwidth="128000" codecs="mp4a.40.2">
        <SegmentList>
          <Initialization sourceURL="a/init.mp4"/>
          <SegmentURL media="a/1.m4s"/>
        </SegmentList>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#;
    let server = ServerBuilder::new()
        .route("/two.mpd", {
            let mut file = hdm_testserver::FileRoute::new(mpd.as_bytes().to_vec());
            file.content_type = Some("application/dash+xml".into());
            Route::File(std::sync::Arc::new(file))
        })
        .start();

    let spec = DownloadSpec::new(server.url("/two.mpd"), std::env::temp_dir());
    let probe = media::probe(&spec).expect("the MPD should parse");
    assert!(probe.separate_audio);
    assert_eq!(probe.streams.len(), 2);
    // Video first, which is what someone choosing a quality expects to see.
    assert_eq!(probe.streams[0].kind, "video");
    assert_eq!(probe.streams[1].kind, "audio");
    assert_eq!(probe.best().unwrap().id, "v0");
    assert_eq!(probe.best_audio().unwrap().id, "a0");
    if media::ffmpeg_path().is_none() {
        // The limitation is stated rather than discovered at the end.
        assert!(
            probe.warnings.iter().any(|w| w.contains("ffmpeg")),
            "{:?}",
            probe.warnings
        );
    }
}

#[test]
fn a_selection_survives_being_written_to_the_state_file() {
    let mut selection = MediaSelection::new("dash", "https://example.com/s.mpd");
    selection.stream_id = Some("v2".into());
    selection.audio_url = Some("https://example.com/s.mpd".into());
    selection.audio_stream_id = Some("a1".into());
    selection.remux = true;

    let text = selection.to_json().to_string_compact();
    let restored = MediaSelection::from_json(&hdm_json::parse(&text).unwrap()).unwrap();
    assert_eq!(restored, selection);
}

// ------------------------------------------------------------------- helpers

fn tempdir(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("hydra-media-{name}-{unique}"));
    std::fs::create_dir_all(&path).unwrap();
    path
}

/// The `.name.trackN.hdm` directories a media download works in.
fn working_directories(
    directory: &std::path::Path,
) -> impl Iterator<Item = std::path::PathBuf> + use<> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .map(|n| n.to_string_lossy().ends_with(".hdm"))
                    .unwrap_or(false)
        })
}
