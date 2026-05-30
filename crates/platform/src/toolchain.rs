use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::PtyLaunchOptions;

const TOOLS_ROOT_ENV: &str = "SHELL_TOOLS_ROOT";
const REQUIRED_COMMANDS: [&str; 6] = ["bash", "sh", "curl", "wget", "ssh", "telnet"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolchainPathPriority {
    SystemFirst,
    ToolchainFirst,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolchainDiscovery {
    pub toolchain: Option<BuiltinToolchain>,
    pub searched_roots: Vec<PathBuf>,
    pub missing_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinToolchain {
    pub root: PathBuf,
    pub usr_bin: PathBuf,
    pub mingw_bin: PathBuf,
    pub ca_bundle: Option<PathBuf>,
}

impl BuiltinToolchain {
    #[must_use]
    pub fn discover() -> ToolchainDiscovery {
        let searched_roots = candidate_roots();
        let mut best_missing = REQUIRED_COMMANDS
            .iter()
            .map(|command| (*command).to_string())
            .collect::<Vec<_>>();

        for root in &searched_roots {
            let Some(toolchain) = Self::from_root(root) else {
                continue;
            };
            let missing = toolchain.missing_required_commands();
            if missing.is_empty() {
                return ToolchainDiscovery {
                    toolchain: Some(toolchain),
                    searched_roots,
                    missing_commands: Vec::new(),
                };
            }
            if missing.len() < best_missing.len() {
                best_missing = missing;
            }
        }

        ToolchainDiscovery {
            toolchain: None,
            searched_roots,
            missing_commands: best_missing,
        }
    }

    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Option<Self> {
        let root = root.into();
        let usr_bin = root.join("usr").join("bin");
        let mingw_bin = root.join("mingw64").join("bin");
        if !usr_bin.is_dir() && !mingw_bin.is_dir() {
            return None;
        }

        Some(Self {
            ca_bundle: ca_bundle_path(&root),
            root,
            usr_bin,
            mingw_bin,
        })
    }

    #[must_use]
    pub fn command_path(&self, command: &str) -> Option<PathBuf> {
        let command = command_with_suffix(command);
        self.bin_dirs()
            .into_iter()
            .map(|dir| dir.join(&command))
            .find(|path| path.is_file())
    }

    #[must_use]
    pub fn path_entries(&self) -> Vec<PathBuf> {
        self.bin_dirs()
            .into_iter()
            .filter(|path| path.is_dir())
            .collect()
    }

    #[must_use]
    pub fn launch_options(&self, priority: ToolchainPathPriority) -> PtyLaunchOptions {
        let mut options = PtyLaunchOptions {
            env: self.environment(),
            ..PtyLaunchOptions::default()
        };
        match priority {
            ToolchainPathPriority::SystemFirst => options.path_append = self.path_entries(),
            ToolchainPathPriority::ToolchainFirst => options.path_prepend = self.path_entries(),
        }
        options
    }

    #[must_use]
    pub fn missing_required_commands(&self) -> Vec<String> {
        REQUIRED_COMMANDS
            .iter()
            .filter(|command| self.command_path(command).is_none())
            .map(|command| (*command).to_string())
            .collect()
    }

    fn environment(&self) -> Vec<(String, String)> {
        let mut env = vec![
            (TOOLS_ROOT_ENV.to_string(), self.root.display().to_string()),
            ("MSYSTEM".to_string(), "MINGW64".to_string()),
            ("MSYS2_PATH_TYPE".to_string(), "inherit".to_string()),
            ("CHERE_INVOKING".to_string(), "1".to_string()),
        ];

        if let Some(ca_bundle) = &self.ca_bundle {
            let ca_bundle = ca_bundle.display().to_string();
            env.push(("SSL_CERT_FILE".to_string(), ca_bundle.clone()));
            env.push(("CURL_CA_BUNDLE".to_string(), ca_bundle.clone()));
            env.push(("GIT_SSL_CAINFO".to_string(), ca_bundle));
        }

        env
    }

    fn bin_dirs(&self) -> Vec<PathBuf> {
        vec![self.usr_bin.clone(), self.mingw_bin.clone()]
    }
}

fn candidate_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(root) = std::env::var(TOOLS_ROOT_ENV) {
        roots.push(PathBuf::from(root));
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(bin_dir) = exe.parent()
    {
        roots.push(bin_dir.join("tools").join("msys64"));
        if let Some(package_root) = bin_dir.parent() {
            roots.push(package_root.join("tools").join("msys64"));
        }
    }

    if let Ok(current_dir) = std::env::current_dir() {
        roots.push(current_dir.join(".msys64"));
    }

    dedupe_paths(roots)
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| seen.insert(path_key(path)))
        .collect()
}

fn path_key(path: &Path) -> String {
    let value = path.display().to_string();
    if cfg!(windows) {
        value.to_ascii_lowercase()
    } else {
        value
    }
}

fn command_with_suffix(command: &str) -> String {
    if cfg!(windows) && !command.to_ascii_lowercase().ends_with(".exe") {
        format!("{command}.exe")
    } else {
        command.to_string()
    }
}

fn ca_bundle_path(root: &Path) -> Option<PathBuf> {
    [
        root.join("etc")
            .join("pki")
            .join("ca-trust")
            .join("extracted")
            .join("pem")
            .join("tls-ca-bundle.pem"),
        root.join("usr")
            .join("ssl")
            .join("certs")
            .join("ca-bundle.crt"),
        root.join("mingw64")
            .join("ssl")
            .join("certs")
            .join("ca-bundle.crt"),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_suffix_is_windows_aware() {
        let command = command_with_suffix("curl");
        if cfg!(windows) {
            assert_eq!(command, "curl.exe");
        } else {
            assert_eq!(command, "curl");
        }
    }

    #[test]
    fn path_dedupe_is_case_insensitive_on_windows() {
        let paths = dedupe_paths(vec![PathBuf::from("C:/Tools"), PathBuf::from("c:/tools")]);
        if cfg!(windows) {
            assert_eq!(paths.len(), 1);
        } else {
            assert_eq!(paths.len(), 2);
        }
    }
}
