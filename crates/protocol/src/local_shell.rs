use std::sync::mpsc;

use shell_core::{ProtocolEvent, Result, TerminalSize};
use shell_platform::{PtyConfig, PtyLaunchOptions};

use crate::pty_connection::PtyConnection;

pub struct LocalShellConnection {
    inner: PtyConnection,
}

impl LocalShellConnection {
    pub fn spawn(size: TerminalSize) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        let (inner, receiver) =
            PtyConnection::spawn(PtyConfig::default_local(size), "local-shell-reader")?;
        Ok((Self { inner }, receiver))
    }

    pub fn spawn_with_command(
        size: TerminalSize,
        program: impl Into<String>,
        args: Vec<String>,
    ) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        Self::spawn_with_options(size, program, args, PtyLaunchOptions::default())
    }

    pub fn spawn_with_options(
        size: TerminalSize,
        program: impl Into<String>,
        args: Vec<String>,
        options: PtyLaunchOptions,
    ) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        let (inner, receiver) = PtyConnection::spawn(
            PtyConfig {
                program: program.into(),
                args,
                env: vec![
                    ("TERM".to_string(), "xterm-256color".to_string()),
                    ("COLORTERM".to_string(), "truecolor".to_string()),
                ],
                path_prepend: Vec::new(),
                path_append: Vec::new(),
                size,
            }
            .with_launch_options(options),
            "local-shell-reader",
        )?;
        Ok((Self { inner }, receiver))
    }

    pub fn send_input(&self, data: &[u8]) -> Result<()> {
        self.inner.send_input(data)
    }

    pub fn resize(&self, size: TerminalSize) -> Result<()> {
        self.inner.resize(size)
    }

    pub fn shutdown(&self) -> Result<()> {
        self.inner.shutdown()
    }
}
