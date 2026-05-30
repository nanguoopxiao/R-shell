use crate::model::{ConnectionProfile, SessionId, SessionStatus, TerminalSize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// 基于已保存或刚创建的连接配置请求新会话。
    NewSession(ConnectionProfile),
    SendInput {
        session_id: SessionId,
        data: Vec<u8>,
    },
    Resize {
        session_id: SessionId,
        size: TerminalSize,
    },
    CloseSession(SessionId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    StatusChanged {
        session_id: SessionId,
        status: SessionStatus,
    },
    Output {
        session_id: SessionId,
        data: Vec<u8>,
    },
    TitleChanged {
        session_id: SessionId,
        title: String,
    },
    Exited {
        session_id: SessionId,
        exit_code: Option<i32>,
    },
    Error {
        session_id: SessionId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolEvent {
    /// 原始终端/协议字节，可交给终端解析器或协议 UI 消费。
    Output(Vec<u8>),
    /// 读取器读到 EOF；仅部分后端能提供退出码。
    Exited(Option<i32>),
    /// 传输层错误；通常需要更新状态并停止轮询。
    Error(String),
}
