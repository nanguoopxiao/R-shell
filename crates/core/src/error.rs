use thiserror::Error;

pub type Result<T> = std::result::Result<T, ShellError>;

#[derive(Debug, Error)]
pub enum ShellError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("platform error: {0}")]
    Platform(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("terminal error: {0}")]
    Terminal(String),
    #[error("feature is not implemented yet: {0}")]
    NotImplemented(&'static str),
}
