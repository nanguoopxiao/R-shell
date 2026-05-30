use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shell_core::{Result, ShellError, TerminalSize};

use crate::{default_shell, default_shell_args};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyConfig {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub path_prepend: Vec<PathBuf>,
    pub path_append: Vec<PathBuf>,
    pub size: TerminalSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PtyLaunchOptions {
    pub env: Vec<(String, String)>,
    pub path_prepend: Vec<PathBuf>,
    pub path_append: Vec<PathBuf>,
}

impl PtyConfig {
    #[must_use]
    pub fn default_local(size: TerminalSize) -> Self {
        Self {
            program: default_shell(),
            args: default_shell_args(),
            env: vec![
                ("TERM".to_string(), "xterm-256color".to_string()),
                ("COLORTERM".to_string(), "truecolor".to_string()),
            ],
            path_prepend: Vec::new(),
            path_append: Vec::new(),
            size,
        }
    }

    #[must_use]
    pub fn with_launch_options(mut self, options: PtyLaunchOptions) -> Self {
        self.env.extend(options.env);
        self.path_prepend.extend(options.path_prepend);
        self.path_append.extend(options.path_append);
        self
    }
}

pub struct LocalPty {
    // 保留主控端用于调整尺寸；读取器/写入器拆开后，协议代码可以在后台线程读取，
    // 并从 GTK 事件中写入。
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl LocalPty {
    pub fn spawn(config: PtyConfig) -> Result<(Self, Box<dyn Read + Send>)> {
        // 所有类终端协议最终都会使用这个基础能力。这里不包含 UI 假设，因此
        // SSH、本地 shell、Telnet 包装层可以共享同一套读取循环和尺寸调整语义。
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(to_pty_size(config.size))
            .map_err(|err| ShellError::Platform(err.to_string()))?;

        let mut command = CommandBuilder::new(config.program);
        for arg in config.args {
            command.arg(arg);
        }

        let mut path_base = None;
        for (key, value) in config.env {
            if is_path_key(&key) {
                path_base = Some(OsString::from(value));
                continue;
            }
            command.env(key, value);
        }

        apply_path_overlay(
            &mut command,
            path_base,
            config.path_prepend,
            config.path_append,
        )?;

        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|err| ShellError::Platform(err.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| ShellError::Platform(err.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|err| ShellError::Platform(err.to_string()))?;

        Ok((
            Self {
                master: pair.master,
                writer,
                child,
            },
            reader,
        ))
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.writer
            .write_all(data)
            .map_err(|err| ShellError::Platform(err.to_string()))
    }

    pub fn resize(&mut self, size: TerminalSize) -> Result<()> {
        self.master
            .resize(to_pty_size(size))
            .map_err(|err| ShellError::Platform(err.to_string()))
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.child
            .kill()
            .map_err(|err| ShellError::Platform(err.to_string()))
    }
}

fn apply_path_overlay(
    command: &mut CommandBuilder,
    path_base: Option<OsString>,
    path_prepend: Vec<PathBuf>,
    path_append: Vec<PathBuf>,
) -> Result<()> {
    if path_base.is_none() && path_prepend.is_empty() && path_append.is_empty() {
        return Ok(());
    }

    let base = path_base.unwrap_or_else(|| std::env::var_os("PATH").unwrap_or_default());
    let base_entries = std::env::split_paths(&base);
    let entries = merge_path_entries(
        path_prepend
            .into_iter()
            .chain(base_entries)
            .chain(path_append),
    );
    let joined =
        std::env::join_paths(entries).map_err(|err| ShellError::Platform(err.to_string()))?;
    command.env("PATH", joined);
    Ok(())
}

fn merge_path_entries(entries: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::<String>::new();
    let mut merged = Vec::new();
    for entry in entries {
        if entry.as_os_str().is_empty() {
            continue;
        }
        let key = path_key(&entry);
        if seen.iter().any(|existing| existing == &key) {
            continue;
        }
        seen.push(key);
        merged.push(entry);
    }
    merged
}

fn path_key(path: &Path) -> String {
    let value = path.display().to_string();
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn is_path_key(key: &str) -> bool {
    if cfg!(windows) {
        key.eq_ignore_ascii_case("PATH")
    } else {
        key == "PATH"
    }
}

fn to_pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows,
        cols: size.cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_overlay_keeps_first_entry_when_duplicate() {
        let entries = merge_path_entries(vec![
            PathBuf::from("C:/tools/bin"),
            PathBuf::from("C:/system/bin"),
            PathBuf::from("C:/tools/bin"),
        ]);
        if cfg!(windows) {
            assert_eq!(entries.len(), 2);
        } else {
            assert_eq!(entries.len(), 3);
        }
    }

    #[test]
    fn path_key_detection_is_platform_aware() {
        assert!(is_path_key("PATH"));
        if cfg!(windows) {
            assert!(is_path_key("Path"));
        } else {
            assert!(!is_path_key("Path"));
        }
    }
}
