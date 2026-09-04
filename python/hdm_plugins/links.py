"""Pulling links out of a page.

`html.parser` rather than a strict XML parser, because real pages are not
well-formed and a site grabber that gives up on the first unclosed tag is
useless. Nothing here fetches anything: the daemon supplies the HTML.
"""

from __future__ import annotations

import re
from html.parser import HTMLParser
from urllib.parse import urljoin, urldefrag, urlparse

#: Attributes that can carry a URL, per element.
URL_ATTRIBUTES = {
    "a": ("href",),
    "area": ("href",),
    "link": ("href",),
    "img": ("src", "data-src", "data-original"),
    "source": ("src", "srcset"),
    "video": ("src", "poster"),
    "audio": ("src"),
    "embed": ("src",),
    "object": ("data",),
    "iframe": ("src",),
    "script": ("src",),
}

#: Elements whose links are page navigation rather than downloadable content.
NAVIGATION_ELEMENTS = {"a", "area"}


class LinkParser(HTMLParser):
    """Collects every URL-bearing attribute, remembering which element it came from."""

    def __init__(self) -> None:
        # convert_charrefs decodes entities, so &amp; in an href becomes &.
        super().__init__(convert_charrefs=True)
        self.found: list[dict] = []
        self.base: str | None = None
        self.title: str | None = None
        self._in_title = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = {name.lower(): (value or "") for name, value in attrs}

        # A <base href> changes what every relative URL on the page means.
        if tag == "base" and values.get("href"):
            self.base = values["href"]
            return
        if tag == "title":
            self._in_title = True
            return

        for attribute in URL_ATTRIBUTES.get(tag, ()):
            raw = values.get(attribute, "")
            if not raw:
                continue
            for url in _split_attribute(attribute, raw):
                self.found.append(
                    {
                        "url": url,
                        "element": tag,
                        "attribute": attribute,
                        # Link text is filled in by handle_data for anchors.
                        "text": values.get("title") or values.get("alt") or "",
                        "navigation": tag in NAVIGATION_ELEMENTS,
                    }
                )

    def handle_endtag(self, tag: str) -> None:
        if tag == "title":
            self._in_title = False

    def handle_data(self, data: str) -> None:
        if self._in_title and self.title is None:
            text = data.strip()
            if text:
                self.title = text
            return
        # Attach anchor text to the most recent anchor, which is what a user
        # recognises a link by.
        text = data.strip()
        if not text or not self.found:
            return
        last = self.found[-1]
        if last["element"] == "a" and not last["text"]:
            last["text"] = text[:200]


def _split_attribute(attribute: str, value: str) -> list[str]:
    """Handles `srcset`, which packs several URLs with descriptors."""
    if attribute != "srcset":
        return [value.strip()]
    out = []
    for candidate in value.split(","):
        url = candidate.strip().split(" ")[0].strip()
        if url:
            out.append(url)
    return out


def extract(html: str, page_url: str) -> dict:
    """Returns every link on the page, resolved against it."""
    parser = LinkParser()
    try:
        parser.feed(html)
        parser.close()
    except Exception:  # noqa: BLE001 - a broken page yields what was parsed so far
        pass

    base = urljoin(page_url, parser.base) if parser.base else page_url
    seen: set[str] = set()
    links: list[dict] = []

    for item in parser.found:
        resolved = _resolve(base, item["url"])
        if resolved is None or resolved in seen:
            continue
        seen.add(resolved)
        links.append(
            {
                "url": resolved,
                "element": item["element"],
                # The attribute matters as much as the element: a <video src>
                # is the media, a <video poster> is a still image of it.
                "attribute": item["attribute"],
                "text": item["text"][:200],
                "navigation": item["navigation"],
                "filename": _filename(resolved),
            }
        )

    return {"links": links, "title": parser.title, "base": base}


def _resolve(base: str, raw: str) -> str | None:
    raw = raw.strip()
    if not raw:
        return None
    # Schemes that name something inside the page rather than a fetchable
    # resource. A crawler following these produces nothing but noise.
    lowered = raw.lower()
    for scheme in ("javascript:", "mailto:", "tel:", "data:", "blob:", "about:", "#"):
        if lowered.startswith(scheme):
            return None
    try:
        resolved, _ = urldefrag(urljoin(base, raw))
    except ValueError:
        return None
    if urlparse(resolved).scheme not in ("http", "https", "ftp", "ftps"):
        return None
    return resolved


def _filename(url: str) -> str:
    path = urlparse(url).path
    name = path.rsplit("/", 1)[-1]
    return name or ""


#: Matches a file extension of one to six characters at the end of a path.
EXTENSION = re.compile(r"\.([A-Za-z0-9]{1,6})$")


def extension_of(url: str) -> str:
    name = _filename(url)
    match = EXTENSION.search(name)
    return match.group(1).lower() if match else ""
