use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    /// 终端列数，单位为单元格；通过构造函数创建后不会为 0。
    pub cols: u16,
    /// 终端行数，单位为单元格；通过构造函数创建后不会为 0。
    pub rows: u16,
}

impl TerminalSize {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolKind {
    /// 通过本地 PTY 运行操作系统 shell。
    LocalShell,
    /// 通过 PTY 中的系统 OpenSSH 运行交互式 SSH 终端。
    Ssh,
    /// 通过 libssh2 SFTP 执行文件传输/浏览。
    Sftp,
    Ftp,
    Serial,
    Telnet,
    Vnc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialsRef {
    None,
    SystemKeychain {
        service: String,
        account: String,
    },
    PrivateKey {
        path: String,
        passphrase_ref: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthConfig {
    None,
    KeyboardInteractive {
        username: String,
    },
    Password {
        username: String,
        password_ref: CredentialsRef,
    },
    PrivateKey {
        username: String,
        key_ref: CredentialsRef,
    },
}

impl AuthConfig {
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::KeyboardInteractive { username }
            | Self::Password { username, .. }
            | Self::PrivateKey { username, .. } => Some(username.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    /// 稳定的连接配置标识，用于已保存会话的更新和删除。
    pub id: Uuid,
    pub name: String,
    pub protocol: ProtocolKind,
    /// SSH/SFTP/FTP/Telnet/VNC 使用的网络主机。本地 shell 和串口会话留空，
    /// 使用下方协议专属字段。
    pub host: Option<String>,
    pub port: Option<u16>,
    pub auth: AuthConfig,
    pub group: Option<String>,
    pub serial_port: Option<String>,
    pub baud_rate: Option<u32>,
}

impl ConnectionProfile {
    #[must_use]
    pub fn new(name: impl Into<String>, protocol: ProtocolKind) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            protocol,
            host: None,
            port: None,
            auth: AuthConfig::None,
            group: None,
            serial_port: None,
            baud_rate: None,
        }
    }

    #[must_use]
    pub fn local_shell(name: impl Into<String>) -> Self {
        Self::new(name, ProtocolKind::LocalShell)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStatus {
    /// 会话对象已创建，但适配器尚未报告可用的数据流。
    Connecting,
    Connected,
    Disconnecting,
    /// 传输层退出后终端页签仍可保持打开；SSH 可利用该状态提供页签内重连。
    Disconnected,
    Failed(String),
}
