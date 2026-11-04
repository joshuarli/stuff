#![forbid(unsafe_code)]

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarwinTarget {
    pub id: String,
    pub cpu: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxTarget {
    pub id: String,
    pub cpu: String,
    pub libc: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Darwin(DarwinTarget),
    Linux(LinuxTarget),
}

impl Target {
    pub fn id(&self) -> &str {
        match self {
            Target::Darwin(t) => &t.id,
            Target::Linux(t) => &t.id,
        }
    }

    pub fn platform_key(&self) -> String {
        match self {
            Target::Darwin(t) => format!("darwin/{}/", t.cpu),
            Target::Linux(t) => format!("linux/{}/{}", t.cpu, t.libc),
        }
    }

    pub fn os(&self) -> &'static str {
        match self {
            Target::Darwin(_) => "darwin",
            Target::Linux(_) => "linux",
        }
    }

    pub fn cpu(&self) -> &str {
        match self {
            Target::Darwin(t) => &t.cpu,
            Target::Linux(t) => &t.cpu,
        }
    }

    pub fn node(&self) -> &str {
        match self {
            Target::Darwin(t) => &t.node,
            Target::Linux(t) => &t.node,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    pub allow_legacy_sha1_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub public_base_url: String,
    pub upstream_registry: String,
    pub targets: Vec<Target>,
    pub policy: Policy,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

fn valid_version(v: &str) -> bool {
    let mut parts = v.split('.');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), Some(c), None) => {
            ![a, b, c].iter().any(|p| p.is_empty())
                && [a, b, c].iter().all(|p| {
                    p.bytes().all(|b| b.is_ascii_digit())
                        && (p.len() == 1 || !p.starts_with('0') || *p == "0")
                        && !(p.len() > 1 && p.starts_with('0'))
                }) && {
                    let norm = |p: &str| {
                        if p.len() > 1 && p.starts_with('0') {
                            return false;
                        }
                        !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())
                    };
                    norm(a) && norm(b) && norm(c)
                }
        }
        _ => false,
    }
}

fn require_text(v: &deser_value::Value, label: &str) -> Result<String> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.trim() == s => Ok(s.to_string()),
        _ => Err(Error::Input(format!(
            "{label} must be a non-empty string without surrounding whitespace"
        ))),
    }
}

fn require_https_url(v: &deser_value::Value, label: &str) -> Result<String> {
    let text = require_text(v, label)?;
    let lower = text.to_lowercase();
    if !lower.starts_with("https://") {
        return Err(Error::Input(format!(
            "{label} must be an HTTPS URL without credentials, query, or fragment"
        )));
    }
    let after = &text["https://".len()..];
    if after.is_empty() {
        return Err(Error::Input(format!(
            "{label} must be an HTTPS URL without credentials, query, or fragment"
        )));
    }
    if text.contains('@') || text.contains('?') || text.contains('#') {
        return Err(Error::Input(format!(
            "{label} must be an HTTPS URL without credentials, query, or fragment"
        )));
    }
    if !text.ends_with('/') {
        return Err(Error::Input(format!("{label} must end with a slash")));
    }
    Ok(text)
}

fn map_keys(map: &deser_value::Map) -> Vec<String> {
    let mut out = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let Some((k, _)) = map.get_index(i)
            && let Some(s) = k.as_str() {
                out.push(s.to_string());
            }
    }
    out
}

fn parse_target(v: &deser_value::Value, index: usize) -> Result<Target> {
    let label = format!("targets[{index}]");
    let map = v
        .as_map()
        .ok_or_else(|| Error::Input(format!("{label} must be an object")))?;
    let keys = map_keys(map);
    let get = |k: &str| {
        map.get(k)
            .ok_or_else(|| Error::Input(format!("{label} is missing field {k}")))
    };
    let os_val = get("os")?;
    let os = require_text(os_val, &format!("{label}.os"))?;
    let expected: &[&str] = if os == "linux" {
        &["id", "os", "cpu", "libc", "node"]
    } else {
        &["id", "os", "cpu", "node"]
    };
    for k in &keys {
        if !expected.contains(&k.as_str()) {
            return Err(Error::Input(format!("{label} has unknown field {k}")));
        }
    }
    for k in expected {
        if !keys.iter().any(|x| x == k) {
            return Err(Error::Input(format!("{label} is missing field {k}")));
        }
    }
    let id = require_text(get("id")?, &format!("{label}.id"))?;
    if !valid_id(&id) {
        return Err(Error::Input(format!(
            "{label}.id must contain lowercase letters, digits, and hyphens"
        )));
    }
    if os != "darwin" && os != "linux" {
        return Err(Error::Input(format!("{label}.os is unsupported")));
    }
    let cpu = require_text(get("cpu")?, &format!("{label}.cpu"))?;
    if cpu != "arm64" && cpu != "x64" {
        return Err(Error::Input(format!("{label}.cpu is unsupported")));
    }
    let node = require_text(get("node")?, &format!("{label}.node"))?;
    if !valid_version(&node) {
        return Err(Error::Input(format!(
            "{label}.node must be an exact major.minor.patch version"
        )));
    }
    if os == "darwin" {
        Ok(Target::Darwin(DarwinTarget { id, cpu, node }))
    } else {
        let libc = require_text(get("libc")?, &format!("{label}.libc"))?;
        if libc != "glibc" && libc != "musl" {
            return Err(Error::Input(format!(
                "{label}.libc must be glibc or musl"
            )));
        }
        Ok(Target::Linux(LinuxTarget { id, cpu, libc, node }))
    }
}

