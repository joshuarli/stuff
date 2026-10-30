#![forbid(unsafe_code)]

use std::io;

#[derive(Debug)]
pub enum Error {
    Usage(String),
    Input(String),
    Operation(String),
    Conflict(String),
    Corrupt(String),
    Cancelled(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Usage(s) => write!(f, "usage: {s}"),
            Error::Input(s) => write!(f, "input: {s}"),
            Error::Operation(s) => write!(f, "operation: {s}"),
            Error::Conflict(s) => write!(f, "conflict: {s}"),
            Error::Corrupt(s) => write!(f, "corrupt: {s}"),
            Error::Cancelled(s) => write!(f, "cancelled: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Operation(e.to_string())
    }
}

impl From<deser::Error> for Error {
    fn from(e: deser::Error) -> Self {
        Error::Input(e.to_string())
    }
}

impl From<stuff_store::Error> for Error {
    fn from(e: stuff_store::Error) -> Self {
        match e {
            stuff_store::Error::Input(s) => Error::Input(s),
            stuff_store::Error::Integrity(s) => Error::Corrupt(s),
            stuff_store::Error::Conflict(s) => Error::Conflict(s),
            stuff_store::Error::Operation(s) => Error::Operation(s),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Input(_) => 2,
            Error::Operation(_) => 1,
            Error::Conflict(_) => 1,
            Error::Corrupt(_) => 1,
            Error::Cancelled(_) => 130,
        }
    }
}
