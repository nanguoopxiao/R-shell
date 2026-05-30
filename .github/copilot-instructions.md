# Copilot Instructions

- This repository is a Rust workspace for a GTK4/gtk-rs cross-platform terminal client.
- Keep the terminal core independent from GTK so it can be tested without a display server.
- Build GTK code behind the `gtk-ui` feature; core crates must compile without GTK4 system libraries.
- Prefer CPU-first rendering paths that work in low-end virtual machines without GPU acceleration.
- Treat memory use as a product requirement: avoid full-buffer clones on hot render paths, keep scrollback bounded by default, and prefer visible-slice rendering over whole-screen snapshots.
- Do not store passwords, private-key passphrases, or tokens in plaintext files.
- Keep protocol adapters isolated from UI code. The UI talks to sessions through events and commands.
- Before finalizing changes, run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` when dependencies are available.
