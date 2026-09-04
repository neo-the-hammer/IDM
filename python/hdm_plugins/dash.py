"""DASH manifests (ISO/IEC 23009-1).

Like HLS, an `.mpd` indexes media rather than being it. Unlike HLS, it is XML
and its segment URLs are usually *templates* to be expanded rather than a list
to be read, which is most of the work here.

Parsing only; the daemon fetches.
"""

from __future__ import annotations

import math
import re
import xml.etree.ElementTree as ET
from urllib.parse import urljoin

#: Refuse to expand more segments than this from a template.
MAX_SEGMENTS = 100_000

_NAMESPACE = re.compile(r"^\{[^}]+\}")


def _tag(element: ET.Element) -> str:
    """The tag without its namespace, which varies between MPD versions."""
    return _NAMESPACE.sub("", element.tag)


def _find(parent: ET.Element, name: str) -> list[ET.Element]:
    return [child for child in parent if _tag(child) == name]


def parse(text: str, url: str) -> dict:
    """Reads an MPD, returning one entry per media stream it offers."""
    try:
        root = ET.fromstring(text)
    except ET.ParseError as exc:
        return {"kind": "invalid", "error": f"not a DASH manifest: {exc}"}
    if _tag(root) != "MPD":
        return {"kind": "invalid", "error": "not a DASH manifest"}

    live = root.get("type", "static") == "dynamic"
    duration = _duration(root.get("mediaPresentationDuration", ""))

    # BaseURL elements nest and each resolves against the one above it.
    base = url
    for element in _find(root, "BaseURL"):
        if element.text:
            base = urljoin(base, element.text.strip())

    streams: list[dict] = []
    for period in _find(root, "Period"):
        period_base = base
        for element in _find(period, "BaseURL"):
            if element.text:
                period_base = urljoin(period_base, element.text.strip())

        for adaptation in _find(period, "AdaptationSet"):
            set_base = period_base
            for element in _find(adaptation, "BaseURL"):
                if element.text:
                    set_base = urljoin(set_base, element.text.strip())

            content_type = adaptation.get("contentType") or _guess_type(adaptation)
            for representation in _find(adaptation, "Representation"):
                stream = _representation(
                    representation, adaptation, set_base, content_type, duration
                )
                if stream:
                    streams.append(stream)

    # Best first, and video before audio, which is the order a person choosing
    # one expects.
    streams.sort(
        key=lambda s: (s["contentType"] != "video", -(s["height"] or 0), -(s["bandwidth"] or 0))
    )
    return {
        "kind": "dash",
        "streams": streams,
        "live": live,
        "duration": duration,
        "encrypted": any(s["encrypted"] for s in streams),
    }


def _representation(
    representation: ET.Element,
    adaptation: ET.Element,
    base: str,
    content_type: str,
    total_duration: float,
) -> dict | None:
    rep_base = base
    for element in _find(representation, "BaseURL"):
        if element.text:
            rep_base = urljoin(rep_base, element.text.strip())

    identifier = representation.get("id", "")
    bandwidth = _int(representation.get("bandwidth"))
    # A Representation inherits geometry and codecs from its AdaptationSet.
    width = _int(representation.get("width") or adaptation.get("width"))
    height = _int(representation.get("height") or adaptation.get("height"))
    codecs = representation.get("codecs") or adaptation.get("codecs") or ""
    mime = representation.get("mimeType") or adaptation.get("mimeType") or ""

    encrypted = bool(
        _find(representation, "ContentProtection") or _find(adaptation, "ContentProtection")
    )

    init, segments, estimated = _segments(
        representation, adaptation, rep_base, identifier, bandwidth, total_duration
    )
    if not segments and not init:
        return None

    return {
        "id": identifier,
        "contentType": content_type,
        "mimeType": mime,
        "codecs": codecs,
        "bandwidth": bandwidth,
        "width": width,
        "height": height,
        "initSegment": init,
        "segments": segments,
        "count": len(segments),
        # True when the count came from arithmetic on the declared duration, so
        # the last segment may not actually exist.
        "estimatedCount": estimated,
        "encrypted": encrypted,
    }


