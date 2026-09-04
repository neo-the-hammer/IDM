"""Tests for the extraction plugins.

Run with: python3 -m unittest discover python/tests
"""

import io
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from hdm_plugins import dash, hls, links, media, protocol  # noqa: E402
from hdm_plugins.__main__ import HANDLERS  # noqa: E402


class TestLinkExtraction(unittest.TestCase):
    def extract(self, html, url="http://example.com/dir/page.html"):
        return links.extract(html, url)

    def urls(self, html, url="http://example.com/dir/page.html"):
        return [link["url"] for link in self.extract(html, url)["links"]]

    def test_resolves_relative_urls(self):
        found = self.urls('<a href="a.zip">a</a><a href="../b.zip">b</a><a href="/c.zip">c</a>')
        self.assertEqual(
            found,
            [
                "http://example.com/dir/a.zip",
                "http://example.com/b.zip",
                "http://example.com/c.zip",
            ],
        )

    def test_base_href_changes_what_relative_means(self):
        html = '<base href="http://cdn.example.com/v1/"><a href="a.zip">a</a>'
        self.assertEqual(self.urls(html), ["http://cdn.example.com/v1/a.zip"])

    def test_drops_schemes_that_are_not_fetchable(self):
        html = """<a href="javascript:go()">j</a><a href="mailto:a@b.c">m</a>
                  <a href="tel:+1">t</a><a href="data:text/plain,x">d</a>
                  <a href="#section">h</a><a href="real.zip">r</a>"""
        self.assertEqual(self.urls(html), ["http://example.com/dir/real.zip"])

    def test_strips_fragments_so_one_page_is_visited_once(self):
        html = '<a href="p.html#a">1</a><a href="p.html#b">2</a>'
        # Both anchors point at the same page; a crawler must see one URL.
        self.assertEqual(self.urls(html), ["http://example.com/dir/p.html"])

    def test_decodes_entities_in_attributes(self):
        html = '<a href="get?a=1&amp;b=2">x</a>'
        self.assertEqual(self.urls(html), ["http://example.com/dir/get?a=1&b=2"])

    def test_splits_srcset(self):
        html = '<source srcset="small.mp4 480w, large.mp4 1080w">'
        self.assertEqual(
            self.urls(html),
            ["http://example.com/dir/small.mp4", "http://example.com/dir/large.mp4"],
        )

    def test_keeps_anchor_text_as_a_title(self):
        result = self.extract('<a href="f.zip">The Manual</a>')
        self.assertEqual(result["links"][0]["text"], "The Manual")

    def test_records_the_attribute_a_url_came_from(self):
        result = self.extract('<video src="v.mp4" poster="p.jpg"></video>')
        attributes = {link["attribute"] for link in result["links"]}
        self.assertEqual(attributes, {"src", "poster"})

    def test_survives_broken_html(self):
        # Real pages are not well-formed; giving up on the first unclosed tag
        # would make the site grabber useless.
        html = '<div><a href="a.zip">a<p><span>unclosed<a href="b.zip">b'
        self.assertEqual(
            self.urls(html),
            ["http://example.com/dir/a.zip", "http://example.com/dir/b.zip"],
        )

    def test_deduplicates(self):
        html = '<a href="a.zip">1</a><a href="a.zip">2</a><a href="./a.zip">3</a>'
        self.assertEqual(len(self.urls(html)), 1)

    def test_reads_the_title(self):
        self.assertEqual(self.extract("<title>Hello</title>")["title"], "Hello")

    def test_marks_navigation_separately_from_content(self):
        result = self.extract('<a href="p.html">page</a><img src="i.png">')
        by_url = {link["url"]: link["navigation"] for link in result["links"]}
        self.assertTrue(by_url["http://example.com/dir/p.html"])
        self.assertFalse(by_url["http://example.com/dir/i.png"])


