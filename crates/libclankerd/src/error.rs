//! Errors, following the Docker Engine API's kinds.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// No such machine.
    NotFound,
    /// The request conflicts with current state (name taken, machine running).
    Conflict,
    /// The request itself is malformed or unsupported.
    InvalidParameter,
    /// A dependency is unavailable or did not answer in time (VMM, guest).
    Unavailable,
    /// Anything else: I/O failure, corrupt state.
    System,
}

#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, message)
    }
    pub fn invalid_parameter(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidParameter, message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unavailable, message)
    }
    pub fn system(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::System, message)
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ErrorKind::NotFound => "not found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::InvalidParameter => "invalid parameter",
            ErrorKind::Unavailable => "unavailable",
            ErrorKind::System => "system error",
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::system(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::system(format!("corrupt machine record: {e}"))
    }
}
