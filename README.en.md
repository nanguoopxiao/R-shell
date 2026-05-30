# Shell

<a href="README.md"><kbd>中文</kbd></a> <kbd>English</kbd>

Shell is a GTK4/gtk-rs based cross-platform terminal client MVP written in Rust.
The project is structured around a lightweight terminal core, protocol adapters, and a GTK4 UI that is enabled explicitly with the `gtk-ui` feature.

## Current MVP Scope

- Rust Cargo workspace with separate core, terminal, platform, protocol, storage, renderer, and app crates.
- CPU-first terminal buffer and renderer architecture.
- Local shell/PTY support using `portable-pty`.
- SSH terminal sessions run an OpenSSH client inside a PTY, so host-key prompts, passwords, and private-key passphrases stay inside the terminal session instead of being stored by the app.
- Windows packages can include a bundled MSYS2-based command environment for common tools such as `curl`, `wget`, `ssh`, and `telnet`.
- Native Telnet byte-stream sessions with basic IAC negotiation and NAWS resize reporting.
- Serial terminal sessions using the `serialport` crate with a simple 8N1-oriented MVP flow.
- GTK4 terminal view resize propagation to LocalShell, SSH, and Telnet sessions.
- GTK4 application behind the `gtk-ui` feature so core crates can be built and tested on machines without GTK4 development libraries.

## Build

Core workspace build without GTK4 system dependencies:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

GTK4 UI build after installing GTK4 development dependencies:

```powershell
cargo run -p shell-app --features gtk-ui
```

Release GUI build:

```powershell
cargo build -p shell-app --release --features gtk-ui
```

`target\release\shell-app.exe` is Cargo's raw build output. It is useful for local development, but on Windows it is not a self-contained GTK app by itself.

On Windows, `target\release\shell-app.exe` is not a self-contained GTK bundle by itself. To build a double-clickable package with the required GTK runtime files, use:

```powershell
.\scripts\package-gtk.ps1
```

To package and immediately verify that the bundled app starts correctly:

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

Then launch:

```text
dist\windows-gtk\bin\shell-app.exe
```

In this workspace, `target` is the compiler output/cache directory and `dist` is the distributable package directory. Keep using `target` for builds and `dist` for the portable bundle you hand to users.

If you need a true single-file `.exe`, the current GTK4 + MSYS2 runtime does not support that packaging model here. GTK requires DLLs plus runtime data under `share` and `lib`, so the supported output is the `dist\windows-gtk` folder, not one standalone executable.

With the local MSYS2 setup used by this workspace:

```powershell
.\scripts\dev-gtk.ps1 check
.\scripts\dev-gtk.ps1 build
.\scripts\dev-gtk.ps1 clippy
.\scripts\dev-gtk.ps1 run
```

## Releases

Package the Windows GTK build locally:

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

The script generates:

```text
dist\windows-gtk\bin\shell-app.exe
```

To upload a package manually to GitHub Releases, zip the distributable folder first:

```powershell
$Version = "v0.1.0"
$Archive = "shell-windows-gtk-$Version.zip"
Compress-Archive -Path "dist\windows-gtk\*" -DestinationPath $Archive -Force
Get-FileHash -Algorithm SHA256 $Archive | Format-List
```

This repository also includes an automated release workflow. Push a `v*` tag and GitHub Actions will build the Windows GTK package, create a zip file plus a `.sha256` checksum, and publish a GitHub Release:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Before publishing a new version, update the workspace version in [Cargo.toml](Cargo.toml), for example from `0.1.0` to `0.1.1`. To test packaging without creating a Release, run the `Release` workflow manually from the GitHub Actions page; manual runs upload a workflow artifact instead of publishing a Release.

## Connection MVP

The GTK app opens a local shell tab on startup. The connection bar supports:

- `Local`: opens another local PTY shell.
- `SSH`: uses `host`, optional `user`, and optional `port`; defaults to port 22.
- `Telnet`: uses `host` and optional `port`; defaults to port 23.
- `Serial`: uses the serial port field and baud-rate field; defaults to 115200 baud.

Saved FTP/SFTP/SSH passwords can be stored securely on Windows and are mirrored into an encrypted local vault so they survive keychain hiccups. Export and import include those saved passwords in encrypted form for the same Windows account. SSH terminal authentication is still delegated to an OpenSSH client running inside the PTY; when a saved password is available, the app replays it to the password prompt.

## Built-In Command Environment

On Windows, the GTK package script copies a portable MSYS2-derived toolchain into:

```text
dist\windows-gtk\tools\msys64
```

When that folder contains the required commands, the app adds a `Shell Tools (Bash)` local terminal entry and can use the bundled `ssh.exe` for SSH terminal sessions. The settings page controls whether the built-in command environment is enabled, whether it is injected into existing local shells, whether SSH sessions prefer the bundled OpenSSH client, and whether bundled commands should take priority over the system `PATH`.

The first required command set is `bash`, `sh`, `curl`, `wget`, `ssh`, and `telnet`. Additional GNU/MSYS2 utilities are made available through the same `PATH` overlay when present in the package. PowerShell keeps its normal alias behavior unless a tool-enabled PowerShell profile is added later, so `Shell Tools (Bash)` is the most predictable first entry for GNU-style commands.

The bundled toolchain is a user-space command environment. It does not install drivers, change global environment variables, or store credentials. Release builds that ship MSYS2 binaries must include the corresponding license files and should refresh the bundled packages regularly for security updates.

## Windows GTK4 Setup

GTK4 development libraries are required only for the `gtk-ui` feature. A typical Windows setup uses MSYS2:

```powershell
winget install MSYS2.MSYS2
```

Then from an MSYS2 MinGW64 shell:

```bash
pacman -S --needed mingw-w64-x86_64-gtk4 mingw-w64-x86_64-pkg-config mingw-w64-x86_64-gcc
```

Add `C:\msys64\mingw64\bin` to `PATH` before building the GTK4 feature, or install MSYS2 locally under `.msys64` and use the helper script:

```powershell
.\scripts\dev-gtk.ps1 check
.\scripts\dev-gtk.ps1 run
```

## Architecture

- `shell-core`: shared models, events, errors, protocol/session types.
- `shell-terminal`: terminal cells, screen buffer, ANSI starter parser, scrollback.
- `shell-platform`: default shell detection and PTY abstraction.
- `shell-protocol`: protocol adapter layer for LocalShell, SSH, Telnet, and Serial.
- `shell-renderer`: UI-independent render snapshots and optional GTK4 terminal view.
- `shell-storage`: connection profile persistence.
- `shell-app`: application entry point and GTK4 UI.

## Memory Budget

- Memory use is treated as a product requirement, not a later tuning pass.
- Hot render paths should avoid full-buffer clones and prefer visible-slice access.
- Scrollback should stay bounded by default; new tabs are expected to keep a modest history budget unless a user-configurable setting is added deliberately.
- On Windows, the current GTK4 runtime pulls in GStreamer-related modules even for this app's idle UI. App-level changes should still minimize shell/terminal memory growth, but further idle-memory reduction will require a leaner GTK runtime choice in addition to Rust-side optimizations.

## Next Protocol Work

- Replace or complement system-SSH mode with a native SSH backend once credential storage and host-key trust flows are designed.
- Add SFTP/FTP file transfer panels after the terminal session model stabilizes.
- Add VNC as an independent framebuffer viewer, not as part of the terminal renderer.
