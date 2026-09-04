# The Hydra API

Everything the interface, the CLI and the browser extensions do goes through
this API. It is stable enough to script against.

## Connecting

The daemon listens on `127.0.0.1` only and publishes where, on startup:

```jsonc
// ~/.local/share/hydra/daemon.json  (%LOCALAPPDATA%\Hydra\daemon.json on Windows)
{ "port": 47113, "token": "…", "pid": 12345, "url": "http://127.0.0.1:47113" }
```

That file is `0600`. Every request needs the token:

```sh
TOKEN=$(python3 -c 'import json;print(json.load(open("'$HOME'/.local/share/hydra/daemon.json"))["token"])')
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:47113/api/v1/downloads
```

### What it refuses, and why

| | |
| --- | --- |
| No token, or a wrong one | `401`. Compared in constant time, so a local attacker cannot recover it a byte at a time by timing rejections. |
| A request carrying a foreign `Origin` | `403`. Without this, any web page you visited could drive the daemon through your browser. Browser extensions and loopback origins are allowed. |
| A `Host` that is not loopback | `403`. Closes DNS rebinding, where a hostname resolving to `127.0.0.1` reaches this server while keeping its own origin. |
| A path that escapes the UI directory | `404`. |

Errors are always `{"error": "..."}` with a useful message.

## Downloads

| Method | Path | Does |
| --- | --- | --- |
| `GET` | `/api/v1/downloads` | Every download, plus totals |
| `POST` | `/api/v1/downloads` | Add one |
| `GET` | `/api/v1/downloads/{id}` | One download |
| `DELETE` | `/api/v1/downloads/{id}?deleteFiles=true` | Remove it |
| `POST` | `/api/v1/downloads/{id}/{action}` | `start`, `pause`, `cancel`, `restart`, `reveal`, `limit`, `queue` |
| `POST` | `/api/v1/downloads-pause-all` | Pause everything |
| `POST` | `/api/v1/downloads-resume-all` | Resume everything |
| `POST` | `/api/v1/downloads-clear-completed` | Forget finished ones |
| `POST` | `/api/v1/downloads-batch` | Add many at once |

### Adding a download

Only `url` is required.

```jsonc
{
  "url": "https://example.com/ubuntu.iso",
  "filename": "ubuntu.iso",        // default: what the server suggests
  "directory": "/home/me/isos",    // default: chosen by category
  "connections": 16,               // 1-32; default: the daemon's setting
  "queue": "overnight",
  "category": "compressed",
  "speedLimit": 512000,            // bytes per second; 0 is unlimited
  "checksum": "ab12…",             // verified before the file is renamed
  "checksumAlgo": "sha256",        // inferred from the digest's length if absent
  "username": "me", "password": "…",
  "referer": "https://example.com/page",
  "cookies": "session=…",
  "userAgent": "Mozilla/5.0 …",
  "headers": [{ "name": "X-Token", "value": "…" }],
  "mirrors": ["https://mirror2.example.com/ubuntu.iso"],
  "proxy": "socks5://127.0.0.1:9050",
  "tlsInsecure": false,
  "autostart": true
}
```

`referer`, `cookies` and `userAgent` are what the browser extension replays. A
great many links are only valid for the session that produced them, and without
these Hydra fetches a login page and saves it as the file. Header values
containing CR or LF are dropped rather than sanitized.

### A download, as returned

```jsonc
{
  "id": "kR3n…",
  "status": "downloading",     // queued probing downloading verifying paused
                               // completed failed cancelled
  "filename": "ubuntu.iso",
  "total": 4831838208,         // null until the server says
  "downloaded": 1287654321,
  "speed": 5242880,            // bytes per second, smoothed
  "eta": 676,                  // seconds; null when the speed is zero
  "outputPath": null,          // set once finished
  "error": null,
  "queue": "main",
  "segments": [                // present while running: one per connection
    { "start": 0, "end": 603979775, "done": 402653184 }
  ]
}
```

`segments` is what the interface draws as per-connection bars. Watching them
fill and rebalance is the clearest evidence that segmentation is doing anything.

## Queues

| Method | Path | Does |
| --- | --- | --- |
| `GET` | `/api/v1/queues` | Every queue |
| `PUT` | `/api/v1/queues/{id}` | Create or update one |
| `DELETE` | `/api/v1/queues/{id}` | Remove it; its downloads move to `main` |
| `POST` | `/api/v1/queues/{id}/pause` | Stop it, including what is already running |
| `POST` | `/api/v1/queues/{id}/resume` | Start it again |

```jsonc
{
  "id": "overnight",
  "name": "Overnight",
  "concurrency": 8,
  "speedLimit": 0,
  "schedule": {
    "enabled": true,
    "start": 60,        // minutes since local midnight, so 01:00
    "stop": 420,        // 07:00; a stop earlier than start wraps past midnight
    "days": 127         // bitmask, bit 0 is Sunday; 127 is every day
  },
  "completion": { "kind": "shutdown" },   // nothing shutdown sleep hibernate exit run
  "paused": false
}
```

Times are local, not UTC: "start at 2am" should keep meaning two in the morning
across a daylight-saving change.

## Batch patterns and the site grabber

| Method | Path | Does |
| --- | --- | --- |
| `POST` | `/api/v1/expand` | Expand a pattern, without adding anything |
| `POST` | `/api/v1/crawl` | Walk a site and report its files |
| `GET` | `/api/v1/plugins` | Whether the Python extraction plugins are usable |

