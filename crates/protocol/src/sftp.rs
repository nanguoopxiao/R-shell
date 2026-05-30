//! 基于 `ssh2`/libssh2 的 SFTP 文件传输适配器。
//!
//! GTK UI 将它视为同步文件管理器 API，并把阻塞操作放到后台任务执行。会话将
//! libssh2 `Session`、`Sftp` 和当前工作目录放在锁后面，便于把克隆句柄传给
//! 后台任务，同时避免不安全地共享可变状态。

use anyhow::{Context, Result};
use shell_core::AuthConfig;
use ssh2::{FileStat, OpenFlags, OpenType, Session, Sftp};
use std::{
    io::{self, Read, Seek, SeekFrom},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// SFTP 连接配置。
#[derive(Debug, Clone)]
pub struct SftpConfig {
    pub host: String,
    pub port: u16,
    pub auth: AuthConfig,
    pub password: Option<String>,
}

impl SftpConfig {
    pub fn new(host: impl Into<String>, auth: AuthConfig) -> Self {
        Self {
            host: host.into(),
            port: 22,
            auth,
            password: None,
        }
    }
}

/// 由 libssh2 支撑的活动 SFTP 会话。
#[derive(Clone)]
pub struct SftpSession {
    /// libssh2 的 SFTP 句柄有内部状态，不能独立保证线程安全；在适配器边界串行化
    /// 操作，避免把锁细节泄漏到 GTK 回调中。
    sftp: Arc<Mutex<Sftp>>,
    /// 为需要 SSH 通道的操作保活，例如远端 tar 压缩。
    ssh: Arc<Mutex<Session>>,
    /// 文件管理器操作使用的逻辑远端工作目录。
    cwd: Arc<Mutex<PathBuf>>,
}

impl SftpSession {
    /// 连接 SFTP 服务器并返回已认证的会话。
    pub fn connect(config: &SftpConfig) -> Result<Self> {
        let addr = format!("{}:{}", config.host, config.port);
        let stream = TcpStream::connect(&addr).with_context(|| format!("connecting to {addr}"))?;

        let mut session = Session::new().context("creating SSH session")?;
        session.set_tcp_stream(stream);
        session.handshake().context("SSH handshake")?;

        // 执行认证。
        match &config.auth {
            AuthConfig::None => {
                // 尝试匿名/无认证连接。
            }
            AuthConfig::Password {
                username,
                password_ref: _,
            } => {
                let password = config.password.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("SFTP password authentication requires a password")
                })?;
                session
                    .userauth_password(username, password)
                    .context("SFTP password authentication")?;
            }
            AuthConfig::KeyboardInteractive { .. } => {
                anyhow::bail!("SFTP keyboard-interactive authentication is not supported yet")
            }
            AuthConfig::PrivateKey { username, key_ref } => {
                if let shell_core::CredentialsRef::PrivateKey {
                    path,
                    passphrase_ref,
                } = key_ref
                {
                    let key_path = Path::new(path);
                    session
                        .userauth_pubkey_file(username, None, key_path, passphrase_ref.as_deref())
                        .context("SFTP pubkey authentication")?;
                } else {
                    anyhow::bail!("PrivateKey auth requires a CredentialsRef::PrivateKey");
                }
            }
        }

        let sftp = session.sftp().context("opening SFTP subsystem")?;
        Ok(Self {
            sftp: Arc::new(Mutex::new(sftp)),
            ssh: Arc::new(Mutex::new(session)),
            cwd: Arc::new(Mutex::new(PathBuf::from("/"))),
        })
    }

    /// 返回当前远端工作目录。
    pub fn cwd(&self) -> PathBuf {
        self.cwd.lock().unwrap().clone()
    }

    /// 切换当前远端工作目录。
    pub fn cd(&self, path: &str) -> Result<()> {
        let new_path = self.resolve_remote_path(path);
        let sftp = self.sftp.lock().unwrap();
        let stat = sftp
            .stat(&new_path)
            .with_context(|| format!("checking remote directory {}", new_path.display()))?;
        if !stat.is_dir() {
            anyhow::bail!("{} is not a directory", new_path.display());
        }
        drop(sftp);
        *self.cwd.lock().unwrap() = new_path;
        Ok(())
    }

    fn resolve_remote_path(&self, path: &str) -> PathBuf {
        resolve_remote_path(&self.cwd(), path)
    }

    /// 列出当前工作目录中的文件。
    pub fn list(&self) -> Result<Vec<SftpEntry>> {
        let cwd = self.cwd();
        self.list_at(&cwd)
    }

    fn list_at(&self, remote_dir: &Path) -> Result<Vec<SftpEntry>> {
        let sftp = self.sftp.lock().unwrap();
        let entries = sftp
            .readdir(remote_dir)
            .with_context(|| format!("listing directory {}", remote_dir.display()))?;

        let mut result: Vec<SftpEntry> = entries
            .into_iter()
            .map(|(path, stat)| SftpEntry {
                name: path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                is_dir: stat.is_dir(),
                size: stat.size.unwrap_or(0),
                modified: stat.mtime.unwrap_or(0),
                permissions: stat.perm,
            })
            .filter(|entry| entry.name != "." && entry.name != "..")
            .collect();

        result.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(result)
    }

    /// 将远端文件下载到本地路径。
    pub fn download(&self, remote_name: &str, local_path: &Path) -> Result<u64> {
        let remote_path = self.resolve_remote_path(remote_name);
        let sftp = self.sftp.lock().unwrap();
        let mut remote_file = sftp
            .open(&remote_path)
            .with_context(|| format!("opening remote file {}", remote_path.display()))?;

        if let Some(parent) = local_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating local directory {}", parent.display()))?;
        }

        let mut local_file = std::fs::File::create(local_path)
            .with_context(|| format!("creating local file {}", local_path.display()))?;

        io::copy(&mut remote_file, &mut local_file).context("copying remote file to local path")
    }

    /// 续传或开始下载远端文件到本地路径。
    pub fn download_resume(&self, remote_name: &str, local_path: &Path) -> Result<u64> {
        let remote_path = self.resolve_remote_path(remote_name);
        let sftp = self.sftp.lock().unwrap();
        let remote_size = sftp
            .stat(&remote_path)
            .with_context(|| format!("checking remote file {}", remote_path.display()))?
            .size
            .unwrap_or(0);
        let local_size = std::fs::metadata(local_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let resume_from = local_size.min(remote_size);
        // 将续传偏移限制在本地/远端较小的一侧，避免过大的陈旧本地文件越过远端 EOF。
        if resume_from == remote_size && remote_size > 0 {
            return Ok(remote_size);
        }

        let mut remote_file = sftp
            .open(&remote_path)
            .with_context(|| format!("opening remote file {}", remote_path.display()))?;
        if resume_from > 0 {
            remote_file
                .seek(SeekFrom::Start(resume_from))
                .context("seeking remote file")?;
        }

        if let Some(parent) = local_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating local directory {}", parent.display()))?;
        }

        let mut local_file = if resume_from > 0 {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(local_path)
        } else {
            std::fs::File::create(local_path)
        }
        .with_context(|| format!("opening local file {}", local_path.display()))?;

        let copied = io::copy(&mut remote_file, &mut local_file)
            .context("copying remote file to local path")?;
        Ok(resume_from + copied)
    }

    /// 下载远端文件或目录树到本地目录，能续传时优先续传。
    pub fn download_recursive(&self, remote_name: &str, local_dir: &Path) -> Result<u64> {
        let remote_path = self.resolve_remote_path(remote_name);
        let local_root = local_dir.join(remote_name.trim_matches('/'));
        self.download_tree(&remote_path, &local_root)
    }

    fn download_tree(&self, remote_path: &Path, local_path: &Path) -> Result<u64> {
        let sftp = self.sftp.lock().unwrap();
        let stat = sftp
            .stat(remote_path)
            .with_context(|| format!("checking remote path {}", remote_path.display()))?;
        drop(sftp);

        if !stat.is_dir() {
            let remote_name = path_to_remote_string(remote_path);
            return self.download_resume(&remote_name, local_path);
        }

        std::fs::create_dir_all(local_path)
            .with_context(|| format!("creating local directory {}", local_path.display()))?;

        let mut total = 0u64;
        for entry in self.list_at(remote_path)? {
            let child_remote = resolve_remote_path(remote_path, &entry.name);
            let child_local = local_path.join(&entry.name);
            total += self.download_tree(&child_remote, &child_local)?;
        }
        Ok(total)
    }

    /// 上传本地文件到当前远端目录。
    pub fn upload(&self, local_path: &Path, remote_name: &str) -> Result<u64> {
        let remote_path = self.resolve_remote_path(remote_name);
        let sftp = self.sftp.lock().unwrap();
        let mut remote_file = sftp
            .create(&remote_path)
            .with_context(|| format!("creating remote file {}", remote_path.display()))?;

        let mut local_file = std::fs::File::open(local_path)
            .with_context(|| format!("opening local file {}", local_path.display()))?;

        io::copy(&mut local_file, &mut remote_file).context("copying local file to remote path")
    }

    /// 续传或开始上传本地文件到当前远端目录。
    pub fn upload_resume(&self, local_path: &Path, remote_name: &str) -> Result<u64> {
        let remote_path = self.resolve_remote_path(remote_name);
        let mut local_file = std::fs::File::open(local_path)
            .with_context(|| format!("opening local file {}", local_path.display()))?;
        let local_size = local_file
            .metadata()
            .with_context(|| format!("checking local file {}", local_path.display()))?
            .len();

        let sftp = self.sftp.lock().unwrap();
        let remote_size = sftp
            .stat(&remote_path)
            .ok()
            .and_then(|stat| stat.size)
            .unwrap_or(0);
        let resume_from = remote_size.min(local_size);
        // 将续传偏移限制在本地/远端较小的一侧，避免陈旧远端元数据越过本地源文件。
        if resume_from == local_size && local_size > 0 {
            return Ok(local_size);
        }

        if resume_from > 0 {
            local_file
                .seek(SeekFrom::Start(resume_from))
                .context("seeking local file")?;
        }

        let mut remote_file = if resume_from > 0 {
            sftp.open_mode(
                &remote_path,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::APPEND,
                0o644,
                OpenType::File,
            )
        } else {
            sftp.create(&remote_path)
        }
        .with_context(|| format!("opening remote file {}", remote_path.display()))?;

        let copied = io::copy(&mut local_file, &mut remote_file)
            .context("copying local file to remote path")?;
        Ok(resume_from + copied)
    }

    /// 上传本地文件或目录树，能续传时优先续传。
    pub fn upload_recursive(&self, local_path: &Path, remote_name: &str) -> Result<u64> {
        if local_path.is_dir() {
            let remote_path = self.resolve_remote_path(remote_name);
            self.upload_tree(local_path, &remote_path)
        } else {
            self.upload_resume(local_path, remote_name)
        }
    }

    fn upload_tree(&self, local_path: &Path, remote_path: &Path) -> Result<u64> {
        {
            let sftp = self.sftp.lock().unwrap();
            if sftp.stat(remote_path).is_err() {
                sftp.mkdir(remote_path, 0o755)
                    .with_context(|| format!("creating directory {}", remote_path.display()))?;
            }
        }

        let mut total = 0u64;
        for entry in std::fs::read_dir(local_path)
            .with_context(|| format!("reading local directory {}", local_path.display()))?
        {
            let entry =
                entry.with_context(|| format!("reading entry in {}", local_path.display()))?;
            let child_local = entry.path();
            let child_remote =
                resolve_remote_path(remote_path, &entry.file_name().to_string_lossy());
            if child_local.is_dir() {
                total += self.upload_tree(&child_local, &child_remote)?;
            } else {
                let remote_name = path_to_remote_string(&child_remote);
                total += self.upload_resume(&child_local, &remote_name)?;
            }
        }
        Ok(total)
    }

    /// 删除远端文件。
    pub fn delete(&self, name: &str) -> Result<()> {
        let remote_path = self.resolve_remote_path(name);
        let sftp = self.sftp.lock().unwrap();
        sftp.unlink(&remote_path)
            .with_context(|| format!("deleting {}", remote_path.display()))
    }

    /// 删除远端目录。
    pub fn rmdir(&self, name: &str) -> Result<()> {
        let remote_path = self.resolve_remote_path(name);
        let sftp = self.sftp.lock().unwrap();
        sftp.rmdir(&remote_path)
            .with_context(|| format!("removing directory {}", remote_path.display()))
    }

    /// 删除远端文件或空目录。
    pub fn delete_entry(&self, name: &str, is_dir: bool) -> Result<()> {
        if is_dir {
            self.rmdir(name)
        } else {
            self.delete(name)
        }
    }

    /// 创建远端目录。
    pub fn mkdir(&self, name: &str) -> Result<()> {
        let remote_path = self.resolve_remote_path(name);
        let sftp = self.sftp.lock().unwrap();
        sftp.mkdir(&remote_path, 0o755)
            .with_context(|| format!("creating directory {}", remote_path.display()))
    }

    /// 创建空的远端文件。
    pub fn create_file(&self, name: &str) -> Result<()> {
        let remote_path = self.resolve_remote_path(name);
        let sftp = self.sftp.lock().unwrap();
        let _file = sftp
            .create(&remote_path)
            .with_context(|| format!("creating file {}", remote_path.display()))?;
        Ok(())
    }

    /// 修改远端文件或目录的权限。
    pub fn chmod(&self, name: &str, mode: u32) -> Result<()> {
        let remote_path = self.resolve_remote_path(name);
        let sftp = self.sftp.lock().unwrap();
        sftp.setstat(
            &remote_path,
            FileStat {
                size: None,
                uid: None,
                gid: None,
                perm: Some(mode),
                atime: None,
                mtime: None,
            },
        )
        .with_context(|| format!("changing permissions on {}", remote_path.display()))
    }

    /// 返回当前目录条目的标准化远端绝对路径。
    pub fn absolute_path(&self, name: &str) -> PathBuf {
        self.resolve_remote_path(name)
    }

    /// 将远端文件或目录树复制到当前目录。
    pub fn copy_entry(&self, source_path: &str, destination_name: &str) -> Result<u64> {
        let source_path = normalize_remote_path(source_path);
        let destination_path = self.resolve_remote_path(destination_name);
        self.copy_tree(&source_path, &destination_path)
    }

    fn copy_tree(&self, source_path: &Path, destination_path: &Path) -> Result<u64> {
        let sftp = self.sftp.lock().unwrap();
        let stat = sftp
            .stat(source_path)
            .with_context(|| format!("checking remote path {}", source_path.display()))?;
        drop(sftp);

        if !stat.is_dir() {
            return self.copy_file(source_path, destination_path);
        }

        {
            let sftp = self.sftp.lock().unwrap();
            if sftp.stat(destination_path).is_err() {
                sftp.mkdir(destination_path, 0o755).with_context(|| {
                    format!("creating directory {}", destination_path.display())
                })?;
            }
        }

        let mut total = 0u64;
        for entry in self.list_at(source_path)? {
            let child_source = resolve_remote_path(source_path, &entry.name);
            let child_destination = resolve_remote_path(destination_path, &entry.name);
            total += self.copy_tree(&child_source, &child_destination)?;
        }
        Ok(total)
    }

    fn copy_file(&self, source_path: &Path, destination_path: &Path) -> Result<u64> {
        let sftp = self.sftp.lock().unwrap();
        let mut source = sftp
            .open(source_path)
            .with_context(|| format!("opening source file {}", source_path.display()))?;
        let mut destination = sftp
            .create(destination_path)
            .with_context(|| format!("creating destination file {}", destination_path.display()))?;
        io::copy(&mut source, &mut destination).context("copying remote file")
    }

    /// 将远端条目压缩为当前目录下的 `.tar.gz` 归档。
    pub fn compress_tar_gz(&self, name: &str) -> Result<String> {
        let source_path = self.resolve_remote_path(name);
        let cwd = self.cwd();
        let archive_name = format!("{}.tar.gz", archive_stem(name));
        let archive_path = self.resolve_remote_path(&archive_name);
        let command = format!(
            "tar -czf {} -C {} {}",
            shell_quote(&path_to_remote_string(&archive_path)),
            shell_quote(&path_to_remote_string(&cwd)),
            shell_quote(
                source_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(name)
            )
        );

        let session = self.ssh.lock().unwrap();
        let mut channel = session
            .channel_session()
            .context("opening compression channel")?;
        channel
            .exec(&command)
            .with_context(|| format!("running remote compression command: {command}"))?;
        let mut stdout = String::new();
        let mut stderr = String::new();
        channel
            .read_to_string(&mut stdout)
            .context("reading compression output")?;
        channel
            .stderr()
            .read_to_string(&mut stderr)
            .context("reading compression errors")?;
        channel
            .wait_close()
            .context("closing compression channel")?;
        let exit_status = channel
            .exit_status()
            .context("reading compression exit status")?;
        if exit_status != 0 {
            anyhow::bail!(
                "remote compression failed with exit code {exit_status}: {}",
                stderr.trim()
            );
        }

        Ok(archive_name)
    }

    /// 在当前远端工作目录内重命名文件或目录。
    pub fn rename(&self, old_name: &str, new_name: &str) -> Result<()> {
        let old_path = self.resolve_remote_path(old_name);
        let new_path = self.resolve_remote_path(new_name);
        let sftp = self.sftp.lock().unwrap();
        sftp.rename(&old_path, &new_path, None)
            .with_context(|| format!("renaming {} to {}", old_path.display(), new_path.display()))
    }
}

