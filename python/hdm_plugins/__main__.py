"""The plugin host.

    python3 -m hdm_plugins

Reads JSON requests on stdin, one per line, and writes one JSON response each.
The daemon starts this and keeps it running; it is also perfectly usable by
hand, which is the point of the line-delimited framing:

    echo '{"action":"capabilities"}' | python3 -m hdm_plugins
"""

from __future__ import annotations

import sys

from . import __version__, links, media, protocol, ytdlp


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


HANDLERS = {
    "ping": handle_ping,
    "capabilities": handle_capabilities,
    "links": handle_links,
    "media": handle_media,
    "ytdlp": handle_ytdlp,
}


def main() -> int:
    return protocol.serve(HANDLERS)


if __name__ == "__main__":
    raise SystemExit(main())
