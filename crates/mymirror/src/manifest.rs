use deser_value::Value;

use crate::error::{Error, Result};

pub const SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveFormat {
    Raw,
    TarGzip,
    TarXz,
    Zip,
}

impl ArchiveFormat {
    pub fn for_url(url: &str) -> Result<Self> {
        let path = url_path(url);
        for (suffix, format) in [
            (".tar.gz", ArchiveFormat::TarGzip),
            (".tgz", ArchiveFormat::TarGzip),
            (".tar.xz", ArchiveFormat::TarXz),
            (".txz", ArchiveFormat::TarXz),
            (".zip", ArchiveFormat::Zip),
        ] {
            if path.to_ascii_lowercase().ends_with(suffix) {
                return Ok(format);
            }
        }
        Ok(ArchiveFormat::Raw)
    }
}

fn url_path(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or(url);
    let without_query = without_fragment.split('?').next().unwrap_or(without_fragment);
    let path_start = without_query.find("://").map(|index| index + 3).unwrap_or(0);
    let after_host = &without_query[path_start..];
    match after_host.find('/') {
        Some(index) => after_host[index..].to_string(),
        None => "/".to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub strip_components: u64,
    pub keep: Option<Vec<String>>,
}

impl Artifact {
    pub fn format(&self) -> Result<ArchiveFormat> {
        ArchiveFormat::for_url(&self.url)
    }

    pub fn file_name(&self) -> String {
        format!("{}.zip", self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub artifacts: Vec<Artifact>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest> {
        let value: Value = deser_toml::from_str(text)
            .map_err(|error| Error::manifest(format!("not valid TOML: {error}")))?;
        let doc = value
            .as_map()
            .ok_or_else(|| Error::manifest("must be a table"))?;
        for key in keys(doc) {
            if key != "schema_version" && key != "artifact" {
                return Err(Error::manifest(format!("unknown key: {key}")));
            }
        }
        let schema = doc
            .get("schema_version")
            .and_then(|value| value.kind().as_u64())
            .ok_or_else(|| Error::manifest("schema_version must be an integer"))?;
        if schema != SCHEMA_VERSION {
            return Err(Error::manifest(format!(
                "unsupported schema_version {schema} (expected {SCHEMA_VERSION})"
            )));
        }
        let raw = doc
            .get("artifact")
            .and_then(|value| value.as_seq())
            .ok_or_else(|| Error::manifest("missing [[artifact]] entries"))?;
        if raw.is_empty() {
            return Err(Error::manifest("at least one [[artifact]] entry is required"));
        }
        let mut artifacts = Vec::with_capacity(raw.len());
        for (index, entry) in raw.iter().enumerate() {
            let entry = entry
                .as_map()
                .ok_or_else(|| Error::manifest(format!("artifact #{} must be a table", index + 1)))?;
            artifacts.push(parse_artifact(entry, index + 1)?);
        }
        let mut seen = Vec::with_capacity(artifacts.len());
        for artifact in &artifacts {
            if seen.contains(&artifact.id) {
                return Err(Error::manifest(format!("duplicate artifact id '{}'", artifact.id)));
            }
            seen.push(artifact.id.clone());
        }
        Ok(Manifest { artifacts })
    }
}

fn parse_artifact(entry: &deser_value::Map, index: usize) -> Result<Artifact> {
    let label = format!("artifact #{index}");
    for key in keys(entry) {
        if !["id", "version", "url", "sha256", "strip_components", "keep"].contains(&key.as_str()) {
            return Err(Error::manifest(format!("{label}: unknown key '{key}'")));
        }
    }
    let id = required_string(entry, "id", &label)?;
    if !valid_id(&id) {
        return Err(Error::manifest(format!(
            "{label}: id '{id}' must match ^[a-z0-9][a-z0-9._-]{{0,127}}$"
        )));
    }
    let version = required_string(entry, "version", &label)?;
    if version.is_empty() || version.chars().any(|c| c.is_control()) {
        return Err(Error::manifest(format!("{label}: version must not be empty or contain control characters")));
    }
    let url = required_string(entry, "url", &label)?;
    validate_url(&url, &label)?;
    let sha256 = required_string(entry, "sha256", &label)?;
    if sha256.len() != 64 || !sha256.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::manifest(format!(
            "{label}: sha256 must be 64 lowercase hexadecimal characters"
        )));
    }
    let strip_components = match entry.get("strip_components") {
        None => 0,
        Some(value) => {
            let parsed = value
                .kind()
                .as_u64()
                .ok_or_else(|| Error::manifest(format!("{label}: strip_components must be an integer")))?;
            if parsed > 16 {
                return Err(Error::manifest(format!("{label}: strip_components must not exceed 16")));
            }
            parsed
        }
    };
    let keep = match entry.get("keep") {
        None => None,
        Some(value) => {
            let seq = value
                .as_seq()
                .ok_or_else(|| Error::manifest(format!("{label}: keep must be a list of glob patterns")))?;
            if seq.is_empty() {
                return Err(Error::manifest(format!("{label}: keep must not be an empty list")));
            }
            let mut patterns = Vec::with_capacity(seq.len());
            for item in seq.iter() {
                let pattern = item
                    .as_str()
                    .ok_or_else(|| Error::manifest(format!("{label}: keep patterns must be strings")))?;
                validate_glob(pattern, &label)?;
                patterns.push(pattern.to_string());
            }
            patterns.sort();
            patterns.dedup();
            Some(patterns)
        }
    };
    Ok(Artifact {
        id,
        version,
        url,
        sha256,
        strip_components,
        keep,
    })
}

fn keys(map: &deser_value::Map) -> Vec<String> {
    (0..map.len())
        .filter_map(|index| map.get_index(index))
        .filter_map(|(key, _)| key.as_str().map(str::to_string))
        .collect()
}

fn required_string(entry: &deser_value::Map, key: &str, label: &str) -> Result<String> {
    entry
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::manifest(format!("{label}: {key} must be a string")))
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 {
        return false;
    }
    let is_slug = |byte: u8| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-');
    matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9') && bytes.iter().all(|&byte| is_slug(byte))
}

