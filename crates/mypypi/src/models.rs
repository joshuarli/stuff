#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageVersion {
    pub name: String,
    pub version: pep440_rs::Version,
}

impl PackageVersion {
    pub fn parse(name: &str, version: &str) -> Result<Self> {
        let canonical = canonicalize_name(name);
        if canonical != name {
            return Err(Error::configuration("package name must be canonical"));
        }
        let parsed = pep440_rs::Version::from_str(version)
            .map_err(|e| Error::configuration(format!("invalid version: {e}")))?;
        if parsed.to_string() != version {
            return Err(Error::configuration("version pin must be canonical"));
        }
        Ok(Self {
            name: canonical,
            version: parsed,
        })
    }
}

impl std::fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}=={}", self.name, self.version)
    }
}

pub fn canonicalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = false;
    for b in name.bytes() {
        let c = if b == b'-' || b == b'_' || b == b'.' {
            b'-'
        } else {
            b.to_ascii_lowercase()
        };
        if c == b'-' {
            if !prev_dash {
                out.push('-');
            }
            prev_dash = true;
        } else {
            out.push(c as char);
            prev_dash = false;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Target {
    pub python_version: pep440_rs::Version,
    pub platform: String,
}

impl Target {
    pub fn new(python_version: &str, platform: &str) -> Result<Self> {
        if platform.is_empty() || platform.trim() != platform {
            return Err(Error::configuration("target platform must be a non-empty name"));
        }
        let version = pep440_rs::Version::from_str(python_version)
            .map_err(|e| Error::configuration(format!("invalid python version: {e}")))?;
        Ok(Self {
            python_version: version,
            platform: platform.to_string(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WheelIdentity {
    pub filename: String,
    pub package: PackageVersion,
    pub build_tag: Option<String>,
    pub tags: Vec<String>,
}

impl WheelIdentity {
    pub fn parse(filename: &str) -> Result<Self> {
        if !filename.ends_with(".whl") {
            return Err(Error::configuration("wheel filename must end in .whl"));
        }
        let stem = &filename[..filename.len() - 4];
        let parts: Vec<&str> = stem.split('-').collect();
        if parts.len() < 5 || parts.len() > 6 {
            return Err(Error::configuration("invalid wheel filename"));
        }
        let (name, version, build, py, abi, plat) = if parts.len() == 6 {
            (parts[0], parts[1], Some(parts[2]), parts[3], parts[4], parts[5])
        } else {
            (parts[0], parts[1], None, parts[2], parts[3], parts[4])
        };
        for part in [name, version, py, abi, plat] {
            if part.is_empty() {
                return Err(Error::configuration("invalid wheel filename"));
            }
        }
        if let Some(b) = build
            && (b.is_empty() || !b.chars().next().is_some_and(|c| c.is_ascii_digit())) {
                return Err(Error::configuration("invalid wheel build tag"));
            }
        let package = PackageVersion {
            name: canonicalize_name(name),
            version: pep440_rs::Version::from_str(version)
                .map_err(|e| Error::configuration(format!("invalid version: {e}")))?,
        };
        if package.version.to_string() != version {
            return Err(Error::configuration("version pin must be canonical"));
        }
        let mut tags = Vec::new();
        for component in [py, abi, plat] {
            for tag in component.split('.') {
                if tag.is_empty() {
                    return Err(Error::configuration("invalid wheel tag"));
                }
                tags.push(tag.to_string());
            }
        }
        tags.sort();
        tags.dedup();
        Ok(Self {
            filename: filename.to_string(),
            package,
            build_tag: build.map(|s| s.to_string()),
            tags,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ValidatedArtifact {
    pub wheel: WheelIdentity,
    pub path: std::path::PathBuf,
    pub sha256: String,
    pub targets: Vec<Target>,
}

impl ValidatedArtifact {
    pub fn new(
        wheel: WheelIdentity,
        path: std::path::PathBuf,
        sha256: String,
        targets: Vec<Target>,
    ) -> Result<Self> {
        if !is_lower_hex(&sha256) {
            return Err(Error::configuration("SHA-256 must contain 64 lowercase hexadecimal characters"));
        }
        if targets.is_empty() {
            return Err(Error::configuration(
                "validated artifact must cover at least one target",
            ));
        }
        Ok(Self { wheel, path, sha256, targets })
    }
}

fn is_lower_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_names() {
        assert_eq!(canonicalize_name("Foo_Bar.Baz"), "foo-bar-baz");
        assert_eq!(canonicalize_name("a--b__c..d"), "a-b-c-d");
    }

    #[test]
    fn parses_pins_and_rejects_noncanonical() {
        assert!(PackageVersion::parse("foo-bar", "1.0").is_ok());
        assert!(PackageVersion::parse("Foo_Bar", "1.0").is_err());
        assert!(PackageVersion::parse("foo", "1.0a").is_err());
        assert!(PackageVersion::parse("foo", "1.0.0.0.0").is_ok());
    }

    #[test]
    fn parses_wheel_filenames() {
        let w = WheelIdentity::parse("foo_bar-1.0-py3-none-any.whl").unwrap();
        assert_eq!(w.package.name, "foo-bar");
        assert!(WheelIdentity::parse("foo-1.0-py3-none.whl").is_err());
        assert!(WheelIdentity::parse("foo-1.0-py3-none-any.tar.gz").is_err());
    }
}
