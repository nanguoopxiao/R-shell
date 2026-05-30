use std::io::Read;
use std::net::TcpStream;
use std::sync::mpsc;

use anyhow::Context;
use shell_core::{ProtocolEvent, Result, ShellError, TerminalSize};
use shell_platform::{PtyConfig, PtyLaunchOptions};
use ssh2::Session;

use crate::pty_connection::PtyConnection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshConfig {
    pub client_program: Option<String>,
    pub host: String,
    pub username: Option<String>,
    pub port: u16,
    pub identity_file: Option<String>,
    pub extra_args: Vec<String>,
    pub launch_options: PtyLaunchOptions,
}

impl SshConfig {
    #[must_use]
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            client_program: None,
            host: host.into(),
            username: None,
            port: 22,
            identity_file: None,
            extra_args: Vec::new(),
            launch_options: PtyLaunchOptions::default(),
        }
    }

    #[must_use]
    pub fn target(&self) -> String {
        match self
            .username
            .as_deref()
            .filter(|username| !username.is_empty())
        {
            Some(username) => format!("{username}@{}", self.host),
            None => self.host.clone(),
        }
    }

    #[must_use]
    pub fn ssh_args(&self) -> Vec<String> {
        // host-key 提示、密码提示和平台特定加密能力优先复用 OpenSSH 自身体验。额外
        // 兼容参数会追加到目标地址之前，便于设置项按需兼容旧设备。
        let mut args = vec![
            "-tt".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=30".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-p".to_string(),
            self.port.to_string(),
        ];

        if let Some(identity_file) = self
            .identity_file
            .as_deref()
            .map(str::trim)
            .filter(|identity_file| !identity_file.is_empty())
        {
            args.push("-i".to_string());
            args.push(identity_file.to_string());
        }

        args.extend(self.extra_args.iter().cloned());
        args.push(self.target());
        args
    }

    fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            return Err(ShellError::InvalidConfig(
                "SSH host is required".to_string(),
            ));
        }
        if self.port == 0 {
            return Err(ShellError::InvalidConfig(
                "SSH port must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

pub struct SshConnection {
    inner: PtyConnection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostStatsConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HostStats {
    pub cpu_percent: f32,
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub uptime_seconds: u64,
}

impl SshConnection {
    pub fn spawn(
        config: SshConfig,
        size: TerminalSize,
    ) -> Result<(Self, mpsc::Receiver<ProtocolEvent>)> {
        // SSH 终端会话有意使用 PTY 承载的系统进程，而不是原生 libssh2 通道。
        // 这样能保留交互行为，并让凭据提示留在终端数据流中。
        config.validate()?;
        let pty_config = PtyConfig {
            program: config
                .client_program
                .clone()
                .unwrap_or_else(|| "ssh".to_string()),
            args: config.ssh_args(),
            env: vec![
                ("TERM".to_string(), "xterm-256color".to_string()),
                ("COLORTERM".to_string(), "truecolor".to_string()),
            ],
            path_prepend: Vec::new(),
            path_append: Vec::new(),
            size,
        }
        .with_launch_options(config.launch_options.clone());
        let (inner, receiver) = PtyConnection::spawn(pty_config, "ssh-reader")?;
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

pub fn fetch_ssh_host_stats(config: &HostStatsConfig) -> anyhow::Result<HostStats> {
    let address = format!("{}:{}", config.host, config.port);
    let stream =
        TcpStream::connect(&address).with_context(|| format!("connecting to {address}"))?;
    let mut session = Session::new().context("creating SSH stats session")?;
    session.set_tcp_stream(stream);
    session.handshake().context("SSH stats handshake")?;
    session
        .userauth_password(&config.username, &config.password)
        .context("SSH stats password authentication")?;

    let mut channel = session.channel_session().context("opening stats channel")?;
    channel
        .exec(HOST_STATS_COMMAND)
        .context("running stats command")?;
    let mut output = String::new();
    channel
        .read_to_string(&mut output)
        .context("reading stats command output")?;
    channel.wait_close().context("closing stats channel")?;

    parse_host_stats(&output)
}

const HOST_STATS_COMMAND: &str = r#"sh -lc '
read uptime _ < /proc/uptime
read _ u1 n1 s1 i1 w1 irq1 sirq1 steal1 _ < /proc/stat
rx1=0; tx1=0
while read iface rest; do
    case "$iface" in lo:|Inter-*|face*) continue;; *:) set -- $rest; rx1=$((rx1 + $1)); tx1=$((tx1 + $9));; esac
done < /proc/net/dev
sleep 1
read _ u2 n2 s2 i2 w2 irq2 sirq2 steal2 _ < /proc/stat
rx2=0; tx2=0
while read iface rest; do
    case "$iface" in lo:|Inter-*|face*) continue;; *:) set -- $rest; rx2=$((rx2 + $1)); tx2=$((tx2 + $9));; esac
done < /proc/net/dev
mem_total=$(awk "/MemTotal/ {print int(\$2/1024)}" /proc/meminfo)
mem_avail=$(awk "/MemAvailable/ {print int(\$2/1024)}" /proc/meminfo)
echo "uptime=${uptime%.*}"
echo "cpu1=$u1 $n1 $s1 $i1 $w1 $irq1 $sirq1 $steal1"
echo "cpu2=$u2 $n2 $s2 $i2 $w2 $irq2 $sirq2 $steal2"
echo "mem_total=$mem_total"
echo "mem_used=$((mem_total - mem_avail))"
echo "rx_bps=$((rx2 - rx1))"
echo "tx_bps=$((tx2 - tx1))"
'"#;

fn parse_host_stats(output: &str) -> anyhow::Result<HostStats> {
    let uptime_seconds = parse_value(output, "uptime")?.parse::<u64>()?;
    let cpu1 = parse_cpu_line(parse_value(output, "cpu1")?)?;
    let cpu2 = parse_cpu_line(parse_value(output, "cpu2")?)?;
    let mem_total_mb = parse_value(output, "mem_total")?.parse::<u64>()?;
    let mem_used_mb = parse_value(output, "mem_used")?.parse::<u64>()?;
    let rx_bytes_per_sec = parse_value(output, "rx_bps")?.parse::<u64>()?;
    let tx_bytes_per_sec = parse_value(output, "tx_bps")?.parse::<u64>()?;

    let idle_delta = cpu2.idle.saturating_sub(cpu1.idle);
    let total_delta = cpu2.total.saturating_sub(cpu1.total);
    let cpu_percent = if total_delta == 0 {
        0.0
    } else {
        ((total_delta - idle_delta) as f32 / total_delta as f32) * 100.0
    };

    Ok(HostStats {
        cpu_percent,
        mem_used_mb,
        mem_total_mb,
        rx_bytes_per_sec,
        tx_bytes_per_sec,
        uptime_seconds,
    })
}

fn parse_value<'a>(output: &'a str, key: &str) -> anyhow::Result<&'a str> {
    output
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(str::trim)
        .ok_or_else(|| anyhow::anyhow!("missing host stat: {key}"))
}

