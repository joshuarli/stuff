#![forbid(unsafe_code)]

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Usage(String),
    Input(String),
    Integrity(String),
    Conflict(String),
    Limit(String),
    Network(String),
    Operation(String),
}

impl Error {
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) | Error::Input(_) => 2,
            Error::Integrity(_) => 3,
            Error::Conflict(_) => 4,
            Error::Limit(_) => 5,
            Error::Network(_) => 6,
            Error::Operation(_) => 1,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, detail) = match self {
            Error::Usage(detail) => ("usage", detail),
            Error::Input(detail) => ("input", detail),
            Error::Integrity(detail) => ("integrity", detail),
            Error::Conflict(detail) => ("conflict", detail),
            Error::Limit(detail) => ("limit", detail),
            Error::Network(detail) => ("network", detail),
            Error::Operation(detail) => ("operation", detail),
        };
        write!(f, "{prefix}: {detail}")
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Operation(error.to_string())
    }
}

impl From<stuff_oci::Error> for Error {
    fn from(error: stuff_oci::Error) -> Self {
        Error::Integrity(error.to_string())
    }
}

impl From<stuff_store::Error> for Error {
    fn from(error: stuff_store::Error) -> Self {
        match error {
            stuff_store::Error::Input(detail) => Error::Input(detail),
            stuff_store::Error::Integrity(detail) => Error::Integrity(detail),
            stuff_store::Error::Conflict(detail) => Error::Conflict(detail),
            stuff_store::Error::Limit(detail) => Error::Limit(detail),
            stuff_store::Error::Network(detail) => Error::Network(detail),
            stuff_store::Error::Operation(detail) => Error::Operation(detail),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_shared_exit_contract() {
        assert_eq!(Error::Usage(String::new()).exit_code(), 2);
        assert_eq!(Error::Input(String::new()).exit_code(), 2);
        assert_eq!(Error::Integrity(String::new()).exit_code(), 3);
        assert_eq!(Error::Conflict(String::new()).exit_code(), 4);
        assert_eq!(Error::Limit(String::new()).exit_code(), 5);
        assert_eq!(Error::Network(String::new()).exit_code(), 6);
        assert_eq!(Error::Operation(String::new()).exit_code(), 1);
    }
}