class TestMediaDetection(unittest.TestCase):
    def find(self, html, url="http://example.com/watch/"):
        return media.find(html, url)["media"]

    def test_recognizes_direct_media(self):
        items = self.find('<a href="clip.mp4">v</a><a href="song.flac">a</a>')
        kinds = {item["url"].rsplit("/", 1)[-1]: item["kind"] for item in items}
        self.assertEqual(kinds["clip.mp4"], "video")
        self.assertEqual(kinds["song.flac"], "audio")

    def test_flags_streaming_manifests(self):
        for name, kind in [("a.m3u8", "hls"), ("b.mpd", "dash")]:
            item = self.find(f'<a href="{name}">s</a>')[0]
            self.assertEqual(item["kind"], kind)
            self.assertTrue(item["streaming"], f"{name} indexes media rather than being it")

    def test_a_poster_frame_is_not_media(self):
        items = self.find('<video src="v.mp4" poster="thumb.jpg"></video>')
        names = [item["url"].rsplit("/", 1)[-1] for item in items]
        self.assertIn("v.mp4", names)
        self.assertNotIn("thumb.jpg", names)

    def test_media_elements_count_even_without_an_extension(self):
        # Signed CDN links routinely have no extension at all.
        items = self.find('<video src="/stream/abc123?sig=x"></video>')
        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["kind"], "video")

    def test_streams_are_listed_first(self):
        items = self.find('<a href="trailer.mp4">t</a><a href="full.m3u8">f</a>')
        self.assertTrue(items[0]["streaming"], "the real content is usually the stream")

    def test_ignores_ordinary_links(self):
        self.assertEqual(self.find('<a href="page.html">p</a><a href="doc.pdf">d</a>'), [])


class TestProtocol(unittest.TestCase):
    def run_host(self, lines):
        stdin = io.StringIO("\n".join(lines) + "\n")
        stdout = io.StringIO()
        protocol.serve(HANDLERS, stdin=stdin, stdout=stdout)
        return [json.loads(line) for line in stdout.getvalue().splitlines() if line]

    def test_answers_each_request_in_order(self):
        replies = self.run_host(
            [
                json.dumps({"action": "ping", "id": 1}),
                json.dumps({"action": "capabilities", "id": 2}),
            ]
        )
        self.assertEqual([r["id"] for r in replies], [1, 2])
        self.assertTrue(all(r["ok"] for r in replies))

    def test_a_bad_request_is_a_reply_not_a_crash(self):
        replies = self.run_host(
            [
                "not json at all",
                json.dumps({"action": "unknown"}),
                json.dumps({"action": "links"}),
                json.dumps(["not", "an", "object"]),
                # A valid request after the failures must still be answered,
                # or one bad page would end the whole crawl.
                json.dumps({"action": "ping"}),
            ]
        )
        self.assertEqual([r["ok"] for r in replies], [False, False, False, False, True])
        for reply in replies[:4]:
            self.assertIn("error", reply)

    def test_a_handler_that_raises_is_reported(self):
        def explode(_request):
            raise RuntimeError("boom")

        stdin = io.StringIO(json.dumps({"action": "explode"}) + "\n")
        stdout = io.StringIO()
        # stderr carries the traceback; the caller gets a usable reply.
        protocol.serve({"explode": explode}, stdin=stdin, stdout=stdout)
        reply = json.loads(stdout.getvalue())
        self.assertFalse(reply["ok"])
        self.assertIn("boom", reply["error"])

    def test_replies_are_single_lines(self):
        # Embedded newlines would break the framing.
        replies = self.run_host([json.dumps({"action": "links", "url": "http://a/",
                                             "html": "<a href='x.zip'>a\nb</a>"})])
        self.assertEqual(len(replies), 1)
        self.assertTrue(replies[0]["ok"])

    def test_blank_lines_are_ignored(self):
        stdin = io.StringIO("\n\n" + json.dumps({"action": "ping"}) + "\n\n")
        stdout = io.StringIO()
        protocol.serve(HANDLERS, stdin=stdin, stdout=stdout)
        self.assertEqual(len(stdout.getvalue().splitlines()), 1)


