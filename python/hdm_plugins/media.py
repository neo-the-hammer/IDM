"""Finding playable media on a page.

The distinction that matters is between a plain file and a streaming manifest.
An `.mp4` can simply be downloaded; an `.m3u8` or `.mpd` is an index of
thousands of segments, and offering it as though it were a file would hand the
user a few kilobytes of text named like a video.
"""

from __future__ import annotations

from urllib.parse import urlparse

from .links import extension_of, extract

#: Extensions that are the media itself.
DIRECT_MEDIA = {
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "flv", "wmv", "mpg", "mpeg", "ogv", "3gp", "ts",
    "mp3", "m4a", "aac", "flac", "wav", "opus", "oga", "ogg", "wma", "aiff",
}

#: Extensions that index media rather than being it.
STREAMING_MANIFESTS = {"m3u8": "hls", "mpd": "dash", "f4m": "hds", "ism": "smooth"}

#: Attributes that reference an image rather than the media itself.
NON_MEDIA_ATTRIBUTES = {"poster"}


def find(html: str, page_url: str) -> dict:
    """Returns the media referenced by a page."""
    parsed = extract(html, page_url)
    items: list[dict] = []
    seen: set[str] = set()

    for link in parsed["links"]:
        url = link["url"]
        if url in seen:
            continue
        extension = extension_of(url)
        kind = classify(url, extension, link["element"], link.get("attribute", ""))
        if kind is None:
            continue
        seen.add(url)
        items.append(
            {
                "url": url,
                "kind": kind,
                "streaming": kind in ("hls", "dash", "hds", "smooth"),
                "extension": extension,
                "element": link["element"],
                "title": link["text"] or parsed["title"] or link["filename"],
                "filename": link["filename"],
            }
        )

    # Manifests first: when a page offers both, the stream is usually the real
    # content and the file is a trailer or a fallback.
    items.sort(key=lambda item: (not item["streaming"], item["url"]))
    return {"media": items, "title": parsed["title"]}


def classify(url: str, extension: str, element: str, attribute: str = "") -> str | None:
    # A poster frame is a picture of the video, not the video.
    if attribute in NON_MEDIA_ATTRIBUTES:
        return None
    if extension in STREAMING_MANIFESTS:
        return STREAMING_MANIFESTS[extension]
    if extension in DIRECT_MEDIA:
        return "audio" if extension in {
            "mp3", "m4a", "aac", "flac", "wav", "opus", "oga", "ogg", "wma", "aiff"
        } else "video"
    # A <video>/<audio>/<source> element is media even when the URL is opaque,
    # which is common for signed CDN links with no extension at all.
    if element in ("video", "audio", "source"):
        return "video" if element != "audio" else "audio"
    # Some CDNs put the format in the path rather than an extension.
    path = urlparse(url).path.lower()
    if "/manifest" in path or path.endswith("/master") or "playlist" in path:
        return "hls" if "m3u8" in url.lower() else None
    return None
