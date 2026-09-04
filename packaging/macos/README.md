# Building and packaging Hydra for macOS

```sh
cargo build --release
```

TLS comes from OpenSSL, loaded at run time. macOS has not shipped a usable
system OpenSSL since Catalina, so install one:

```sh
brew install openssl@3
```

Hydra looks in Homebrew's and MacPorts' usual locations as well as the standard
library paths, so no configuration is needed once it is installed. On a machine
without it, `hdmd` starts and says exactly what is missing rather than failing
on the first `https://` link.

## An application bundle

`./build-app.sh` assembles `Hydra.app`:

```
Hydra.app/Contents/
  Info.plist
  MacOS/Hydra          a launcher that starts hdmd and opens the interface
  MacOS/hdmd hdm hdm-host
  Resources/ui/        the interface
  Resources/python/    the site grabber and media extraction
```

Double-clicking it starts the daemon and opens the interface in the default
browser.

## Signing and notarization

The bundle is unsigned, so Gatekeeper will refuse it on first launch:
right-click, Open, and confirm once.

Signing needs an Apple Developer account, which is why no signed build is
provided here. With one:

```sh
codesign --deep --force --options runtime --sign "Developer ID Application: YOUR NAME" Hydra.app
xcrun notarytool submit Hydra.zip --apple-id you@example.com --team-id TEAMID --wait
xcrun stapler staple Hydra.app
```

## Starting at login

```sh
cp com.hydradm.daemon.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.hydradm.daemon.plist
```

A LaunchAgent rather than a LaunchDaemon: Hydra needs exactly the permissions
of the person using it — writing to their Downloads folder — and running it
system-wide would give it far more reach than the job requires.

## Where things go

| | |
| --- | --- |
| State, settings, token | `~/Library/Application Support/Hydra` |
| Downloads | `~/Downloads`, sorted by category |
