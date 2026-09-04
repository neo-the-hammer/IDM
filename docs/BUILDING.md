# Building Hydra

```sh
git clone https://github.com/neo-the-hammer/IDM
cd IDM
cargo build --release
```

That is the whole thing. There is nothing to `npm install` and nothing to
`pip install`, because Hydra has no third-party dependencies at all.

Three binaries land in `target/release/`:

| | |
| --- | --- |
| `hdmd` | The background service that does the downloading |
| `hdm` | The command-line client |
| `hdm-host` | The browser extensions' native messaging host |

## Running it

```sh
./target/release/hdmd
```

It prints where the interface is — `http://127.0.0.1:47113/` by default — and
serves it from the `ui/` directory, which it finds next to the binary, under
`share/hydra/`, or in the source tree.

## What each platform needs

### Linux

Nothing at build time. At run time, OpenSSL 3 (`libssl3`) for HTTPS, which
every desktop distribution already has. It is loaded with `dlopen` at run time
rather than linked, so no `-dev` package is needed to build **or** to run, and
the same binary works against `libssl.so.3` or `libssl.so.1.1`.

### Windows

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

TLS comes from WinHTTP, which Windows already has, so there is no OpenSSL to
find, build or ship.

> The Windows-specific code has never been compiled — see
> [`packaging/windows/README.md`](../packaging/windows/README.md). It is small
> and isolated behind `#[cfg]`, but the first Windows build is a real
> checkpoint.

### macOS

```sh
brew install openssl@3     # macOS has shipped no usable system OpenSSL since Catalina
cargo build --release
```

Hydra looks in Homebrew's and MacPorts' usual locations as well as the standard
library paths. Without one, `hdmd` still starts and says what is missing rather
than failing on the first `https://` link.

## The optional parts

### The interface

Committed already built, so this is only needed after editing it:

```sh
npx tsc -p ui/tsconfig.json
```

Plain TypeScript compiled to ES modules — no framework, no bundler, no
`node_modules`.

### The extraction plugins

Python 3.8 or newer, and nothing beyond its standard library. They power the
site grabber and media extraction; everything else works without them.

```sh
./target/release/hdm plugins    # reports exactly what is and is not available
```

`pip install yt-dlp` adds extraction from the sites Hydra does not parse itself.
Hydra still does the transfer, so those downloads keep segmentation, resume and
the queue.

## Tests

```sh
cargo test --workspace                        # ~250 tests
python3 -m unittest discover -s python/tests  # the plugin layer

# With a daemon running:
node ui/verify.mjs                            # drives the interface in Chromium
node extensions/verify.mjs --url http://…     # drives the extension in Chromium
```

The browser checks need Playwright (`npm install -g playwright`); the Rust and
Python suites need nothing beyond the toolchains themselves and run fully
offline against a local origin server built into the test suite.

## Packaging

| | |
| --- | --- |
| Linux | `packaging/linux/build-deb.sh` builds a `.deb`; there is a systemd **user** unit and a desktop entry beside it |
| Windows | `packaging/windows/install.ps1` installs per-user under `%LOCALAPPDATA%`, no elevation |
| macOS | `packaging/macos/build-app.sh` assembles `Hydra.app` |

The whole `.deb` is about 610 KB, including all three binaries, the interface
and the plugins. That is what an empty dependency tree buys.

## Layout

```
crates/
  hdm-json      JSON, strict, with a nesting limit
  hdm-crypto    MD5, SHA-1, SHA-256, HMAC, base64, CSPRNG
  hdm-net       HTTP/1.1, TLS, FTP, auth, cookies, proxies
  hdm-core      the engine: segments, resume, queues, scheduling
  hdm-api       the local REST and WebSocket server
  hdm-daemon    hdmd
  hdm-cli       hdm
  hdm-host      the browser bridge
  hdm-testserver  a deliberately hostile origin, for tests only
ui/             the interface
python/         the extraction plugins
extensions/     the browser extensions
desktop/        an optional native window
packaging/      per-platform packaging
```
