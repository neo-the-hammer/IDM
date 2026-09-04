# The Hydra desktop shell

A native window around the same interface the daemon already serves. It is
optional: Hydra is fully usable in a browser, and this only changes where the
interface appears, not what it can do.

> **This has never been built.** Tauri and its dependencies could not be fetched
> in the environment Hydra was developed in, so the skeleton here is written
> against Tauri v2's documented API but is unverified. The web interface it
> wraps, by contrast, is tested — so if something here does not compile, nothing
> of substance is lost by using the browser instead.

## Building

```sh
cd desktop
npm install          # fetches the Tauri CLI
npm run tauri build
```

Tauri needs a system webview: WebView2 on Windows (present on Windows 11 and
installable on 10), WebKitGTK on Linux (`libwebkit2gtk-4.1-dev`), and WKWebView
on macOS, which is built in.

## What it does

On launch it looks for a running daemon by reading `daemon.json` from Hydra's
data directory. If there is one it opens a window on it; if not it starts
`hdmd` as a child process first and shuts it down again on exit.

That is the whole shell. The interface is the same TypeScript and CSS served
over loopback, so there is exactly one implementation to maintain and a fix
made for the browser is a fix here too.

## Why the interface is not bundled

Tauri can serve assets from inside the binary, and that would remove the
loopback round trip. It would also mean two copies of the interface — one
compiled in, one on disk for browser users — that could drift apart.

Serving both from the daemon keeps them identical. The cost is a local HTTP
request the operating system never puts on a wire.
