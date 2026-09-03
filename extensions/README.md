# The Hydra browser extension

Takes a download away from the browser and hands it to Hydra, with the session
context that made the link work in the first place. This is the piece that makes
a download manager feel like one.

`chromium/` is the source of truth. `firefox/` is generated from it by
`./build-firefox.sh` — the two differ only in the manifest.

## Installing

### Chromium, Chrome, Edge, Brave

1. Open `chrome://extensions` and turn on **Developer mode**.
2. **Load unpacked**, and choose `extensions/chromium`.
3. Copy the extension's id from the card that appears.
4. Register the native messaging host so the extension can find Hydra by itself:

   ```sh
   packaging/native-host/install.sh "" <extension-id>          # Linux, macOS
   packaging\native-host\install.ps1 -ExtensionIds <id>        # Windows
   ```

5. Restart the browser.

### Firefox

1. Run `./build-firefox.sh`.
2. Open `about:debugging#/runtime/this-firefox` → **Load Temporary Add-on**, and
   choose `extensions/firefox/manifest.json`.
3. Register the host as above; Firefox is identified by the add-on id in its
   manifest, so no extension id is needed.

Without the native host the extension still works — open its options page and
paste the address and token from `daemon.json` in Hydra's data folder.

## What it does

**Capture.** `chrome.downloads.onCreated` fires when the browser starts a
download; the extension confirms Hydra is reachable, hands the download over,
and only then cancels the browser's copy and removes the partial file. Doing it
in that order matters: cancelling first and failing to hand off would leave the
user with nothing.

**Session context.** A great many download links are only valid for the session
that produced them. The extension forwards the page's cookies, the referring
page, and the browser's own user-agent string, so Hydra fetches the file rather
than a login page.

**Context menu.** Right-click any link, image, video or audio element and send
it straight to Hydra.

**Media sniffing.** Responses with a video or audio content type are noted per
tab and offered in the popup. Streaming manifests (HLS, DASH) are labelled as
such rather than presented as ordinary files, because segmented media needs the
media grabber rather than a plain transfer.

**Filters.** Capture can be limited by file extension, by minimum size, or
turned off for particular sites — and page types the browser handles inline
(`.html`, `.css`, `.js` and friends) are never captured.

## About the permissions

`<all_urls>` and `cookies` look broad, and they are. They are what allows the
extension to read the cookies for a download's own URL, which is the difference
between a working capture and a downloaded login page. The extension sends
cookies to one place — the Hydra daemon on loopback — and nowhere else.

`webRequest` is used only to observe response headers for media sniffing; no
request is ever blocked or modified.

## Verifying it

With a daemon running and a URL that serves a file:

```sh
node extensions/verify.mjs --url http://127.0.0.1:8000/some-file.bin
```

It loads the extension into a real Chromium, pairs it, seeds a cookie, triggers
a download, and asserts that Hydra received it, that the cookie, referer and
user-agent came with it, and that the browser kept no copy of its own.
