# The Hydra extraction plugins

Hydra's engine is Rust. This package is the part that reads pages.

Parsing real-world HTML, recognising media, and delegating to yt-dlp are jobs
Python is genuinely better at, and running them in a separate process means a
malformed page or a crashing extractor cannot take the daemon down.

**Python is optional.** Downloading, resuming, queues, scheduling and batch
patterns all work without it. The site grabber and media extraction are the
parts that need it, and they say so plainly when it is missing — `hdm plugins`
reports exactly what is and is not available.

## The division of labour

**The daemon fetches; this package only parses.** Cookies, authentication,
proxies and TLS stay in one place rather than being reimplemented here, so a
crawl behaves exactly like a download of the same URL.

## The protocol

One JSON request per line on stdin, one JSON response per line on stdout.
Line-delimited rather than length-prefixed because it is trivial to drive by
hand when something goes wrong:

```sh
echo '{"action":"capabilities"}' | python3 -m hdm_plugins
```

Every response carries `ok`. A failure is a reply, never a traceback on stdout
and never a non-zero exit — the daemon has to be able to tell "this page had no
links" from "the plugin host is broken", and a crash makes those look alike.

| Action | Takes | Returns |
| --- | --- | --- |
| `ping` | — | `version` |
| `capabilities` | — | `actions`, `python`, `ytdlp` |
| `links` | `url`, `html` | every URL on the page, resolved |
| `media` | `url`, `html` | playable media, streams flagged |
| `hls` | `url`, `text` | an HLS playlist's variants, or its segments |
| `dash` | `url`, `text` | a DASH manifest's streams and their segments |
| `manifest` | `url`, `text` | either of the above, decided from the bytes |
| `ytdlp` | `url` | what yt-dlp says the media is |

`manifest` exists because the daemon usually cannot tell the two apart from the
URL: plenty of streams are served from an extensionless path under a generic
content type. The first line of the file, however, is unambiguous.

## Modules

| File | Role |
| --- | --- |
| `protocol.py` | Framing, and turning an exception into a reply |
| `links.py` | Tolerant HTML link extraction on `html.parser` |
| `media.py` | Telling a playable file from a streaming manifest |
| `hls.py` | HLS playlists (RFC 8216): variants, segments, keys, byte ranges |
| `dash.py` | DASH manifests (ISO/IEC 23009-1) and their segment templates |
| `ytdlp.py` | The optional bridge, absent cleanly when yt-dlp is not installed |

### Why `html.parser`

Real pages are not well-formed. A site grabber that gives up on the first
unclosed tag is useless, so the parser is deliberately the forgiving one and
returns whatever it managed to read.

### Streams are not files

An `.mp4` can simply be downloaded. An `.m3u8` or `.mpd` is an index of
thousands of segments, and offering it as though it were a file would hand the
user a few kilobytes of text named like a video. Manifests are flagged
`streaming` so nothing pretends otherwise, and `hls.py` and `dash.py` turn one
into the list of things that actually have to be fetched.

Parsing only, in both. The daemon does every fetch, so cookies, authentication,
proxies and speed limits behave for a stream exactly as they do for a file —
and none of that has to exist twice.

A DASH segment count derived from `mediaPresentationDuration` is marked
`estimatedCount`, because a manifest declaring `PT16.016S` of four-second
segments is genuinely ambiguous about whether there are four or five. The
daemon lets the last one be missing when that flag is set, and only the last
one.

### yt-dlp does extraction only

yt-dlp is asked *where* the media is; Hydra downloads it. The transfer keeps
segmentation, resume, throttling and the queue — which is the entire reason for
using a download manager.

## Tests

```sh
python3 -m unittest discover -s python/tests
```

No dependencies beyond the standard library.