```jsonc
// POST /api/v1/expand
{ "pattern": "https://example.com/photo[001-250].jpg" }
// → { "count": 250, "urls": [...] }
```

Ranges are `[1-100]`, `[001-100]` for zero padding, `[a-z]`, and `[1-100:2]` to
step. Several in one pattern produce every combination. Anything over 10,000 is
refused: that is far more likely to be a typo than an intention.

```jsonc
// POST /api/v1/crawl
{
  "url": "https://example.com/docs/",
  "depth": 2,                  // capped at 8
  "include": ["pdf", "zip"],   // empty means everything that is not a page
  "exclude": [],
  "sameHost": true,            // where the crawler may *go*
  "filesSameHost": false,      // what it may *collect*; files often live on a CDN
  "stayUnderPath": true,
  "respectRobots": true,
  "delayMs": 250,
  "maxPages": 100, "maxFiles": 1000, "timeoutSeconds": 300
}
// → { "files": [{ "url", "filename", "extension", "foundOn", "text" }],
//     "pagesVisited": 4, "errors": [], "truncated": false }
```

`foundOn` becomes the download's `Referer`, which many servers require.

The site grabber needs Python 3. Batch patterns, and everything else, do not.

## The media grabber

| Method | Path | Does |
| --- | --- | --- |
| `POST` | `/api/v1/media/probe` | Read a manifest and report what it offers |
| `POST` | `/api/v1/media/download` | Add a stream as a download |

An `.m3u8` or `.mpd` is an *index* of segments, not the media itself. Passing
one to `/api/v1/downloads` saves a few kilobytes of text under a film's name;
these two routes exist so that does not happen.

```jsonc
// POST /api/v1/media/probe
{ "url": "https://example.com/video/master.m3u8" }
// →
{
  "url": "https://example.com/video/master.m3u8",
  "format": "hls",              // or "dash"
  "live": false,
  "duration": 1873.2,           // seconds; 0 when the manifest does not say
  "separateAudio": false,       // true when audio must be chosen as well
  "ffmpeg": true,               // whether the daemon found ffmpeg
  "streams": [
    {
      "id": "v0",
      "url": "https://example.com/video/1080.m3u8",
      "kind": "video",          // "video", "audio" or "text"
      "label": "1080p · 5.0 Mbit/s · avc1.640028",
      "width": 1920, "height": 1080, "bandwidth": 5000000,
      "codecs": "avc1.640028", "language": "",
      "segments": 0,            // 0 until the variant playlist is fetched
      "encrypted": false
    }
  ],
  "warnings": []                // DRM, live streams, missing ffmpeg
}
```

`POST /api/v1/media/download` takes every option `/api/v1/downloads` takes —
`directory`, `filename`, `connections`, `speedLimit`, `headers`, `cookies`,
`referer`, `proxy`, credentials — plus:

```jsonc
{
  "url": "https://example.com/video/1080.m3u8",
  "format": "hls",              // required alongside streamId
  "streamId": "v0",
  "audioUrl": null,             // for DASH, where audio is a separate stream
  "audioStreamId": null,
  "remux": false                // convert to MP4 with ffmpeg
}
```

With no `streamId`, the manifest is probed and the best video is taken — along
with an audio track when the manifest keeps them apart. The response is a
download record like any other, so progress, pause, resume and cancel all work
through the usual routes.

What happens then: segments are fetched in parallel (up to `connections`, capped
at 16), `AES-128` segments are decrypted, and the result is concatenated in
playlist order. A paused media download keeps its fetched segments and resumes
into them. MPEG-TS is saved as `.ts` and fragmented MP4 as `.mp4`; `remux`
converts either to MP4, and combining a separate audio track always needs
ffmpeg. Without ffmpeg the two tracks are saved side by side as
`name.video.mp4` and `name.audio.mp4` rather than the audio being dropped.

Like the site grabber, this needs Python 3 — it is what reads the manifests.

## Settings

`GET` and `PUT` on `/api/v1/settings`. `PUT` takes the whole object back, so
read, change and write.

## The event stream

`GET /api/v1/events` upgrades to a WebSocket and pushes a snapshot whenever
something changes — never on a timer, so an idle Hydra stays quiet.

```js
// The token goes in the query string because a browser cannot set headers on
// a WebSocket handshake.
const socket = new WebSocket(`ws://127.0.0.1:47113/api/v1/events?token=${token}`);
socket.onmessage = (event) => {
  const { downloads, totals } = JSON.parse(event.data);
};
```

## A worked example

```sh
BASE=http://127.0.0.1:47113/api/v1
AUTH="Authorization: Bearer $TOKEN"

# Queue a big file over sixteen connections, capped at 2 MiB/s.
curl -s -H "$AUTH" -H 'Content-Type: application/json' "$BASE/downloads" \
  -d '{"url":"https://example.com/big.iso","connections":16,"speedLimit":2097152}'

# Everything that failed.
curl -s -H "$AUTH" "$BASE/downloads" |
  python3 -c 'import json,sys; [print(d["id"], d["error"]) for d in json.load(sys.stdin)["downloads"] if d["status"]=="failed"]'

# Grab every PDF two levels down, then queue them.
curl -s -H "$AUTH" -H 'Content-Type: application/json' "$BASE/crawl" \
  -d '{"url":"https://example.com/docs/","depth":2,"include":["pdf"]}' > found.json
curl -s -H "$AUTH" -H 'Content-Type: application/json' "$BASE/downloads-batch" \
  -d "$(python3 -c 'import json;print(json.dumps({"urls":json.load(open("found.json"))["files"]}))')"
```