class TestHls(unittest.TestCase):
    BASE = "https://cdn.example.com/v/index.m3u8"

    def test_attribute_values_may_contain_commas(self):
        attributes = hls.parse_attributes(
            'BANDWIDTH=4000000,CODECS="avc1.4d401f,mp4a.40.2",RESOLUTION=1920x1080'
        )
        # Splitting on every comma truncates the codec list, which is present
        # in essentially every real master playlist.
        self.assertEqual(attributes["CODECS"], "avc1.4d401f,mp4a.40.2")
        self.assertEqual(attributes["RESOLUTION"], "1920x1080")
        self.assertEqual(attributes["BANDWIDTH"], "4000000")

    def test_master_playlist_lists_variants_best_first(self):
        text = (
            "#EXTM3U\n"
            '#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="a",NAME="English",'
            'LANGUAGE="en",DEFAULT=YES,URI="audio/en.m3u8"\n'
            "#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360\n"
            "low.m3u8\n"
            '#EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1920x1080,AUDIO="a"\n'
            "high.m3u8\n"
        )
        parsed = hls.parse(text, self.BASE)
        self.assertEqual(parsed["kind"], "master")
        self.assertEqual([v["height"] for v in parsed["variants"]], [1080, 360])
        self.assertEqual(parsed["variants"][0]["url"], "https://cdn.example.com/v/high.m3u8")
        self.assertEqual(parsed["audio"][0]["language"], "en")
        self.assertEqual(parsed["audio"][0]["url"], "https://cdn.example.com/v/audio/en.m3u8")

    def test_media_playlist_returns_segments_and_duration(self):
        text = (
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n"
            "#EXTINF:5.0,\na.ts\n#EXTINF:5.0,\nb.ts\n#EXTINF:2.5,\nc.ts\n"
            "#EXT-X-ENDLIST\n"
        )
        parsed = hls.parse(text, self.BASE)
        self.assertEqual(parsed["count"], 3)
        self.assertEqual(parsed["duration"], 12.5)
        self.assertFalse(parsed["live"])
        self.assertFalse(parsed["encrypted"])
        self.assertEqual(parsed["segments"][2]["url"], "https://cdn.example.com/v/c.ts")

    def test_a_playlist_without_an_endlist_is_live(self):
        parsed = hls.parse("#EXTM3U\n#EXTINF:4.0,\na.ts\n", self.BASE)
        self.assertTrue(parsed["live"])

    def test_sequence_numbers_start_at_the_media_sequence(self):
        text = "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:97\n#EXTINF:4,\na.ts\n#EXTINF:4,\nb.ts\n"
        parsed = hls.parse(text, self.BASE)
        # With no explicit IV the sequence number *is* the AES IV, so an
        # off-by-one here decrypts to noise rather than to an error.
        self.assertEqual([s["sequence"] for s in parsed["segments"]], [97, 98])

    def test_a_key_of_none_cancels_the_previous_one(self):
        text = (
            "#EXTM3U\n"
            '#EXT-X-KEY:METHOD=AES-128,URI="k.bin",IV=0x0f0e0d0c0b0a09080706050403020100\n'
            "#EXTINF:4,\na.ts\n"
            "#EXT-X-KEY:METHOD=NONE\n"
            "#EXTINF:4,\nb.ts\n#EXT-X-ENDLIST\n"
        )
        parsed = hls.parse(text, self.BASE)
        self.assertEqual(parsed["segments"][0]["encryption"]["method"], "AES-128")
        self.assertEqual(
            parsed["segments"][0]["encryption"]["uri"], "https://cdn.example.com/v/k.bin"
        )
        self.assertIsNone(parsed["segments"][1]["encryption"])
        self.assertEqual(parsed["encryptionMethods"], ["AES-128"])

    def test_byte_ranges_continue_from_the_previous_segment(self):
        text = (
            "#EXTM3U\n"
            "#EXT-X-BYTERANGE:1000@0\n#EXTINF:4,\nall.ts\n"
            "#EXT-X-BYTERANGE:2000\n#EXTINF:4,\nall.ts\n#EXT-X-ENDLIST\n"
        )
        segments = hls.parse(text, self.BASE)["segments"]
        # A tag with no offset means "immediately after the last one".
        self.assertEqual(segments[0]["byteRange"], {"offset": 0, "length": 1000})
        self.assertEqual(segments[1]["byteRange"], {"offset": 1000, "length": 2000})

    def test_an_init_segment_is_reported(self):
        text = '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:4,\na.m4s\n#EXT-X-ENDLIST\n'
        self.assertEqual(
            hls.parse(text, self.BASE)["initSegment"], "https://cdn.example.com/v/init.mp4"
        )

    def test_something_that_is_not_a_playlist_is_rejected(self):
        self.assertEqual(hls.parse("<html></html>", self.BASE)["kind"], "invalid")


