"""An optional bridge to yt-dlp.

yt-dlp knows how to extract media from well over a thousand sites, which is far
more than Hydra's own parsing will ever cover, and reimplementing that would be
foolish. But it is a large dependency that many users will not have, so this is
strictly optional: everything else works without it, and its absence is reported
as a fact rather than an error.

Only extraction is delegated. yt-dlp is asked *where* the media is, and Hydra
downloads it -- so the transfer still gets segmentation, resume, throttling and
the queue, which is the entire reason for using a download manager.
"""

from __future__ import annotations

import shutil
import subprocess
from typing import Any


def available() -> dict:
    """Reports whether yt-dlp can be used, and how it was found."""
    try:
        import yt_dlp  # noqa: F401  # imported for the version only

        return {
            "available": True,
            "how": "module",
            "version": getattr(yt_dlp, "__version__", "unknown"),
        }
    except ImportError:
        pass

    binary = shutil.which("yt-dlp") or shutil.which("youtube-dl")
    if binary:
        try:
            version = subprocess.run(
                [binary, "--version"], capture_output=True, text=True, timeout=15
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            version = "unknown"
        return {"available": True, "how": "binary", "path": binary, "version": version}

    return {
        "available": False,
        "reason": (
            "yt-dlp is not installed. Install it with `pip install yt-dlp` or your "
            "package manager to extract media from sites Hydra does not parse itself."
        ),
    }


def extract(url: str, timeout: int = 90) -> dict:
    """Asks yt-dlp where the media for `url` actually lives."""
    status = available()
    if not status["available"]:
        return {"ok": False, "error": status["reason"], "ytdlp": status}

    info = _extract_via_module(url) if status["how"] == "module" else _extract_via_binary(
        status["path"], url, timeout
    )
    if isinstance(info, dict) and info.get("__error__"):
        return {"ok": False, "error": info["__error__"], "ytdlp": status}

    return {"ok": True, "ytdlp": status, **_summarize(info)}


def _extract_via_module(url: str) -> Any:
    import yt_dlp

    options = {
        "quiet": True,
        "no_warnings": True,
        # Extraction only: yt-dlp must not start downloading anything, because
        # the whole point is that Hydra does the transfer.
        "skip_download": True,
        "noplaylist": False,
    }
    try:
        with yt_dlp.YoutubeDL(options) as ydl:
            return ydl.extract_info(url, download=False)
    except Exception as exc:  # noqa: BLE001 - yt-dlp raises many types
        return {"__error__": f"{type(exc).__name__}: {exc}"}


def _extract_via_binary(path: str, url: str, timeout: int) -> Any:
    import json

    try:
        result = subprocess.run(
            [path, "--dump-single-json", "--no-warnings", "--skip-download", url],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return {"__error__": f"yt-dlp timed out after {timeout}s"}
    except OSError as exc:
        return {"__error__": f"cannot run yt-dlp: {exc}"}

    if result.returncode != 0:
        message = (result.stderr or "").strip().splitlines()
        return {"__error__": message[-1] if message else "yt-dlp failed"}
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        return {"__error__": f"yt-dlp returned unreadable JSON: {exc}"}


def _summarize(info: Any) -> dict:
    """Reduces yt-dlp's very large info dict to what Hydra needs."""
    if not isinstance(info, dict):
        return {"entries": []}

    # A playlist comes back with `entries`; a single video does not.
    if info.get("_type") == "playlist" or "entries" in info:
        entries = [_one(entry) for entry in (info.get("entries") or []) if entry]
        return {
            "playlist": True,
            "title": info.get("title"),
            "entries": [entry for entry in entries if entry],
        }
    single = _one(info)
    return {"playlist": False, "entries": [single] if single else []}


def _one(info: dict) -> dict | None:
    if not isinstance(info, dict):
        return None
    formats = []
    for fmt in info.get("formats") or []:
        url = fmt.get("url")
        if not url:
            continue
        formats.append(
            {
                "url": url,
                "formatId": fmt.get("format_id"),
                "extension": fmt.get("ext"),
                "width": fmt.get("width"),
                "height": fmt.get("height"),
                "filesize": fmt.get("filesize") or fmt.get("filesize_approx"),
                "vcodec": fmt.get("vcodec"),
                "acodec": fmt.get("acodec"),
                "protocol": fmt.get("protocol"),
                # A fragmented protocol needs the media grabber rather than a
                # plain ranged transfer, so flag it rather than pretend.
                "streaming": str(fmt.get("protocol") or "").startswith(("m3u8", "http_dash", "f4m")),
                "note": fmt.get("format_note"),
            }
        )

    # Prefer the best progressive format: one URL with both video and audio is
    # something Hydra can download and the user can play, with no muxing.
    best = None
    progressive = [
        f for f in formats
        if not f["streaming"] and f["vcodec"] not in (None, "none") and f["acodec"] not in (None, "none")
    ]
    if progressive:
        best = max(progressive, key=lambda f: (f["height"] or 0, f["filesize"] or 0))
    elif info.get("url"):
        best = {"url": info["url"], "extension": info.get("ext"), "streaming": False}

    return {
        "title": info.get("title"),
        "id": info.get("id"),
        "extractor": info.get("extractor_key") or info.get("extractor"),
        "duration": info.get("duration"),
        "thumbnail": info.get("thumbnail"),
        "webpage": info.get("webpage_url"),
        "best": best,
        "formats": formats,
    }
