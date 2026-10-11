//! Shared transport for embedded SFTP and monitoring. Verify identity before
//! either caller is allowed to send authentication credentials.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::Engine;
use ssh2::{CheckResult, HashType, KnownHostFileKind, Session};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) fn known_hosts_path() -> Result<PathBuf> {
    Ok(dirs_next::home_dir()
        .context("Cannot locate the home directory for SSH host-key verification")?
        .join(".ssh")
        .join("known_hosts"))
}

pub(crate) fn connect_verified(host: &str, port: u16) -> Result<Session> {
    validate_endpoint(host, port)?;
    let known_hosts_path = known_hosts_path()?;
    let addresses = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("Resolving SSH host {host}:{port}"))?;
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = None;
    let mut connected = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = connected.ok_or_else(|| {
        anyhow::anyhow!(
            "Could not connect to SSH host {host}:{port}: {}",
            last_error.map_or_else(
                || "no usable address or connection timed out".to_string(),
                |e| e.to_string()
            )
        )
    })?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut session = Session::new().context("Creating SSH session")?;
    session.set_timeout(IO_TIMEOUT.as_millis() as u32);
    session.set_tcp_stream(stream);
    session.handshake().context("SSH handshake")?;

    let (key, _) = session
        .host_key()
        .context("SSH server did not provide a host key")?;
    let hash = session
        .host_key_hash(HashType::Sha256)
        .context("Could not compute the SSH server key fingerprint")?;
    let fingerprint = format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash)
    );
    verify_known_host(&session, host, port, key, &fingerprint, &known_hosts_path)?;
    Ok(session)
}

fn validate_endpoint(host: &str, port: u16) -> Result<()> {
    // ssh2::KnownHosts::check uses CString::new(...).unwrap(). Reject
    // embedded NULs here rather than allowing an imported profile to panic.
    if host.trim().is_empty()
        || host.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        || port == 0
    {
        bail!(
            "SSH requires a non-empty host without whitespace/control characters and a nonzero port"
        );
    }
    Ok(())
}

