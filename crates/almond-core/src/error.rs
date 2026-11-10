//! API errors: `{ code, message }` with the JSON-RPC codes from docs/API.md.

use almond_types::Error;
use almond_types::api::error_codes;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: i32,
    pub message: String,
}

pub const METHOD_NOT_FOUND: i32 = -32601;

impl ApiError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }
    pub fn not_implemented(method: &str) -> Self {
        ApiError::new(
            METHOD_NOT_FOUND,
            format!("method not implemented yet: {method}"),
        )
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        ApiError::new(error_codes::INVALID, message)
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        let code = match &e {
            Error::NotFound(_) => error_codes::NOT_FOUND,
            Error::Invalid(_) | Error::Serde(_) => error_codes::INVALID,
            Error::Unsupported(_) => error_codes::UNSUPPORTED,
            Error::AuthRequired(_) | Error::PermissionDenied(_) => error_codes::AUTH_REQUIRED,
            _ => error_codes::INTERNAL,
        };
        ApiError::new(code, e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::invalid(e.to_string())
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
