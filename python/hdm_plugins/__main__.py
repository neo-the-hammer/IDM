"""The plugin host.

    python3 -m hdm_plugins

Reads JSON requests on stdin, one per line, and writes one JSON response each.
The daemon starts this and keeps it running; it is also perfectly usable by
hand, which is the point of the line-delimited framing:

    echo '{"action":"capabilities"}' | python3 -m hdm_plugins
"""

from __future__ import annotations

import sys

from . import __version__, dash, hls, links, media, protocol, ytdlp


def handle_capabilities(_request: dict) -> dict:
    """What this host can do, so the daemon can disable features cleanly."""
    return protocol.ok(
        version=__version__,
        python=sys.version.split()[0],
        actions=sorted(HANDLERS),
        ytdlp=ytdlp.available(),
    )


def handle_ping(_request: dict) -> dict:
    return protocol.ok(version=__version__)


def handle_links(request: dict) -> dict:
    html = request.get("html")
    url = request.get("url")
    if not isinstance(html, str) or not isinstance(url, str):
        return protocol.error("`html` and `url` are required")
    return protocol.ok(**links.extract(html, url))


def handle_media(request: dict) -> dict:
    html = request.get("html")
    url = request.get("url")
    if not isinstance(html, str) or not isinstance(url, str):
        return protocol.error("`html` and `url` are required")
    return protocol.ok(**media.find(html, url))


def handle_ytdlp(request: dict) -> dict:
    url = request.get("url")
    if not isinstance(url, str):
        return protocol.error("`url` is required")
    result = ytdlp.extract(url, timeout=int(request.get("timeout", 90)))
    if not result.pop("ok", False):
        return protocol.error(result.get("error", "extraction failed"), **result)
    return protocol.ok(**result)


def handle_hls(request: dict) -> dict:
    text = request.get("text")
    url = request.get("url")
    if not isinstance(text, str) or not isinstance(url, str):
        return protocol.error("`text` and `url` are required")
    if len(text) > hls.MAX_MANIFEST_BYTES:
        return protocol.error("the playlist is too large to be a playlist")
    parsed = hls.parse(text, url)
    if parsed.get("kind") == "invalid":
        return protocol.error(parsed.get("error", "not an HLS playlist"))
    return protocol.ok(**parsed)


def handle_dash(request: dict) -> dict:
    text = request.get("text")
    url = request.get("url")
    if not isinstance(text, str) or not isinstance(url, str):
        return protocol.error("`text` and `url` are required")
    parsed = dash.parse(text, url)
    if parsed.get("kind") == "invalid":
        return protocol.error(parsed.get("error", "not a DASH manifest"))
    return protocol.ok(**parsed)


def handle_manifest(request: dict) -> dict:
    """Parses a manifest without being told which kind it is.

    The daemon usually cannot tell from the URL: plenty of streams are served
    from an extensionless path with a generic content type. The bytes,
    however, are unambiguous.
    """
    text = request.get("text")
    if not isinstance(text, str):
        return protocol.error("`text` is required")
    if "#EXTM3U" in text[:200]:
        return handle_hls(request)
    if "<MPD" in text[:4096]:
        return handle_dash(request)
    return protocol.error("this is neither an HLS playlist nor a DASH manifest")


HANDLERS = {
    "ping": handle_ping,
    "capabilities": handle_capabilities,
    "links": handle_links,
    "media": handle_media,
    "ytdlp": handle_ytdlp,
    "hls": handle_hls,
    "dash": handle_dash,
    "manifest": handle_manifest,
}


def main() -> int:
    return protocol.serve(HANDLERS)


if __name__ == "__main__":
    raise SystemExit(main())