fn verify_known_host(
    session: &Session,
    host: &str,
    port: u16,
    key: &[u8],
    fingerprint: &str,
    path: &Path,
) -> Result<()> {
    validate_endpoint(host, port)?;
    let mut known_hosts = session.known_hosts().context("Opening SSH known hosts")?;
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => bail!("Cannot read SSH known hosts at {}: {error}", path.display()),
    };
    // Rust reads the file so non-ASCII Windows profile paths also work.
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // libssh2 does not implement OpenSSH host certificates/revocation.
        // Never silently ignore a revocation and accept another matching line.
        if line.starts_with('@') {
            bail!(
                "Unsupported SSH known_hosts marker at {}:{}; embedded SFTP/monitoring cannot safely verify this file",
                path.display(),
                index + 1
            );
        }
        known_hosts
            .read_str(line, KnownHostFileKind::OpenSSH)
            .with_context(|| {
                format!(
                    "Cannot parse SSH known hosts at {}:{}",
                    path.display(),
                    index + 1
                )
            })?;
    }
    // check_port also falls back to the port-22 entry for nonstandard ports.
    // Match the exact OpenSSH host token instead: ports are distinct identities.
    let host_token = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    match known_hosts.check(&host_token, key) {
        CheckResult::Match => Ok(()),
        CheckResult::NotFound => bail!(
            "Unknown SSH host key for {host}:{port} ({fingerprint}). Connect through the SSH terminal first, verify the server fingerprint, and save its key in {} before retrying SFTP or monitoring.",
            path.display()
        ),
        CheckResult::Mismatch => bail!(
            "SSH HOST KEY CHANGED for {host}:{port} ({fingerprint}). Connection refused before authentication; verify the server identity and the entry in {}.",
            path.display()
        ),
        CheckResult::Failure => {
            bail!("SSH host-key verification failed for {host}:{port}; connection refused")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ssh2::KnownHostKeyFormat;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    struct TestKnownHosts(std::path::PathBuf);

    impl TestKnownHosts {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "r-shell-known-hosts-测试-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            )))
        }

        fn save(&self, session: &Session, name: &str, key: &[u8]) {
            let mut known_hosts = session.known_hosts().unwrap();
            known_hosts
                .add(name, key, "test fixture", KnownHostKeyFormat::Ed25519)
                .unwrap();
            let entry = known_hosts.hosts().unwrap().remove(0);
            let line = known_hosts
                .write_string(&entry, KnownHostFileKind::OpenSSH)
                .unwrap();
            std::fs::write(&self.0, line).unwrap();
        }
    }

    impl Drop for TestKnownHosts {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn trusted_host_is_accepted_and_changed_key_is_rejected() {
        let session = Session::new().unwrap();
        let file = TestKnownHosts::new();
        file.save(&session, "server.example", b"trusted-key");
        verify_known_host(
            &session,
            "server.example",
            22,
            b"trusted-key",
            "SHA256:test",
            &file.0,
        )
        .unwrap();
        let before = std::fs::read(&file.0).unwrap();
        let error = verify_known_host(
            &session,
            "server.example",
            22,
            b"different-key",
            "SHA256:changed",
            &file.0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("HOST KEY CHANGED"));
        assert_eq!(std::fs::read(&file.0).unwrap(), before);
    }

    #[test]
    fn unknown_hosts_are_not_automatically_trusted() {
        let session = Session::new().unwrap();
        let file = TestKnownHosts::new();
        let error = verify_known_host(
            &session,
            "server.example",
            22,
            b"key",
            "SHA256:test",
            &file.0,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Unknown SSH host key"));
        assert!(error.to_string().contains("SHA256:test"));
        assert!(!file.0.exists());
    }

    #[test]
    fn nonstandard_ports_are_checked_separately() {
        let session = Session::new().unwrap();
        let file = TestKnownHosts::new();
        file.save(&session, "[server.example]:2222", b"trusted-key");
        verify_known_host(
            &session,
            "server.example",
            2222,
            b"trusted-key",
            "SHA256:test",
            &file.0,
        )
        .unwrap();
        assert!(
            verify_known_host(
                &session,
                "server.example",
                22,
                b"trusted-key",
                "SHA256:test",
                &file.0
            )
            .is_err()
        );
        file.save(&session, "server.example", b"trusted-key");
        assert!(
            verify_known_host(
                &session,
                "server.example",
                2222,
                b"trusted-key",
                "SHA256:test",
                &file.0
            )
            .is_err()
        );
    }

    #[test]
    fn openssh_hashed_hostnames_are_supported() {
        let session = Session::new().unwrap();
        let file = TestKnownHosts::new();
        let key = base64::engine::general_purpose::STANDARD.encode(b"trusted-key");
        // OpenSSH HMAC-SHA1 host tokens with a fixed 20-byte test salt.
        for (port, hash) in [
            (22, "zVhOZsHURS579QSKIuuFeROyLGM="),
            (2222, "WV1VhORPV/TQvSSEonNuLkrKxew="),
        ] {
            std::fs::write(
                &file.0,
                format!(
                    "# test fixture\n|1|AAECAwQFBgcICQoLDA0ODxAREhM=|{hash} ssh-ed25519 {key}\n"
                ),
            )
            .unwrap();
            verify_known_host(
                &session,
                "server.example",
                port,
                b"trusted-key",
                "SHA256:test",
                &file.0,
            )
            .unwrap();
        }
    }

    #[test]
    fn invalid_known_hosts_and_revocations_fail_closed() {
        let session = Session::new().unwrap();
        let file = TestKnownHosts::new();
        file.save(&session, "server.example", b"trusted-key");
        let trusted_line = std::fs::read_to_string(&file.0).unwrap();
        std::fs::write(&file.0, format!("@revoked {trusted_line}{trusted_line}")).unwrap();
        assert!(
            verify_known_host(
                &session,
                "server.example",
                22,
                b"trusted-key",
                "SHA256:test",
                &file.0
            )
            .is_err()
        );
        std::fs::write(&file.0, [0xff, 0xfe]).unwrap();
        assert!(
            verify_known_host(
                &session,
                "server.example",
                22,
                b"trusted-key",
                "SHA256:test",
                &file.0
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_endpoints_are_errors_instead_of_panics() {
        for (host, port) in [
            ("", 22),
            ("server\0name", 22),
            ("server\nname", 22),
            ("server", 0),
        ] {
            assert!(validate_endpoint(host, port).is_err());
        }
        assert!(validate_endpoint("::1", 22).is_ok());
    }
}
