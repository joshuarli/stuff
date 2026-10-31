#![forbid(unsafe_code)]

#[derive(Debug)]
pub enum Error {
    Usage(String),
    Corrupt(String),
    Conflict(String),
    Limit(String),
    Network(String),
    Operation(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Usage(s) => write!(f, "usage: {s}"),
            Error::Corrupt(s) => write!(f, "corrupt: {s}"),
            Error::Conflict(s) => write!(f, "conflict: {s}"),
            Error::Limit(s) => write!(f, "limit: {s}"),
            Error::Network(s) => write!(f, "network: {s}"),
            Error::Operation(s) => write!(f, "operation: {s}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    pub fn usage(detail: impl Into<String>) -> Self {
        Error::Usage(detail.into())
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => 2,
            Error::Corrupt(_) => 3,
            Error::Conflict(_) => 4,
            Error::Limit(_) => 5,
            Error::Network(_) => 6,
            Error::Operation(_) => 1,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_shared_contract() {
        assert_eq!(Error::usage("x").exit_code(), 2);
        assert_eq!(Error::Corrupt(String::new()).exit_code(), 3);
        assert_eq!(Error::Conflict(String::new()).exit_code(), 4);
        assert_eq!(Error::Limit(String::new()).exit_code(), 5);
        assert_eq!(Error::Network(String::new()).exit_code(), 6);
        assert_eq!(Error::Operation(String::new()).exit_code(), 1);
    }
}
