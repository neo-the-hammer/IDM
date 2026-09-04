# Building and packaging Hydra for Windows

> **Read this first.** The Windows-specific code — `hdm_net::winhttp` and
> `hdm_core::platform`'s Windows branches — was written in a Linux container
> with no Windows Rust target available, so **it has never been compiled**. It
> is deliberately small and confined behind `#[cfg]` so it cannot affect the
> paths that are tested, but the first Windows build is a real checkpoint
> rather than a formality. Please report anything that does not compile or
> behave; that is expected feedback, not a nuisance.

## Building

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

There are no third-party crates to fetch and nothing to `npm install`. TLS
comes from WinHTTP, which every Windows installation already has, so there is
no OpenSSL to find, build or ship.

The three binaries land in `target\x86_64-pc-windows-msvc\release\`:

| Binary | Role |
| --- | --- |
| `hdmd.exe` | The background service that does the downloading |
| `hdm.exe` | The command-line client |
| `hdm-host.exe` | The browser extensions' native messaging host |

### Building the interface

The web UI is committed already built, so this is only needed after editing it:

```powershell
npx tsc -p ui\tsconfig.json
```

## The portable layout

Everything Hydra needs, in one folder that can live on a USB stick:

```
Hydra\
  hdmd.exe
  hdm.exe
  hdm-host.exe
  ui\                     the interface, served on localhost
  python\hdm_plugins\     optional: the site grabber and media extraction
  extensions\             optional: the browser extensions
```

`hdmd.exe` finds `ui\` and `python\` next to itself, so no configuration is
needed. To keep state beside the binaries rather than in `%LOCALAPPDATA%`:

```powershell
hdmd.exe --data-dir .\data --download-dir .\downloads
```

## Installing properly

Hydra needs no installer to work, so the choice is only about integration.

### Where things go

| | |
| --- | --- |
| Program | `%LOCALAPPDATA%\Programs\Hydra` |
| State, settings, token | `%LOCALAPPDATA%\Hydra` |
| Downloads | `%USERPROFILE%\Downloads`, sorted by category |

Installing per-user under `%LOCALAPPDATA%` avoids needing administrator
rights, and means uninstalling touches nothing outside the user's own profile.

### Starting with Windows

```powershell
.\install.ps1
```

That registers `hdmd.exe` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
and the native messaging host for the browsers it finds. HKCU rather than HKLM
throughout: no elevation, and nothing left behind for other users.

### Building an MSI or an installer

Neither is required, and neither is provided as a prebuilt artifact, because
signing is the difficult part and an unsigned installer is worse than a zip.

If you want one, the layout above is all an installer has to place. With
[WiX](https://wixtoolset.org/), one `Component` per binary plus a `Directory`
for `ui\` is enough; with [NSIS](https://nsis.sourceforge.io/), a `File /r` of
the portable folder plus the registry writes `install.ps1` already performs.

## Firewall

Hydra listens on `127.0.0.1` only. Windows Firewall does not prompt for
loopback, so there is nothing to allow. If a prompt does appear, something is
listening on a public interface and that is worth investigating.
