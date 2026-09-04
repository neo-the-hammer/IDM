"""HLS playlists (RFC 8216).

An `.m3u8` is not a video: it is an index of hundreds or thousands of segments.
Handing one to a plain downloader produces a few kilobytes of text named like a
film. This module turns a manifest into the list of things that actually have
to be fetched.

Parsing only. The daemon does the fetching, so cookies, authentication and
proxies behave exactly as they do for any other download.
"""

from __future__ import annotations

from urllib.parse import urljoin

#: A manifest larger than this is not a manifest.
MAX_MANIFEST_BYTES = 8 * 1024 * 1024


def parse_attributes(text: str) -> dict[str, str]:
    """Parses `KEY=value,KEY="quoted,value"` from a tag.

    Splitting on commas naively breaks on `CODECS="avc1.4d401f,mp4a.40.2"`,
    which is present in almost every real master playlist.
    """
    attributes: dict[str, str] = {}
    key = value = ""
    in_key = True
    in_quotes = False

    for char in text:
        if in_key:
            if char == "=":
                in_key = False
            else:
                key += char
        else:
            if char == '"':
                in_quotes = not in_quotes
            elif char == "," and not in_quotes:
                attributes[key.strip()] = value.strip().strip('"')
                key = value = ""
                in_key = True
            else:
                value += char
    if key.strip():
        attributes[key.strip()] = value.strip().strip('"')
    return attributes


def parse(text: str, url: str) -> dict:
    """Reads a playlist, returning either its variants or its segments."""
    if "#EXTM3U" not in text[:200]:
        return {"kind": "invalid", "error": "not an HLS playlist"}

    lines = [line.strip() for line in text.splitlines() if line.strip()]
    # A master playlist lists other playlists; a media playlist lists segments.
    if any(line.startswith("#EXT-X-STREAM-INF") for line in lines):
        return _parse_master(lines, url)
    return _parse_media(lines, url)


def _parse_master(lines: list[str], url: str) -> dict:
    variants = []
    audio_renditions = []
    pending: dict | None = None

    for line in lines:
        if line.startswith("#EXT-X-STREAM-INF:"):
            attributes = parse_attributes(line.split(":", 1)[1])
            resolution = attributes.get("RESOLUTION", "")
            width, _, height = resolution.partition("x")
            pending = {
                "bandwidth": _int(attributes.get("BANDWIDTH")),
                "averageBandwidth": _int(attributes.get("AVERAGE-BANDWIDTH")),
                "width": _int(width),
                "height": _int(height),
                "codecs": attributes.get("CODECS", ""),
                "frameRate": attributes.get("FRAME-RATE", ""),
                # Names the audio group this video should be paired with.
                "audioGroup": attributes.get("AUDIO", ""),
            }
        elif line.startswith("#EXT-X-MEDIA:"):
            attributes = parse_attributes(line.split(":", 1)[1])
            if attributes.get("TYPE") == "AUDIO" and attributes.get("URI"):
                audio_renditions.append(
                    {
                        "url": urljoin(url, attributes["URI"]),
                        "group": attributes.get("GROUP-ID", ""),
                        "name": attributes.get("NAME", ""),
                        "language": attributes.get("LANGUAGE", ""),
                        "default": attributes.get("DEFAULT", "NO") == "YES",
                    }
                )
        elif not line.startswith("#") and pending is not None:
            pending["url"] = urljoin(url, line)
            variants.append(pending)
            pending = None

    # Best first: that is what a person picking one wants at the top.
    variants.sort(key=lambda v: (v["height"] or 0, v["bandwidth"] or 0), reverse=True)
    return {"kind": "master", "variants": variants, "audio": audio_renditions}


def _parse_media(lines: list[str], url: str) -> dict:
    segments: list[dict] = []
    duration = 0.0
    pending_duration = 0.0
    encryption: dict | None = None
    init_segment: str | None = None
    byte_range: str | None = None
    complete = False
    # The sequence number is not decoration: when a playlist gives no IV, the
    # AES-128 IV *is* this number, so getting it wrong yields noise.
    sequence = 0
    for line in lines:
        if line.startswith("#EXT-X-MEDIA-SEQUENCE:"):
            sequence = _int(line.split(":", 1)[1].strip()) or 0
            break

    # Byte-range segments continue from where the previous one ended when the
    # tag gives only a length, so the running offset has to be tracked.
    next_offset = 0

    for line in lines:
        if line.startswith("#EXTINF:"):
            pending_duration = _float(line.split(":", 1)[1].split(",")[0])
        elif line.startswith("#EXT-X-BYTERANGE:"):
            byte_range = line.split(":", 1)[1].strip()
        elif line.startswith("#EXT-X-KEY:"):
            attributes = parse_attributes(line.split(":", 1)[1])
            method = attributes.get("METHOD", "NONE")
            # NONE cancels any previous key, which is how a playlist mixes
            # clear and encrypted sections.
            encryption = (
                None
                if method == "NONE"
                else {
                    "method": method,
                    "uri": urljoin(url, attributes["URI"]) if attributes.get("URI") else None,
                    "iv": attributes.get("IV"),
                }
            )
        elif line.startswith("#EXT-X-MAP:"):
            attributes = parse_attributes(line.split(":", 1)[1])
            if attributes.get("URI"):
                # Fragmented MP4: this header must precede every segment or the
                # result is unplayable.
                init_segment = urljoin(url, attributes["URI"])
        elif line.startswith("#EXT-X-ENDLIST"):
            complete = True
        elif not line.startswith("#"):
            span = None
            if byte_range:
                length, _, offset = byte_range.partition("@")
                start = _int(offset) if offset else next_offset
                size = _int(length) or 0
                span = {"offset": start or 0, "length": size}
                next_offset = (start or 0) + size
            segments.append(
                {
                    "url": urljoin(url, line),
                    "sequence": sequence + len(segments),
                    "duration": pending_duration,
                    "byteRange": span,
                    "encryption": encryption,
                }
            )
            duration += pending_duration
            pending_duration = 0.0
            byte_range = None

    encrypted = [s for s in segments if s["encryption"]]
    return {
        "kind": "media",
        "mediaSequence": sequence,
        "segments": segments,
        "count": len(segments),
        "duration": round(duration, 3),
        "initSegment": init_segment,
        # An unterminated playlist is a live stream, which has no end to
        # download to. Saying so is better than fetching forever.
        "live": not complete,
        "encrypted": bool(encrypted),
        "encryptionMethods": sorted({s["encryption"]["method"] for s in encrypted}),
    }


def _int(value: str | None) -> int | None:
    try:
        return int(value) if value else None
    except ValueError:
        return None


def _float(value: str) -> float:
    try:
        return float(value)
    except ValueError:
        return 0.0