class TestDash(unittest.TestCase):
    BASE = "https://cdn.example.com/v/stream.mpd"

    def parse(self, body, attributes=''):
        text = (
            '<?xml version="1.0"?>'
            '<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" '
            f'mediaPresentationDuration="PT12S" {attributes}>{body}</MPD>'
        )
        return dash.parse(text, self.BASE)

    def test_namespaces_do_not_hide_the_elements(self):
        parsed = self.parse(
            '<Period><AdaptationSet contentType="video" mimeType="video/mp4">'
            '<Representation id="v0" bandwidth="900000" width="1280" height="720">'
            '<SegmentList><Initialization sourceURL="init.mp4"/>'
            '<SegmentURL media="1.m4s"/><SegmentURL media="2.m4s"/>'
            "</SegmentList></Representation></AdaptationSet></Period>"
        )
        self.assertEqual(parsed["kind"], "dash")
        stream = parsed["streams"][0]
        self.assertEqual(stream["height"], 720)
        self.assertEqual(stream["initSegment"], "https://cdn.example.com/v/init.mp4")
        self.assertEqual(stream["segments"][1], "https://cdn.example.com/v/2.m4s")
        self.assertFalse(stream["estimatedCount"])

    def test_number_templates_are_expanded_and_zero_padded(self):
        parsed = self.parse(
            '<Period><AdaptationSet contentType="video" mimeType="video/mp4">'
            '<SegmentTemplate initialization="$RepresentationID$/i.mp4" '
            'media="$RepresentationID$/s-$Number%04d$.m4s" duration="4" '
            'timescale="1" startNumber="1"/>'
            '<Representation id="v0" bandwidth="900000" width="1280" height="720"/>'
            "</AdaptationSet></Period>"
        )
        stream = parsed["streams"][0]
        self.assertEqual(stream["initSegment"], "https://cdn.example.com/v/v0/i.mp4")
        self.assertEqual(stream["count"], 3)
        self.assertEqual(stream["segments"][0], "https://cdn.example.com/v/v0/s-0001.m4s")
        self.assertEqual(stream["segments"][2], "https://cdn.example.com/v/v0/s-0003.m4s")
        # A count derived from the duration may be one out, and says so.
        self.assertTrue(stream["estimatedCount"])

    def test_a_segment_timeline_is_exact_rather_than_derived(self):
        parsed = self.parse(
            '<Period><AdaptationSet contentType="video" mimeType="video/mp4">'
            '<SegmentTemplate media="s-$Time$.m4s" timescale="1000">'
            '<SegmentTimeline><S t="0" d="4000" r="1"/><S d="2000"/></SegmentTimeline>'
            "</SegmentTemplate>"
            '<Representation id="v0" bandwidth="900000"/>'
            "</AdaptationSet></Period>"
        )
        stream = parsed["streams"][0]
        self.assertFalse(stream["estimatedCount"])
        self.assertEqual(
            stream["segments"],
            [
                "https://cdn.example.com/v/s-0.m4s",
                "https://cdn.example.com/v/s-4000.m4s",
                "https://cdn.example.com/v/s-8000.m4s",
            ],
        )

    def test_base_urls_nest(self):
        parsed = self.parse(
            "<BaseURL>https://a.example.com/root/</BaseURL>"
            "<Period><BaseURL>period/</BaseURL>"
            '<AdaptationSet contentType="video" mimeType="video/mp4">'
            '<Representation id="v0" bandwidth="1"><BaseURL>file.mp4</BaseURL>'
            "</Representation></AdaptationSet></Period>"
        )
        self.assertEqual(
            parsed["streams"][0]["segments"],
            ["https://a.example.com/root/period/file.mp4"],
        )

    def test_video_sorts_before_audio_and_best_first(self):
        parsed = self.parse(
            '<Period><AdaptationSet contentType="audio" mimeType="audio/mp4">'
            '<Representation id="a0" bandwidth="128000"><BaseURL>a.m4a</BaseURL>'
            "</Representation></AdaptationSet>"
            '<AdaptationSet contentType="video" mimeType="video/mp4">'
            '<Representation id="v0" bandwidth="500000" height="360">'
            "<BaseURL>low.mp4</BaseURL></Representation>"
            '<Representation id="v1" bandwidth="900000" height="1080">'
            "<BaseURL>high.mp4</BaseURL></Representation>"
            "</AdaptationSet></Period>"
        )
        self.assertEqual([s["id"] for s in parsed["streams"]], ["v1", "v0", "a0"])

    def test_drm_is_reported_rather_than_downloaded_in_vain(self):
        parsed = self.parse(
            '<Period><AdaptationSet contentType="video" mimeType="video/mp4">'
            '<ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011"/>'
            '<Representation id="v0" bandwidth="1"><BaseURL>f.mp4</BaseURL>'
            "</Representation></AdaptationSet></Period>"
        )
        self.assertTrue(parsed["encrypted"])

    def test_iso_8601_durations(self):
        self.assertEqual(dash._duration("PT1H2M3.5S"), 3723.5)
        self.assertEqual(dash._duration("PT30S"), 30.0)
        self.assertEqual(dash._duration("nonsense"), 0.0)

    def test_something_that_is_not_a_manifest_is_rejected(self):
        self.assertEqual(dash.parse("not xml at all", self.BASE)["kind"], "invalid")
        self.assertEqual(dash.parse("<html></html>", self.BASE)["kind"], "invalid")


