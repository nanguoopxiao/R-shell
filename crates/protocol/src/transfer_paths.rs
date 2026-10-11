//! Path checks for recursive transfers. Reject ambiguous names instead of
//! silently rewriting them into a different local or remote path.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const MAX_TRANSFER_DEPTH: usize = 128;

pub(crate) fn check_depth(depth: usize) -> Result<()> {
    if depth > MAX_TRANSFER_DEPTH {
        bail!("Directory nesting exceeds the transfer limit of {MAX_TRANSFER_DEPTH}");
    }
    Ok(())
}

pub(crate) fn check_remote_name(name: &str) -> Result<()> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        bail!("Unsafe or unsupported remote entry name: {name:?}");
    }
    Ok(())
}

pub(crate) fn local_child(parent: &Path, name: &str) -> Result<PathBuf> {
    check_remote_name(name)?;
    #[cfg(windows)]
    {
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        let port_device = stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&suffix)
            });
        let reserved = matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || port_device;
        if reserved
            || name.ends_with(['.', ' '])
            || name.contains([':', '<', '>', '"', '|', '?', '*'])
        {
            bail!("Remote filename cannot be represented safely on Windows: {name:?}");
        }
    }
    let path = parent.join(name);
    check_local_link(&path)?;
    Ok(path)
}

pub(crate) fn check_local_link(path: &Path) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Checking {}", path.display())),
    };
    let is_link = metadata.file_type().is_symlink();
    #[cfg(windows)]
    let is_link = {
        use std::os::windows::fs::MetadataExt;
        // Directory junctions also cross the selected transfer tree.
        is_link || (metadata.is_dir() && metadata.file_attributes() & 0x400 != 0)
    };
    if is_link {
        bail!(
            "Recursive transfers do not follow symbolic links or junctions: {}",
            path.display()
        );
    }
    if !metadata.is_dir() && !metadata.is_file() {
        bail!(
            "Recursive transfers only support regular files and directories: {}",
            path.display()
        );
    }
    Ok(())
}

pub(crate) fn check_copy_destination(source: &Path, destination: &Path) -> Result<()> {
    let source = source.to_string_lossy().replace('\\', "/");
    let destination = destination.to_string_lossy().replace('\\', "/");
    let prefix = format!("{}/", source.trim_end_matches('/'));
    if source == destination || destination.starts_with(&prefix) {
        bail!("Cannot copy a file or directory onto itself or into its own subtree");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires permission to create a local symbolic link"]
    fn existing_download_links_are_rejected_without_touching_the_target() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(self.0.join("link"));
                let _ = std::fs::remove_file(self.0.join("target"));
                let _ = std::fs::remove_dir(&self.0);
            }
        }
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "r-shell-transfer-link-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Fixture(root);
        let target = fixture.0.join("target");
        let link = fixture.0.join("link");
        std::fs::write(&target, b"preserve this file").unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target, &link).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(local_child(&fixture.0, "link").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"preserve this file");
    }

    #[test]
    fn downloads_cannot_escape_the_selected_directory() {
        let parent = std::env::temp_dir();
        for name in [
            "",
            ".",
            "..",
            "../outside",
            "/absolute",
            "a/b",
            "a\\b",
            "a\0b",
            "a\nb",
        ] {
            assert!(local_child(&parent, name).is_err(), "accepted {name:?}");
        }
        assert_eq!(
            local_child(&parent, "文件 example.txt").unwrap(),
            parent.join("文件 example.txt")
        );
    }

    #[test]
    #[cfg(windows)]
    fn downloads_reject_device_names_streams_and_windows_aliases() {
        for name in [
            "CON",
            "CONOUT$",
            "con .txt",
            "COM¹.log",
            "LPT²",
            "nul.txt",
            "COM1.log",
            "LPT9",
            "C:escape",
            "file:stream",
            "file.",
            "file ",
            "a?b",
        ] {
            assert!(
                local_child(&std::env::temp_dir(), name).is_err(),
                "accepted {name:?}"
            );
        }
    }

    #[test]
    fn copies_cannot_target_their_own_source_tree() {
        assert!(check_copy_destination(Path::new("/a"), Path::new("/a")).is_err());
        assert!(check_copy_destination(Path::new("/a"), Path::new("/a/b")).is_err());
        assert!(check_copy_destination(Path::new("/"), Path::new("/copy")).is_err());
        assert!(check_copy_destination(Path::new("/a"), Path::new("/a-copy")).is_ok());
        assert!(check_copy_destination(Path::new("/a"), Path::new("/b/a")).is_ok());
        assert!(check_depth(128).is_ok());
        assert!(check_depth(129).is_err());
    }
}