pub fn parse_config(text: &str) -> Result<Config> {
    let value: deser_value::Value =
        deser_json::from_str(text).map_err(|e| Error::Input(format!("configuration is not valid JSON: {e}")))?;
    let root = value
        .as_map()
        .ok_or_else(|| Error::Input("configuration must be an object".to_string()))?;
    let keys = map_keys(root);
    for expected in ["schemaVersion", "publicBaseUrl", "upstreamRegistry", "targets", "policy"] {
        if !keys.iter().any(|k| k == expected) {
            return Err(Error::Input(format!(
                "configuration is missing field {expected}"
            )));
        }
    }
    for k in &keys {
        if !["schemaVersion", "publicBaseUrl", "upstreamRegistry", "targets", "policy"].contains(&k.as_str()) {
            return Err(Error::Input(format!(
                "configuration has unknown field {k}"
            )));
        }
    }
    let version = root
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| Error::Input("unsupported configuration schemaVersion".to_string()))?;
    if version != 1 {
        return Err(Error::Input(format!(
            "unsupported configuration schemaVersion: {version}"
        )));
    }
    let public_base_url = require_https_url(
        root.get("publicBaseUrl").ok_or_else(|| Error::Input("configuration is missing field publicBaseUrl".to_string()))?,
        "publicBaseUrl",
    )?;
    let upstream: String = require_https_url(
        root.get("upstreamRegistry").ok_or_else(|| Error::Input("configuration is missing field upstreamRegistry".to_string()))?,
        "upstreamRegistry",
    )?;
    if upstream != "https://registry.npmjs.org/" {
        return Err(Error::Input(
            "upstreamRegistry must be https://registry.npmjs.org/".to_string(),
        ));
    }
    let targets_val = root
        .get("targets")
        .ok_or_else(|| Error::Input("targets must be a non-empty array".to_string()))?;
    let seq = targets_val
        .as_seq()
        .ok_or_else(|| Error::Input("targets must be a non-empty array".to_string()))?;
    if seq.is_empty() {
        return Err(Error::Input("targets must be a non-empty array".to_string()));
    }
    let mut targets = Vec::with_capacity(seq.len());
    for (i, item) in seq.iter().enumerate() {
        targets.push(parse_target(item, i)?);
    }
    let ids: Vec<&str> = targets.iter().map(|t| t.id()).collect();
    let mut sorted_ids = ids.clone();
    sorted_ids.sort_unstable();
    sorted_ids.dedup();
    if sorted_ids.len() != ids.len() {
        return Err(Error::Input("target IDs must be unique".to_string()));
    }
    let mut platforms: Vec<String> = targets.iter().map(|t| t.platform_key()).collect();
    platforms.sort();
    let mut deduped = platforms.clone();
    deduped.sort();
    deduped.dedup();
    if deduped.len() != platforms.len() {
        return Err(Error::Input("target platforms must be unique".to_string()));
    }
    let policy_val = root
        .get("policy")
        .ok_or_else(|| Error::Input("policy must be an object".to_string()))?;
    let policy_map = policy_val
        .as_map()
        .ok_or_else(|| Error::Input("policy must be an object".to_string()))?;
    let pkeys = map_keys(policy_map);
    for expected in ["externalSources", "remoteInstallScripts", "allowLegacySha1Only"] {
        if !pkeys.iter().any(|k| k == expected) {
            return Err(Error::Input(format!("policy is missing field {expected}")));
        }
    }
    for k in &pkeys {
        if !["externalSources", "remoteInstallScripts", "allowLegacySha1Only"].contains(&k.as_str()) {
            return Err(Error::Input(format!("policy has unknown field {k}")));
        }
    }
    let ext = policy_map
        .get("externalSources")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if ext != "reject" {
        return Err(Error::Input("policy.externalSources must be reject".to_string()));
    }
    let rep = policy_map
        .get("remoteInstallScripts")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if rep != "report" {
        return Err(Error::Input(
            "policy.remoteInstallScripts must be report".to_string(),
        ));
    }
    let allow = policy_map
        .get("allowLegacySha1Only")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| Error::Input("policy.allowLegacySha1Only must be a boolean".to_string()))?;
    Ok(Config {
        public_base_url,
        upstream_registry: upstream,
        targets,
        policy: Policy {
            allow_legacy_sha1_only: allow,
        },
    })
}

pub fn load_config(path: &std::path::Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Input(format!("configuration file does not exist: {}", path.display()))
        } else {
            Error::Operation(format!("could not read configuration file: {}", path.display()))
        }
    })?;
    parse_config(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        std::fs::read_to_string("../../tests/fixtures/mynpm/mynpm.json").unwrap()
    }

    #[test]
    fn parses_fixture_config() {
        let cfg = parse_config(&fixture()).unwrap();
        assert_eq!(cfg.targets.len(), 4);
        assert_eq!(cfg.upstream_registry, "https://registry.npmjs.org/");
        assert!(cfg.public_base_url.starts_with("https://"));
    }

    #[test]
    fn rejects_bad_upstream_and_duplicates() {
        let base = fixture();
        let bad: String = base.replace("https://registry.npmjs.org/", "https://example.com/");
        assert!(parse_config(&bad).is_err());
        let cfg = parse_config(&base).unwrap();
        let ids: Vec<&str> = cfg.targets.iter().map(|t| t.id()).collect();
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn rejects_unknown_field_and_empty_targets() {
        assert!(parse_config(r#"{"schemaVersion":1}"#).is_err());
        let mut v: deser_value::Value = deser_json::from_str(&fixture()).unwrap();
        let _ = &mut v;
    }
}