fn resolve_remote_path(cwd: &Path, path: &str) -> PathBuf {
    let path = path.trim();
    if path.starts_with('/') {
        return normalize_remote_path(path);
    }

    let cwd = path_to_remote_string(cwd);
    let combined = if cwd == "/" {
        format!("/{path}")
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), path)
    };
    normalize_remote_path(&combined)
}

fn path_to_remote_string(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    if value.is_empty() {
        "/".to_string()
    } else {
        value
    }
}

fn normalize_remote_path(path: &str) -> PathBuf {
    let normalized = path.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            value => parts.push(value),
        }
    }

    if parts.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(format!("/{}", parts.join("/")))
    }
}

fn archive_stem(name: &str) -> String {
    let value = name.trim_matches('/').rsplit('/').next().unwrap_or(name);
    let value = value.trim();
    if value.is_empty() {
        "archive".to_string()
    } else {
        value.replace(['/', '\\'], "_")
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// 单个远端目录条目的元数据。
#[derive(Debug, Clone)]
pub struct SftpEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: u64,
    pub permissions: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sftp_entry_sorted_dirs_first() {
        let mut entries = [
            SftpEntry {
                name: "zfile.txt".into(),
                is_dir: false,
                size: 10,
                modified: 0,
                permissions: Some(0o644),
            },
            SftpEntry {
                name: "adir".into(),
                is_dir: true,
                size: 0,
                modified: 0,
                permissions: Some(0o755),
            },
            SftpEntry {
                name: "afile.txt".into(),
                is_dir: false,
                size: 5,
                modified: 0,
                permissions: Some(0o644),
            },
        ];
        entries.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        assert_eq!(entries[0].name, "adir");
        assert_eq!(entries[1].name, "afile.txt");
        assert_eq!(entries[2].name, "zfile.txt");
    }

    #[test]
    fn remote_paths_are_posix_normalized() {
        assert_eq!(
            normalize_remote_path("/var//log/../tmp")
                .display()
                .to_string(),
            "/var/tmp"
        );
        assert_eq!(
            resolve_remote_path(Path::new("/home/root"), "../log/app.txt")
                .display()
                .to_string(),
            "/home/log/app.txt"
        );
        assert_eq!(
            resolve_remote_path(Path::new("/"), "etc/hosts")
                .display()
                .to_string(),
            "/etc/hosts"
        );
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    #[ignore = "requires SHELL_SFTP_TEST_HOST, SHELL_SFTP_TEST_USERNAME and SHELL_SFTP_TEST_PASSWORD"]
    fn sftp_stability_large_file_resume_and_directory_roundtrip() -> Result<()> {
        // 默认测试套件有意忽略该用例：它会修改真实服务器。测试会创建隔离的远端目录，
        // 传输确定性载荷，校验 checksum/大小，并清理本地和远端临时目录。
        let settings = SftpStabilitySettings::from_env()?;
        let session = SftpSession::connect(&settings.config)?;
        session.cd(&settings.remote_root)?;

        let test_name = format!(
            "shell-sftp-stability-{}-{}",
            std::process::id(),
            unix_millis()
        );
        session.mkdir(&test_name)?;
        let remote_test_path = path_to_remote_string(&resolve_remote_path(
            Path::new(&settings.remote_root),
            &test_name,
        ));
        let _remote_cleanup = RemoteCleanupGuard::new(session.clone(), remote_test_path);
        session.cd(&test_name)?;

        let local_root = std::env::temp_dir().join(&test_name);
        let _local_cleanup = LocalCleanupGuard::new(local_root.clone())?;

        let large_file = local_root.join("large.bin");
        let expected_large_hash = write_deterministic_file(&large_file, settings.large_bytes)?;

        let upload_prefix = local_root.join("large-prefix.bin");
        let upload_prefix_len = settings.large_bytes / 3;
        copy_prefix(&large_file, &upload_prefix, upload_prefix_len)?;
        assert_eq!(
            session.upload(&upload_prefix, "large.bin")?,
            upload_prefix_len
        );
        assert_eq!(
            session.upload_resume(&large_file, "large.bin")?,
            settings.large_bytes
        );

        let downloaded_large = local_root.join("large-downloaded.bin");
        copy_prefix(&large_file, &downloaded_large, settings.large_bytes / 5)?;
        assert_eq!(
            session.download_resume("large.bin", &downloaded_large)?,
            settings.large_bytes
        );
        assert_eq!(checksum_file(&downloaded_large)?, expected_large_hash);

        assert_eq!(
            session.copy_entry("large.bin", "large-copy.bin")?,
            settings.large_bytes
        );
        session.rename("large-copy.bin", "large-renamed.bin")?;
        session.chmod("large-renamed.bin", 0o644)?;
        assert!(
            session
                .list()?
                .iter()
                .any(|entry| entry.name == "large-renamed.bin")
        );

        let source_tree = local_root.join("tree-source");
        let expected_tree = write_deterministic_tree(&source_tree, settings.file_count)?;
        let expected_tree_bytes = expected_tree.values().sum::<u64>();
        assert_eq!(
            session.upload_recursive(&source_tree, "tree")?,
            expected_tree_bytes
        );

        let tree_download_root = local_root.join("tree-download");
        assert_eq!(
            session.download_recursive("tree", &tree_download_root)?,
            expected_tree_bytes
        );
        assert_eq!(
            collect_file_sizes(&tree_download_root.join("tree"))?,
            expected_tree
        );

        println!(
            "SFTP stability test passed: {} bytes large file, {} tree files",
            settings.large_bytes, settings.file_count
        );
        Ok(())
    }

    struct SftpStabilitySettings {
        config: SftpConfig,
        remote_root: String,
        large_bytes: u64,
        file_count: usize,
    }

    impl SftpStabilitySettings {
        fn from_env() -> Result<Self> {
            let host = required_env("SHELL_SFTP_TEST_HOST")?;
            let username = required_env("SHELL_SFTP_TEST_USERNAME")?;
            let password = required_env("SHELL_SFTP_TEST_PASSWORD")?;
            let port = optional_env_parse("SHELL_SFTP_TEST_PORT", 22_u16)?;
            let large_mb = optional_env_parse("SHELL_SFTP_TEST_LARGE_MB", 8_u64)?.max(1);
            let file_count = optional_env_parse("SHELL_SFTP_TEST_FILE_COUNT", 64_usize)?.max(1);
            let remote_root =
                std::env::var("SHELL_SFTP_TEST_REMOTE_ROOT").unwrap_or_else(|_| "/tmp".to_string());

            Ok(Self {
                config: SftpConfig {
                    host,
                    port,
                    auth: AuthConfig::Password {
                        username,
                        password_ref: shell_core::CredentialsRef::None,
                    },
                    password: Some(password),
                },
                remote_root,
                large_bytes: large_mb * 1024 * 1024,
                file_count,
            })
        }
    }

    fn required_env(name: &str) -> Result<String> {
        std::env::var(name).with_context(|| format!("missing environment variable {name}"))
    }

    fn optional_env_parse<T>(name: &str, default: T) -> Result<T>
    where
        T: std::str::FromStr,
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        match std::env::var(name) {
            Ok(value) if !value.trim().is_empty() => value
                .parse::<T>()
                .with_context(|| format!("parsing {name}={value}")),
            _ => Ok(default),
        }
    }

    struct LocalCleanupGuard {
        path: PathBuf,
    }

    impl LocalCleanupGuard {
        fn new(path: PathBuf) -> Result<Self> {
            if path.exists() {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("removing stale local test dir {}", path.display()))?;
            }
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating local test dir {}", path.display()))?;
            Ok(Self { path })
        }
    }

    impl Drop for LocalCleanupGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct RemoteCleanupGuard {
        session: SftpSession,
        path: PathBuf,
    }

    impl RemoteCleanupGuard {
        fn new(session: SftpSession, path: String) -> Self {
            Self {
                session,
                path: PathBuf::from(path),
            }
        }
    }

    impl Drop for RemoteCleanupGuard {
        fn drop(&mut self) {
            cleanup_remote_tree(&self.session, &self.path);
        }
    }

    fn cleanup_remote_tree(session: &SftpSession, path: &Path) {
        if let Ok(entries) = session.list_at(path) {
            for entry in entries {
                let child = resolve_remote_path(path, &entry.name);
                if entry.is_dir {
                    cleanup_remote_tree(session, &child);
                } else if let Ok(sftp) = session.sftp.lock() {
                    let _ = sftp.unlink(&child);
                }
            }
        }

        if let Ok(sftp) = session.sftp.lock() {
            let _ = sftp.rmdir(path);
        }
    }

    fn unix_millis() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    }

    fn deterministic_byte(index: u64) -> u8 {
        let mixed = index
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (mixed >> 24) as u8
    }

    fn checksum_update(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    fn write_deterministic_file(path: &Path, len: u64) -> Result<u64> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let mut file =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        let mut remaining = len;
        let mut offset = 0_u64;
        let mut hash = 14_695_981_039_346_656_037_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            for (index, byte) in buffer[..count].iter_mut().enumerate() {
                *byte = deterministic_byte(offset + index as u64);
            }
            file.write_all(&buffer[..count])
                .with_context(|| format!("writing {}", path.display()))?;
            hash = checksum_update(hash, &buffer[..count]);
            offset += count as u64;
            remaining -= count as u64;
        }
        Ok(hash)
    }

    fn checksum_file(path: &Path) -> Result<u64> {
        let mut file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut hash = 14_695_981_039_346_656_037_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("reading {}", path.display()))?;
            if read == 0 {
                break;
            }
            hash = checksum_update(hash, &buffer[..read]);
        }
        Ok(hash)
    }

    fn copy_prefix(source: &Path, destination: &Path, len: u64) -> Result<()> {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        let mut input =
            std::fs::File::open(source).with_context(|| format!("opening {}", source.display()))?;
        let mut output = std::fs::File::create(destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        let mut remaining = len;
        let mut buffer = [0_u8; 64 * 1024];
        while remaining > 0 {
            let count = remaining.min(buffer.len() as u64) as usize;
            let read = input
                .read(&mut buffer[..count])
                .with_context(|| format!("reading {}", source.display()))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .with_context(|| format!("writing {}", destination.display()))?;
            remaining -= read as u64;
        }
        Ok(())
    }

    fn write_deterministic_tree(
        root: &Path,
        file_count: usize,
    ) -> Result<std::collections::BTreeMap<PathBuf, u64>> {
        let mut files = std::collections::BTreeMap::new();
        for index in 0..file_count {
            let relative = PathBuf::from(format!("group-{}/file-{index:04}.txt", index % 4));
            let path = root.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            let text = format!(
                "file={index:04}; payload={}\n",
                "stable-sftp-transfer".repeat(1 + index % 17)
            );
            std::fs::write(&path, text.as_bytes())
                .with_context(|| format!("writing {}", path.display()))?;
            files.insert(relative, text.len() as u64);
        }
        Ok(files)
    }

    fn collect_file_sizes(root: &Path) -> Result<std::collections::BTreeMap<PathBuf, u64>> {
        let mut files = std::collections::BTreeMap::new();
        collect_file_sizes_inner(root, root, &mut files)?;
        Ok(files)
    }

    fn collect_file_sizes_inner(
        root: &Path,
        current: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, u64>,
    ) -> Result<()> {
        for entry in
            std::fs::read_dir(current).with_context(|| format!("reading {}", current.display()))?
        {
            let entry = entry.with_context(|| format!("reading entry in {}", current.display()))?;
            let path = entry.path();
            if path.is_dir() {
                collect_file_sizes_inner(root, &path, files)?;
            } else {
                let relative = path
                    .strip_prefix(root)
                    .with_context(|| format!("relativizing {}", path.display()))?
                    .to_path_buf();
                files.insert(relative, entry.metadata()?.len());
            }
        }
        Ok(())
    }
}