def _segments(
    representation: ET.Element,
    adaptation: ET.Element,
    base: str,
    identifier: str,
    bandwidth: int | None,
    total_duration: float,
) -> tuple[str | None, list[str], bool]:
    """Resolves whichever of the three addressing schemes this stream uses.

    The third element says whether the segment count was *derived* from the
    presentation duration rather than listed, in which case it may be one out.
    """
    # 1. A single self-contained file: no segment addressing at all, so the
    #    BaseURL *is* the media. (A SegmentBase only carries index byte ranges,
    #    so it is the same single file.)
    if not any(
        _find(parent, name)
        for parent in (representation, adaptation)
        for name in ("SegmentList", "SegmentTemplate")
    ):
        return (None, [base], False) if base else (None, [], False)

    # 2. An explicit list.
    for parent in (representation, adaptation):
        for segment_list in _find(parent, "SegmentList"):
            init = None
            for initialization in _find(segment_list, "Initialization"):
                if initialization.get("sourceURL"):
                    init = urljoin(base, initialization.get("sourceURL"))
            urls = [
                urljoin(base, element.get("media"))
                for element in _find(segment_list, "SegmentURL")
                if element.get("media")
            ]
            if urls:
                return init, urls, False

    # 3. A template to expand, which is what most live and large VOD streams use.
    for parent in (representation, adaptation):
        for template in _find(parent, "SegmentTemplate"):
            return _expand_template(template, base, identifier, bandwidth, total_duration)

    return None, [], False


def _expand_template(
    template: ET.Element,
    base: str,
    identifier: str,
    bandwidth: int | None,
    total_duration: float,
) -> tuple[str | None, list[str], bool]:
    init_pattern = template.get("initialization")
    media_pattern = template.get("media")
    if not media_pattern:
        return None, [], False

    substitute = lambda pattern, number=None, time=None: urljoin(  # noqa: E731
        base, _fill(pattern, identifier, bandwidth, number, time)
    )
    init = substitute(init_pattern) if init_pattern else None

    # A SegmentTimeline enumerates durations explicitly, which is exact.
    timelines = _find(template, "SegmentTimeline")
    start_number = _int(template.get("startNumber")) or 1
    urls: list[str] = []

    if timelines:
        number = start_number
        current_time = 0
        for element in _find(timelines[0], "S"):
            if element.get("t") is not None:
                current_time = _int(element.get("t")) or 0
            duration = _int(element.get("d")) or 0
            repeat = _int(element.get("r")) or 0
            for _ in range(repeat + 1):
                if len(urls) >= MAX_SEGMENTS:
                    return init, urls, False
                urls.append(substitute(media_pattern, number, current_time))
                current_time += duration
                number += 1
        return init, urls, False

    # Otherwise the count comes from the total duration over the segment length.
    duration = _int(template.get("duration"))
    timescale = _int(template.get("timescale")) or 1
    if duration and total_duration > 0:
        seconds_each = duration / timescale
        # Round up, because the final segment is usually short: floor would
        # truncate the video by a few seconds. This count is a derivation from
        # a declared duration rather than a list the manifest gave, so the
        # caller is told it is an estimate.
        count = math.ceil(total_duration / seconds_each - 1e-9)
        for index in range(min(count, MAX_SEGMENTS)):
            urls.append(substitute(media_pattern, start_number + index))
        return init, urls, True
    return init, urls, False


def _fill(
    pattern: str, identifier: str, bandwidth: int | None, number: int | None, time: int | None
) -> str:
    """Expands `$RepresentationID$`, `$Number%05d$` and friends."""

    def replace(match: re.Match) -> str:
        name = match.group(1)
        fmt = match.group(2) or ""
        if name == "":
            return "$"  # `$$` is a literal dollar sign.
        value: object
        if name == "RepresentationID":
            value = identifier
        elif name == "Bandwidth":
            value = bandwidth or 0
        elif name == "Number":
            value = number if number is not None else 0
        elif name == "Time":
            value = time if time is not None else 0
        else:
            return match.group(0)
        return ("%" + fmt.lstrip("%")) % value if fmt else str(value)

    return re.sub(r"\$(\w*)(%0\d+d)?\$", replace, pattern)


def _guess_type(adaptation: ET.Element) -> str:
    mime = adaptation.get("mimeType", "")
    if mime.startswith("video"):
        return "video"
    if mime.startswith("audio"):
        return "audio"
    if mime.startswith("text"):
        return "text"
    return "unknown"


def _duration(value: str) -> float:
    """Parses an ISO 8601 duration such as `PT1H2M3.5S`."""
    match = re.fullmatch(
        r"P(?:(\d+)Y)?(?:(\d+)M)?(?:(\d+)D)?"
        r"(?:T(?:(\d+)H)?(?:(\d+)M)?(?:([\d.]+)S)?)?",
        value or "",
    )
    if not match:
        return 0.0
    years, months, days, hours, minutes, seconds = match.groups()
    return (
        float(years or 0) * 31_536_000
        + float(months or 0) * 2_592_000
        + float(days or 0) * 86_400
        + float(hours or 0) * 3_600
        + float(minutes or 0) * 60
        + float(seconds or 0)
    )


def _int(value: str | None) -> int | None:
    try:
        return int(value) if value else None
    except (TypeError, ValueError):
        return None


def _float(value: str | None) -> float:
    try:
        return float(value) if value else 0.0
    except (TypeError, ValueError):
        return 0.0
