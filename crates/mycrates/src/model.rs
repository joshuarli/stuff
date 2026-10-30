use crate::error::{Error, Result};
use crate::version::Version as InnerVersion;
use deser::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CrateName(String);

impl CrateName {
    pub fn new(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Err(Error::Input("empty crate name".to_string()));
        }
        let bytes = s.as_bytes();
        if bytes.is_empty() || !bytes[0].is_ascii_alphabetic() {
            return Err(Error::Input("crate name must start with letter".to_string()));
        }
        if bytes.len() > 64 {
            return Err(Error::Input("crate name too long".to_string()));
        }
        for &b in bytes {
            if !(b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err(Error::Input("invalid crate name character".to_string()));
            }
        }
        Ok(CrateName(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn lower(&self) -> String {
        self.0.to_lowercase()
    }

    pub fn norm_key(&self) -> String {
        self.lower().replace('_', "-")
    }
}

impl fmt::Display for CrateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version(InnerVersion);

impl Version {
    pub fn new(s: &str) -> Result<Self> {
        let v = InnerVersion::parse(s)?;
        Ok(Version(v))
    }

    pub fn key(&self) -> String {
        self.0.to_string()
    }

    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    pub fn uniqueness_key(&self) -> String {
        self.0.uniqueness_key()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[deser(rename_all = "snake_case")]
pub struct Dependency {
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub name: Option<String>,
    #[deser(default)]
    pub version_req: String,
    #[deser(default)]
    pub features: Vec<String>,
    #[deser(default)]
    pub optional: bool,
    #[deser(default)]
    pub default_features: bool,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub target: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub kind: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub registry: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub package: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub explicit_name_in_toml: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishBody {
    pub name: String,
    pub vers: String,
    pub deps: Vec<Dependency>,
    #[deser(default)]
    pub features: std::collections::BTreeMap<String, Vec<String>>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub authors: Option<Vec<String>>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub description: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub documentation: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub homepage: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub keywords: Option<Vec<String>>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub license: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub license_file: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub readme: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub readme_file: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub repository: Option<String>,
    #[deser(default)]
    pub badges: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub links: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub rust_version: Option<String>,
    #[deser(default)]
    pub checksum: String,
    #[deser(default)]
    pub yanked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    #[deser(default)]
    pub name: String,
    #[deser(default)]
    pub vers: String,
    #[deser(default)]
    pub deps: Vec<IndexDep>,
    #[deser(default)]
    pub cksum: String,
    #[deser(default)]
    pub features: std::collections::BTreeMap<String, Vec<String>>,
    #[deser(default)]
    pub yanked: bool,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub links: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub rust_version: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub v: Option<u32>,
    #[deser(flatten)]
    pub extra: std::collections::BTreeMap<String, deser_value::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDep {
    #[deser(default)]
    pub name: String,
    #[deser(default)]
    pub req: String,
    #[deser(default)]
    pub features: Vec<String>,
    #[deser(default)]
    pub optional: bool,
    #[deser(default)]
    pub default_features: bool,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub target: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub kind: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub registry: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub package: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UpstreamIndexLine {
    pub raw: String,
    pub value: IndexEntry,
}