#[derive(Debug, Clone, Copy)]
struct CpuLine {
    idle: u64,
    total: u64,
}

fn parse_cpu_line(line: &str) -> anyhow::Result<CpuLine> {
    let values: Vec<u64> = line
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if values.len() < 8 {
        anyhow::bail!("invalid CPU stats line")
    }
    let idle = values[3] + values[4];
    let total = values.iter().sum();
    Ok(CpuLine { idle, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_plain_ssh_args() {
        let args = SshConfig::new("example.com").ssh_args();

        assert!(args.contains(&"-tt".to_string()));
        assert!(args.contains(&"22".to_string()));
        assert_eq!(args.last(), Some(&"example.com".to_string()));
    }

    #[test]
    fn builds_user_port_and_identity_args() {
        let mut config = SshConfig::new("example.com");
        config.username = Some("root".to_string());
        config.port = 2222;
        config.identity_file = Some("C:/keys/id_ed25519".to_string());

        let args = config.ssh_args();

        assert!(args.contains(&"2222".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert_eq!(args.last(), Some(&"root@example.com".to_string()));
    }

    #[test]
    fn parses_host_stats_output() {
        let output = "uptime=3600\ncpu1=1 1 1 97 0 0 0 0\ncpu2=2 1 1 196 0 0 0 0\nmem_total=1024\nmem_used=256\nrx_bps=4096\ntx_bps=2048\n";
        let stats = parse_host_stats(output).unwrap();

        assert_eq!(stats.uptime_seconds, 3600);
        assert_eq!(stats.mem_used_mb, 256);
        assert_eq!(stats.mem_total_mb, 1024);
        assert_eq!(stats.rx_bytes_per_sec, 4096);
        assert_eq!(stats.tx_bytes_per_sec, 2048);
        assert!(stats.cpu_percent > 0.0);
    }
}
