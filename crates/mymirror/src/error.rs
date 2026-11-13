#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Manifest(String),
    Transport(String),
    Integrity(String),
    Archive(String),
    Operation(String),
}

impl Error {
    pub(crate) fn manifest(message: impl Into<String>) -> Self {
        Error::Manifest(message.into())
    }

    pub(crate) fn integrity(message: impl Into<String>) -> Self {
        Error::Integrity(message.into())
    }

    pub(crate) fn archive(message: impl Into<String>) -> Self {
        Error::Archive(message.into())
    }

    pub(crate) fn operation(message: impl Into<String>) -> Self {
        Error::Operation(message.into())
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Error::Transport(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Manifest(detail) => write!(f, "manifest: {detail}"),
            Error::Transport(detail) => write!(f, "transport: {detail}"),
            Error::Integrity(detail) => write!(f, "integrity: {detail}"),
            Error::Archive(detail) => write!(f, "archive: {detail}"),
            Error::Operation(detail) => write!(f, "{detail}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Operation(format!("{error}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;
