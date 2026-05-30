//! 工作区的共享领域层。
//!
//! 该包有意不包含 GTK、PTY、SSH 或存储实现，只定义 UI、协议适配器、
//! 渲染器和持久化层共同使用的数据契约（`ConnectionProfile`、`ProtocolKind`、
//! 会话事件和错误类型）。

pub mod error;
pub mod events;
pub mod model;

pub use error::{Result, ShellError};
pub use events::{AppCommand, ProtocolEvent, SessionEvent};
pub use model::{
    AuthConfig, ConnectionProfile, CredentialsRef, ProtocolKind, SessionId, SessionStatus,
    TerminalSize,
};
