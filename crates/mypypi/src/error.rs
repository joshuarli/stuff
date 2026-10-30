#![forbid(unsafe_code)]

#[derive(Debug)]
pub enum Kind {
    InvalidInput,
    Configuration,
    MissingArtifact,
    Integrity,
    Incompatible,
    Dependency,
    Build,
    Validation,
    Conflict,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Configuration,
    Planning,
    Acquisition,
    Build,
    Repair,
    Validation,
    Publication,
    Command,
}

impl Stage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Stage::Configuration => "configuration",
            Stage::Planning => "planning",
            Stage::Acquisition => "acquisition",
            Stage::Build => "build",
            Stage::Repair => "repair",
            Stage::Validation => "validation",
            Stage::Publication => "publication",
            Stage::Command => "command",
        }
    }
}

#[derive(Debug)]
pub struct Error {
    pub kind: Kind,
    pub detail: String,
    pub stage: Stage,
    pub package: Option<String>,
    pub target: Option<String>,
    pub usage: bool,
}

impl Error {
    pub fn new(kind: Kind, detail: impl Into<String>, stage: Stage) -> Self {
        Self {
            kind,
            detail: detail.into(),
            stage,
            package: None,
            target: None,
            usage: false,
        }
    }

    pub fn usage(detail: impl Into<String>) -> Self {
        Self {
            kind: Kind::InvalidInput,
            detail: detail.into(),
            stage: Stage::Command,
            package: None,
            target: None,
            usage: true,
        }
    }

    pub fn with_package(mut self, package: impl std::fmt::Display) -> Self {
        self.package = Some(package.to_string());
        self
    }

    pub fn with_target(mut self, python_version: &str, platform: &str) -> Self {
        self.target = Some(format!("Python {python_version} on {platform}"));
        self
    }

    pub fn configuration(detail: impl Into<String>) -> Self {
        Self::new(Kind::Configuration, detail, Stage::Configuration)
    }

    pub fn invalid_input(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::InvalidInput, detail, stage)
    }

    pub fn missing_artifact(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::MissingArtifact, detail, stage)
    }

    pub fn integrity(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Integrity, detail, stage)
    }

    pub fn incompatible(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Incompatible, detail, stage)
    }

    pub fn dependency(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Dependency, detail, stage)
    }

    pub fn build(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Build, detail, stage)
    }

    pub fn validation(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Validation, detail, stage)
    }

    pub fn conflict(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Conflict, detail, stage)
    }

    pub fn io(detail: impl Into<String>, stage: Stage) -> Self {
        Self::new(Kind::Io, detail, stage)
    }

    pub fn exit_code(&self) -> i32 {
        if self.usage {
            return 2;
        }
        match self.kind {
            Kind::InvalidInput | Kind::Configuration | Kind::MissingArtifact => 2,
            Kind::Integrity => 3,
            Kind::Conflict => 4,
            Kind::Incompatible
            | Kind::Dependency
            | Kind::Build
            | Kind::Validation
            | Kind::Io => 1,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage.as_str(), self.detail)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(Kind::Io, e.to_string(), Stage::Command)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_shared_contract() {
        let kind_code = |kind: Kind| Error::new(kind, "x", Stage::Command).exit_code();
        assert_eq!(kind_code(Kind::InvalidInput), 2);
        assert_eq!(kind_code(Kind::Configuration), 2);
        assert_eq!(kind_code(Kind::MissingArtifact), 2);
        assert_eq!(kind_code(Kind::Integrity), 3);
        assert_eq!(kind_code(Kind::Conflict), 4);
        assert_eq!(kind_code(Kind::Io), 1);
        assert_eq!(kind_code(Kind::Build), 1);
        assert_eq!(Error::usage("x").exit_code(), 2);
    }
}
