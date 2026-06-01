# R-shell

<a href="README.md"><kbd>中文</kbd></a> <kbd>English</kbd>

R-shell is a cross-platform terminal client built with Rust and GTK4. It aims to be more than a local shell window: local terminals, remote sessions, a bundled command environment, and file-transfer workflows are being brought together into one lightweight, maintainable desktop tool.

The project is still in the MVP stage, but it already has a runnable Windows GTK package, local PTY terminals, SSH/Telnet/Serial sessions, SFTP/FTP-related capabilities, and a bundled command environment for common tools.

## Who It Is For

- Users who want a lightweight desktop terminal with local, SSH, Telnet, and serial entry points in one place.
- Windows users who often miss commands such as `curl`, `wget`, `ssh`, or `telnet`, and want an app that can bring its own command toolchain.
- Developers interested in how Rust, GTK4, PTYs, terminal rendering, and protocol adapters can be separated cleanly.

## Features

- Local shell/PTY: starts and manages local terminal sessions through `portable-pty`.
- SSH terminal: runs an OpenSSH client inside a PTY, keeping authentication prompts in the terminal stream.
- Telnet: native byte-stream sessions with basic IAC negotiation and NAWS resize reporting.
- Serial: terminal sessions using the `serialport` crate, currently focused on common 8N1 scenarios.
- SFTP/FTP: connection configuration, file listing, and transfer-panel related implementation.
- Built-in command environment: Windows packages can include `bash`, `curl`, `wget`, `ssh`, `telnet`, and related utilities.
- Connection storage: saved FTP/SFTP/SSH passwords can be stored securely on Windows and mirrored into an encrypted local vault.
- CPU-first rendering: the terminal core is independent from GTK, and hot render paths prefer visible-slice access over full-buffer copies.

## Download And Run

Download the Windows GTK package from GitHub Releases:

```text
R-shell-windows-gtk-v0.1.0.zip
```

Extract it and run the executable at the package root:

```text
R-shell.exe
```

The package is not a single-file exe. GTK4 requires bundled DLLs plus runtime data under `share` and `lib`, so keep the extracted directory structure intact.

## Built-In Command Environment

The Windows package carries a slim MSYS2-derived command toolchain under:

```text
tools\msys64
```

When that directory exists, R-shell adds a `Shell Tools (Bash)` local terminal entry and can prefer the bundled `ssh.exe` for SSH sessions. The settings page controls whether the built-in command environment is enabled, whether it is injected into existing local shells, and whether bundled commands take priority over the system `PATH`.

This toolchain only affects sessions launched by R-shell. It does not install drivers, modify global environment variables, or store credentials.

## Project Status

R-shell is an early MVP. The core architecture is split into independent crates and the main connection paths are runnable, but UI polish, native SSH support, file-transfer details, cross-platform packaging, and long-term stability are still evolving.

Current priorities:

- Polish the Windows release package experience and size.
- Stabilize local terminal, SSH, Telnet, and Serial session behavior.
- Improve SFTP/FTP file-transfer workflows.
- Keep the terminal core independent from GTK for headless testing.
- Keep memory usage bounded and avoid hot-path full-buffer cloning.

## Run From Source

Core workspace build without GTK4 system dependencies:

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

Run the GUI after installing GTK4 development dependencies:

```powershell
cargo run -p shell-app --features gtk-ui
```

Build the release GUI:

```powershell
cargo build -p shell-app --release --features gtk-ui
```

`target\release\R-shell.exe` is Cargo's raw build output and is useful for local development. Use the packaging script for a distributable Windows bundle.

## Windows GTK4 Development Setup

On Windows, MSYS2 is the usual way to install GTK4 development libraries:

```powershell
winget install MSYS2.MSYS2
```

Then install dependencies from an MSYS2 MinGW64 shell:

```bash
pacman -S --needed mingw-w64-x86_64-gtk4 mingw-w64-x86_64-pkg-config mingw-w64-x86_64-gcc
```

Before building the GTK4 feature, add `C:\msys64\mingw64\bin` to `PATH`; alternatively, install MSYS2 under the repo-local `.msys64` directory and use the helper script:

```powershell
.\scripts\dev-gtk.ps1 check
.\scripts\dev-gtk.ps1 build
.\scripts\dev-gtk.ps1 clippy
.\scripts\dev-gtk.ps1 run
```

## Packaging

Build the local Windows GTK package:

```powershell
.\scripts\package-gtk.ps1
```

Package and immediately verify that the app starts:

```powershell
.\scripts\package-gtk.ps1 -SmokeTest
```

The script generates:

```text
dist\windows-gtk\R-shell.exe
```

Create a release archive manually:

```powershell
$Version = "v0.1.0"
$Archive = "R-shell-windows-gtk-$Version.zip"
Compress-Archive -Path "dist\windows-gtk\*" -DestinationPath $Archive -CompressionLevel Optimal -Force
Get-FileHash -Algorithm SHA256 $Archive | Format-List
```

The repository also includes an automated release workflow. Push a `v*` tag and GitHub Actions will build the Windows GTK package, create a zip plus a `.sha256` checksum, and publish a GitHub Release. The release body automatically includes a commit summary from the previous tag to the current tag, followed by GitHub's generated release notes. See [docs/release-process.md](docs/release-process.md) for the full workflow.

Recommended release flow:

```powershell
git status --short
git tag v0.1.1
git push origin main --tags
```

For a more polished product-style announcement, edit the generated GitHub Release body afterward and group the notes into sections such as Added, Fixed, and Known Issues.

## Architecture

- `shell-core`: shared models, events, errors, and protocol/session types.
- `shell-terminal`: terminal cells, ANSI/VT parsing, screen buffer, and scrollback.
- `shell-platform`: default shell detection, PTY abstraction, and bundled toolchain discovery.
- `shell-protocol`: protocol adapter layer for LocalShell, SSH, Telnet, Serial, FTP, and SFTP.
- `shell-renderer`: UI-independent render snapshots and optional GTK4 terminal view.
- `shell-storage`: profile, settings, and secret persistence.
- `shell-app`: application entry point, GTK4 UI, session pages, and settings.

The GTK UI entry point for `shell-app` is `crates/app/src/gtk_app.rs`; larger UI areas are split
under `crates/app/src/gtk_app/`: `session_tabs.rs` owns the custom tab strip,
`sftp_ui.rs` owns the SFTP browser and file-operation panel, and `formatting.rs` owns UI display formatting.

## Security Notes

- SSH terminal authentication is delegated to an OpenSSH client by default, keeping password and private-key passphrase prompts inside the PTY interaction stream.
- Saved connection passwords are managed through system credential support and a local encrypted vault; they should not be written to plaintext config files.
- The built-in command environment is a user-space toolchain bundled with the app and does not modify the global system environment.

## License

R-shell is released under the GPLv3 license. See [LICENSE](LICENSE) for the full terms.

## Roadmap

- Replace or complement system OpenSSH mode with a native SSH backend.
- Continue improving SFTP/FTP file-transfer panels.
- Add VNC as an independent framebuffer viewer.
- Improve cross-platform packaging.
- Continue reducing Windows GTK runtime size and idle memory usage.
