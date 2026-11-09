use thiserror::Error;

/// Crate-agnostic error. Implementations map their internal errors into these variants so that
/// callers (UI, sync, pipeline) can react uniformly. Keep details in the message.
#[derive(Debug, Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("authentication required: {0}")]
    AuthRequired(String),
    #[error("network: {0}")]
    Network(String),
    #[error("cancelled")]
    Cancelled,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn other(e: impl std::fmt::Display) -> Self {
        Error::Other(e.to_string())
    }
}
