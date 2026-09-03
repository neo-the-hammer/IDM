<div align="center">

# 🐉 Hydra Download Manager

**A modern, open-source replacement for Internet Download Manager.**

Many heads, one file. Segmented multi-connection downloading, resumable transfers,
queues and scheduling, browser capture, and a genuinely modern themeable interface.

*Zero third-party dependencies. No adware. No license key. No nag screens.*

</div>

---

## Why

Internet Download Manager has been the benchmark for download acceleration for two decades,
but it is closed-source, Windows-only, paid, and its interface has barely changed since 2005.

Hydra aims to match it feature-for-feature while being open, cross-platform, and pleasant to look at.

## Highlights

| | |
| --- | --- |
| ⚡ **Segmented downloads** | Up to 32 parallel connections per file with dynamic re-segmentation — idle connections steal work from slow ones |
| ⏯️ **Real resume** | Survives crashes, reboots and network loss. Resume state is validated against `ETag`/`Last-Modified`/size so you never silently corrupt a file |
| 📅 **Queues & scheduler** | Concurrency limits, start/stop times, days of the week, periodic sync, post-download actions |
| 🎚️ **Speed limiting** | Global and per-download token-bucket throttling |
| 🌐 **Browser capture** | MV3 extensions for Chrome/Edge/Brave and Firefox, with cookie, referer and user-agent hand-off |
| 🕷️ **Site grabber** | Crawl a site with filters, or expand batch patterns like `file[1-100].jpg` |
| 🎨 **12 built-in themes** | Light, Dark, AMOLED, Nord, Dracula, Catppuccin, Gruvbox, Tokyo Night, Solarized, Rosé Pine — plus custom accent colors and your own themes |
| 🌍 **Localized** | English and Persian, with full right-to-left layout support |
| 🧩 **Scriptable** | REST + WebSocket API, a real CLI, and a Python plugin system for site extractors |

## Architecture

```
        browser ext (MV3)        web UI (TS+CSS)        hdm CLI
               │                       │                   │
        native host bridge         REST + WebSocket        │
               └───────────────┬───────┴───────────────────┘
                               ▼
                        hdm-daemon (hdmd)
                               │
 ┌─────────────────────────────┼──────────────────────────┐
 ▼                             ▼                          ▼
hdm-core                   hdm-api                 python plugin host
segments · resume        HTTP/1.1 + WS             spider · batch
queues · scheduler        token auth               HLS/DASH · yt-dlp
 │
 ▼
hdm-net ── Transport ──┬── raw: TCP + TLS (OpenSSL)   [Unix, macOS]
                       └── winhttp: winhttp.dll       [Windows]
```

**Rust** does the network and the engine. **Python** is the extraction brain.
**TypeScript** is the face.

### Zero dependencies, on purpose

Hydra's Rust code has an **empty dependency tree** — no `tokio`, no `reqwest`, no `openssl-sys`.
It uses the standard library plus hand-written FFI to TLS that the operating system already ships:
OpenSSL 3 (loaded at runtime via `dlopen`, so no `libssl-dev` is needed to run) on Unix, and
WinHTTP on Windows.

A download manager is a security-sensitive network client that runs constantly and touches
untrusted servers. An empty dependency tree is the strongest supply-chain guarantee we can give
you, and it keeps the binary small and the build instant.

## Building

```bash
git clone https://github.com/neo-the-hammer/IDM
cd IDM
cargo build --release
```

That's the whole thing — there is nothing to `npm install` and nothing to `pip install`.

Platform notes live in [`packaging/`](packaging/): [Windows](packaging/windows/README.md),
[Linux](packaging/linux/README.md), [macOS](packaging/macos/README.md).

## Status

Under active development. See [`docs/ROADMAP.md`](docs/ROADMAP.md) for what is done and what is next.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option.

---

<div align="center" dir="rtl">

## هایدرا — مدیر دانلود متن‌باز

جایگزینی مدرن و متن‌باز برای Internet Download Manager.

دانلود چنداتصاله با تقسیم‌بندی پویا، ازسرگیری مطمئن دانلودهای نیمه‌کاره، صف و زمان‌بندی،
یکپارچگی با مرورگر، و رابط کاربری واقعاً مدرن با ۱۲ تم آماده.

**بدون هیچ وابستگی خارجی. بدون تبلیغات. بدون کرک و لایسنس.**

رابط کاربری به‌طور کامل از فارسی و چیدمان راست‌به‌چپ پشتیبانی می‌کند.

</div>
