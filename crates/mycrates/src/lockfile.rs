use crate::error::{Error, Result};
use deser::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Lockfile {
    pub version: Option<i32>,
    #[deser(default)]
    pub package: Vec<LockPackage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockPackage {
    pub name: String,
    pub version: String,
    #[deser(default)]
    pub source: Option<String>,
    #[deser(default)]
    pub checksum: Option<String>,
    #[deser(default)]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum PkgKind {
    CratesIo,
    AlternateRegistry { url: String },
    Git,
    Path,
    Workspace,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct Classified {
    pub pkg: LockPackage,
    pub kind: PkgKind,
}

pub fn parse_lockfile(s: &str) -> Result<Lockfile> {
    let v: crate::toml::Value = crate::toml::from_str(s).map_err(|e| Error::Input(e.to_string()))?;
    let version = v.get("version").and_then(|x| x.as_i64());
    if let Some(ver) = version
        && ver != 3 && ver != 4 {
        return Err(Error::Input(format!("unknown lock version {}", ver)));
    }
    let lock: Lockfile = crate::toml::from_str(s).map_err(|e| Error::Input(e.to_string()))?;
    if let Some(ver) = lock.version
        && ver != 3 && ver != 4 {
        return Err(Error::Input(format!("unknown lock version {}", ver)));
    }
    Ok(lock)
}

pub fn classify(pkg: &LockPackage) -> Classified {
    match &pkg.source {
        None => Classified { pkg: pkg.clone(), kind: PkgKind::Workspace },
        Some(src) => {
            if src.starts_with("git+") {
                Classified { pkg: pkg.clone(), kind: PkgKind::Git }
            } else if src.starts_with("registry+") {
                if src.contains("index.crates.io") || src == "registry+https://github.com/rust-lang/crates.io-index" {
                    Classified { pkg: pkg.clone(), kind: PkgKind::CratesIo }
                } else {
                    Classified { pkg: pkg.clone(), kind: PkgKind::AlternateRegistry { url: src.clone() } }
                }
            } else if src.starts_with("path+") || src.contains("(path+") {
                Classified { pkg: pkg.clone(), kind: PkgKind::Path }
            } else {
                Classified { pkg: pkg.clone(), kind: PkgKind::Unsupported }
            }
        }
    }
}
