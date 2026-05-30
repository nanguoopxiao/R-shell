//! 协议适配层。
//!
//! 每个适配器都把外部协议或进程转换为 GTK 应用可消费的统一 `ProtocolEvent`
//! 流。类终端协议暴露 `send_input`/`resize`；文件传输协议则把状态和文件操作
//! 与 UI 隔离。

mod ftp;
mod local_shell;
mod pty_connection;
mod serial;
mod sftp;
mod ssh;
mod telnet;

pub use ftp::{FtpConfig, FtpConnection};
pub use local_shell::LocalShellConnection;
pub use serial::{SerialConfig, SerialConnection, available_serial_ports};
pub use sftp::{SftpConfig, SftpEntry, SftpSession};
pub use shell_core::ProtocolEvent;
pub use ssh::{HostStats, HostStatsConfig, SshConfig, SshConnection, fetch_ssh_host_stats};
pub use telnet::{TelnetConfig, TelnetConnection};
