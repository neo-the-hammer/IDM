# Roadmap

Hydra is built in milestones. Each one is independently useful and lands as its own commit.

| # | Milestone | Status |
| --- | --- | --- |
| M0 | Workspace scaffold, licenses, TLS spike | ✅ done |
| M1 | `hdm-json` + `hdm-crypto` | ✅ done |
| M2 | `hdm-net` — HTTP/1.1, TLS, FTP, auth, cookies, proxy | ✅ done |
| M3 | `hdm-core` — segmented engine, resume, throttle, store | ✅ done |
| M4 | WinHTTP transport, REST + WebSocket API, `hdmd`, `hdm` CLI | ✅ done |
| M5 | Themed web UI (12 themes, en/fa, RTL) | ✅ done |
| M6 | Queues, scheduler, post-download actions | ✅ done |
| M7 | Browser extensions + native messaging host | ✅ done |
| M8 | Python plugin host, site grabber, batch patterns | ✅ done |
| M9 | Packaging (Windows/Linux/macOS), Tauri shell | ⏳ |
| M10 | Documentation | ⏳ |
| M11 | Media grabber — HLS/DASH, ffmpeg mux, yt-dlp bridge | 🔜 later |

## Deliberately out of scope

- **BitTorrent / magnet links.** IDM does not do torrents either, and a good BitTorrent client is
  a large project in its own right. Use a dedicated client.
- **HTTP/2 and HTTP/3 in the portable transport.** HTTP/1.1 with `Range` is universally supported
  and is exactly what segmentation needs. Windows gets HTTP/2 for free through WinHTTP.

## Known limitations

- **The Windows code paths are written but not yet machine-verified.** They were developed in a
  Linux container with no Windows Rust target available, so `transport::winhttp` and
  `platform::windows` have not been compiled. They are deliberately small and isolated behind
  `#[cfg]` so they cannot affect the tested Unix paths. The first Windows build is a checkpoint,
  not a formality — please report anything that does not compile or behave.
