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

impl From<stuff_store::Error> for Error {
    fn from(e: stuff_store::Error) -> Self {
        match e {
            stuff_store::Error::Input(s) => Error::Input(s),
            stuff_store::Error::Integrity(s) => Error::Integrity(s),
            stuff_store::Error::Conflict(s) => Error::Conflict(s),
            stuff_store::Error::Operation(s) => Error::Operation(s),
        }
    }
}

impl From<Error> for stuff_store::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Input(s) => stuff_store::Error::Input(s),
            Error::Integrity(s) => stuff_store::Error::Integrity(s),
            Error::Conflict(s) => stuff_store::Error::Conflict(s),
            Error::Limit(s) => stuff_store::Error::Operation(format!("limit: {s}")),
            Error::Network(s) => stuff_store::Error::Operation(format!("network: {s}")),
            Error::Operation(s) => stuff_store::Error::Operation(s),
        }
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