fn validate_url(url: &str, label: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::manifest(format!("{label}: url must use https")))?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains('@') || host.contains(' ') {
        return Err(Error::manifest(format!("{label}: url has an invalid host")));
    }
    Ok(())
}

fn validate_glob(pattern: &str, label: &str) -> Result<()> {
    if pattern.is_empty() || pattern.starts_with('/') || pattern.contains('\\') {
        return Err(Error::manifest(format!(
            "{label}: keep pattern '{pattern}' must be a relative slash-separated glob"
        )));
    }
    for segment in pattern.split('/') {
        if segment == ".." {
            return Err(Error::manifest(format!(
                "{label}: keep pattern '{pattern}' must not contain '..'"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
schema_version = 1

[[artifact]]
id = "node-v26.13.1-darwin-arm64"
version = "26.13.1"
url = "https://nodejs.org/dist/v26.13.1/node-v26.13.1-darwin-arm64.tar.xz"
sha256 = "af6202f5370a3a5209e85dc543758dfd9f44c8726626a236730cd603a67a8559"
strip_components = 1
keep = ["bin/**", "lib/**", "LICENSE"]
"#;

    #[test]
    fn a_valid_manifest_parses() {
        let manifest = Manifest::parse(VALID).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
        let artifact = &manifest.artifacts[0];
        assert_eq!(artifact.id, "node-v26.13.1-darwin-arm64");
        assert_eq!(artifact.version, "26.13.1");
        assert_eq!(artifact.strip_components, 1);
        assert_eq!(artifact.keep.as_deref(), Some(&["LICENSE".to_string(), "bin/**".to_string(), "lib/**".to_string()][..]));
        assert_eq!(artifact.format().unwrap(), ArchiveFormat::TarXz);
    }

    #[test]
    fn keep_defaults_to_keep_everything() {
        let text = VALID.replace("keep = [\"bin/**\", \"lib/**\", \"LICENSE\"]\n", "");
        let manifest = Manifest::parse(&text).unwrap();
        assert_eq!(manifest.artifacts[0].keep, None);
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        let text = VALID.replace("strip_components = 1", "strips = 1");
        let error = Manifest::parse(&text).unwrap_err();
        assert!(error.to_string().contains("unknown key 'strips'"), "{error}");
        let text = VALID.replace("schema_version = 1", "schema_version = 1\nbogus = true");
        let error = Manifest::parse(&text).unwrap_err();
        assert!(error.to_string().contains("unknown key: bogus"), "{error}");
    }

    #[test]
    fn ids_must_be_path_safe_slugs() {
        for bad in ["Node-1", "../x", "a b", "a/b", "", "-a", ".a", "a\\b"] {
            let text = VALID.replace("node-v26.13.1-darwin-arm64", bad);
            assert!(Manifest::parse(&text).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let text = format!("{VALID}\n[[artifact]]\nid = \"node-v26.13.1-darwin-arm64\"\nversion = \"x\"\nurl = \"https://example.test/a.zip\"\nsha256 = \"{}\"\n", "0".repeat(64));
        let error = Manifest::parse(&text).unwrap_err();
        assert!(error.to_string().contains("duplicate"), "{error}");
    }

    #[test]
    fn urls_must_be_https_with_a_host() {
        for bad in ["http://nodejs.org/a.tar.xz", "https://", "https://user@host/a.tar.xz", "ftp://h/a.zip", "nodejs.org/a.zip"] {
            let text = VALID.replace("https://nodejs.org/dist/v26.13.1/node-v26.13.1-darwin-arm64.tar.xz", bad);
            assert!(Manifest::parse(&text).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn sha256_must_be_lowercase_hex() {
        for bad in [&"g".repeat(64), &"A".repeat(64), &"0".repeat(63)] {
            let text = VALID.replace("af6202f5370a3a5209e85dc543758dfd9f44c8726626a236730cd603a67a8559", bad);
            assert!(Manifest::parse(&text).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn keep_patterns_must_be_relative_globs() {
        for bad in ["/abs", "a/../b", "back\\slash", ""] {
            let text = VALID.replace("\"bin/**\"", &format!("\"{bad}\""));
            assert!(Manifest::parse(&text).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn formats_are_detected_from_the_url_path() {
        for (url, expected) in [
            ("https://h/a.tar.gz", ArchiveFormat::TarGzip),
            ("https://h/a.tgz", ArchiveFormat::TarGzip),
            ("https://h/a.TAR.GZ", ArchiveFormat::TarGzip),
            ("https://h/a.tar.xz?x=1#y", ArchiveFormat::TarXz),
            ("https://h/a.txz", ArchiveFormat::TarXz),
            ("https://h/a.zip", ArchiveFormat::Zip),
            ("https://h/install.sh", ArchiveFormat::Raw),
            ("https://h/", ArchiveFormat::Raw),
        ] {
            assert_eq!(ArchiveFormat::for_url(url).unwrap(), expected, "{url}");
        }
    }
}
