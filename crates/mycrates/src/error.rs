use std::io;

#[derive(Debug)]
pub enum Error {
    Input(String),
    Integrity(String),
    Conflict(String),
    Limit(String),
    Network(String),
    Operation(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Input(s) => write!(f, "input: {s}"),
            Error::Integrity(s) => write!(f, "integrity: {s}"),
            Error::Conflict(s) => write!(f, "conflict: {s}"),
            Error::Limit(s) => write!(f, "limit: {s}"),
            Error::Network(s) => write!(f, "network: {s}"),
            Error::Operation(s) => write!(f, "operation: {s}"),
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

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Input(_) => 2,
            Error::Integrity(_) => 3,
            Error::Conflict(_) => 4,
            Error::Limit(_) => 5,
            Error::Network(_) => 6,
            Error::Operation(_) => 1,
        }
    }
}

impl From<std::convert::Infallible> for Error {
    fn from(_: std::convert::Infallible) -> Self {
        Error::Operation("infallible".to_string())
    }
}
