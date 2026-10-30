#![forbid(unsafe_code)]

use std::io;

#[derive(Debug)]
pub enum Error {
    Usage(String),
    Input(String),
    Operation(String),
    Conflict(String),
    Corrupt(String),
    Limit(String),
    Network(String),
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
            Error::Limit(s) => write!(f, "limit: {s}"),
            Error::Network(s) => write!(f, "network: {s}"),
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
            stuff_store::Error::Limit(s) => Error::Limit(s),
            stuff_store::Error::Network(s) => Error::Network(s),
            stuff_store::Error::Operation(s) => Error::Operation(s),
        }
    }
}

impl From<Error> for stuff_store::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Usage(s) | Error::Input(s) => stuff_store::Error::Input(s),
            Error::Operation(s) | Error::Cancelled(s) => stuff_store::Error::Operation(s),
            Error::Conflict(s) => stuff_store::Error::Conflict(s),
            Error::Corrupt(s) => stuff_store::Error::Integrity(s),
            Error::Limit(s) => stuff_store::Error::Limit(s),
            Error::Network(s) => stuff_store::Error::Network(s),
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
            Error::Conflict(_) => 4,
            Error::Corrupt(_) => 3,
            Error::Limit(_) => 5,
            Error::Network(_) => 6,
            Error::Cancelled(_) => 130,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_shared_contract() {
        assert_eq!(Error::Usage(String::new()).exit_code(), 2);
        assert_eq!(Error::Input(String::new()).exit_code(), 2);
        assert_eq!(Error::Operation(String::new()).exit_code(), 1);
        assert_eq!(Error::Conflict(String::new()).exit_code(), 4);
        assert_eq!(Error::Corrupt(String::new()).exit_code(), 3);
        assert_eq!(Error::Limit(String::new()).exit_code(), 5);
        assert_eq!(Error::Network(String::new()).exit_code(), 6);
        assert_eq!(Error::Cancelled(String::new()).exit_code(), 130);
    }

    #[test]
    fn limit_and_network_survive_the_store_boundary() {
        let stored = stuff_store::Error::from(Error::Limit("quota".to_string()));
        assert!(matches!(Error::from(stored), Error::Limit(_)));
        let stored = stuff_store::Error::from(Error::Network("down".to_string()));
        assert!(matches!(Error::from(stored), Error::Network(_)));
    }
}