class TestManifestDispatch(unittest.TestCase):
    """The daemon usually cannot tell HLS from DASH by the URL alone."""

    def dispatch(self, text):
        return HANDLERS["manifest"]({"text": text, "url": "https://x.example.com/a"})

    def test_playlists_and_manifests_are_told_apart_by_their_bytes(self):
        self.assertEqual(self.dispatch("#EXTM3U\n#EXTINF:4,\na.ts\n")["kind"], "media")
        self.assertEqual(
            self.dispatch('<?xml version="1.0"?><MPD type="static"><Period/></MPD>')["kind"],
            "dash",
        )

    def test_anything_else_is_declined_with_a_reason(self):
        reply = self.dispatch("just some text")
        self.assertFalse(reply["ok"])
        self.assertIn("HLS", reply["error"])


class TestYtDlpBridge(unittest.TestCase):
    def test_absence_is_described_usefully(self):
        from hdm_plugins import ytdlp

        status = ytdlp.available()
        self.assertIn("available", status)
        if not status["available"]:
            self.assertIn("yt-dlp", status["reason"])
            self.assertIn("install", status["reason"].lower())
            # Extraction must decline with the same explanation rather than
            # raising something obscure.
            result = ytdlp.extract("https://example.com/watch")
            self.assertFalse(result["ok"])
            self.assertIn("yt-dlp", result["error"])


if __name__ == "__main__":
    unittest.main()
