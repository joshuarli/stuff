#![forbid(unsafe_code)]

use std::path::Path;

use deser_value::Value;

use crate::publish::PublishError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    pub bucket: String,
    pub public_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciConfig {
    pub bucket: String,
    pub blobs_bucket: String,
    pub public_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub cf_profile: String,
    pub cargo: RegistryConfig,
    pub npm: RegistryConfig,
    pub pypi: RegistryConfig,
    pub oci: OciConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cf_profile: "stuff".to_string(),
            cargo: RegistryConfig {
                bucket: "stuff-mycrates".to_string(),
                public_base_url: "https://crates.17166969.xyz".to_string(),
            },
            npm: RegistryConfig {
                bucket: "stuff-mynpm".to_string(),
                public_base_url: "https://npm.17166969.xyz".to_string(),
            },
            pypi: RegistryConfig {
                bucket: "stuff-mypypi".to_string(),
                public_base_url: "https://pypi.17166969.xyz".to_string(),
            },
            oci: OciConfig {
                bucket: "stuff-oci".to_string(),
                blobs_bucket: "stuff-oci-blobs".to_string(),
                public_base_url: "https://oci.17166969.xyz".to_string(),
            },
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, PublishError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(PublishError::Config(format!(
                "could not read {}: {error}",
                path.display()
            ))),
        }
    }

    pub fn parse(text: &str) -> Result<Self, PublishError> {
        let value: Value = deser_json::from_str(text)
            .map_err(|error| PublishError::Config(format!("not valid JSON: {error}")))?;
        if value.as_map().is_none() {
            return Err(PublishError::Config("must be a JSON object".to_string()));
        }
        let mut config = Config::default();
        if let Some(profile) = string_field(&value, "cf_profile", "config")? {
            if profile.is_empty() {
                return Err(PublishError::Config("cf_profile must not be empty".to_string()));
            }
            config.cf_profile = profile;
        }
        if let Some(section) = section(&value, "cargo")? {
            config.cargo = registry_config(section, "cargo", &config.cargo)?;
        }
        if let Some(section) = section(&value, "npm")? {
            config.npm = registry_config(section, "npm", &config.npm)?;
        }
        if let Some(section) = section(&value, "pypi")? {
            config.pypi = registry_config(section, "pypi", &config.pypi)?;
        }
        if let Some(section) = section(&value, "oci")? {
            config.oci = oci_config(section, &config.oci)?;
        }
        Ok(config)
    }
}

fn section<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, PublishError> {
    let map = value
        .as_map()
        .ok_or_else(|| PublishError::Config("must be a JSON object".to_string()))?;
    match map.get(key) {
        None => Ok(None),
        Some(section) if section.as_map().is_some() => Ok(Some(section)),
        Some(_) => Err(PublishError::Config(format!("{key} must be an object"))),
    }
}

fn string_field(value: &Value, key: &str, context: &str) -> Result<Option<String>, PublishError> {
    let map = value
        .as_map()
        .ok_or_else(|| PublishError::Config(format!("{context} must be an object")))?;
    match map.get(key) {
        None => Ok(None),
        Some(field) => field
            .as_str()
            .map(|text| text.to_string())
            .map(Some)
            .ok_or_else(|| PublishError::Config(format!("{context}.{key} must be a string"))),
    }
}

fn registry_config(
    value: &Value,
    context: &str,
    defaults: &RegistryConfig,
) -> Result<RegistryConfig, PublishError> {
    let bucket = string_field(value, "bucket", context)?.unwrap_or_else(|| defaults.bucket.clone());
    let public_base_url = string_field(value, "public_base_url", context)?
        .unwrap_or_else(|| defaults.public_base_url.clone());
    Ok(RegistryConfig { bucket, public_base_url })
}

fn oci_config(value: &Value, defaults: &OciConfig) -> Result<OciConfig, PublishError> {
    let bucket = string_field(value, "bucket", "oci")?.unwrap_or_else(|| defaults.bucket.clone());
    let blobs_bucket = string_field(value, "blobs_bucket", "oci")?
        .unwrap_or_else(|| defaults.blobs_bucket.clone());
    let public_base_url = string_field(value, "public_base_url", "oci")?
        .unwrap_or_else(|| defaults.public_base_url.clone());
    Ok(OciConfig { bucket, blobs_bucket, public_base_url })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;

    #[test]
    fn defaults_match_the_deploy_scripts() {
        let config = Config::default();
        assert_eq!(config.cf_profile, "stuff");
        assert_eq!(config.cargo.bucket, "stuff-mycrates");
        assert_eq!(config.cargo.public_base_url, "https://crates.17166969.xyz");
        assert_eq!(config.npm.bucket, "stuff-mynpm");
        assert_eq!(config.npm.public_base_url, "https://npm.17166969.xyz");
        assert_eq!(config.pypi.bucket, "stuff-mypypi");
        assert_eq!(config.pypi.public_base_url, "https://pypi.17166969.xyz");
        assert_eq!(config.oci.bucket, "stuff-oci");
        assert_eq!(config.oci.blobs_bucket, "stuff-oci-blobs");
        assert_eq!(config.oci.public_base_url, "https://oci.17166969.xyz");
    }

    #[test]
    fn missing_file_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.json")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn valid_file_overrides_only_present_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{
                "cf_profile": "publisher",
                "cargo": {"bucket": "custom-cargo"},
                "npm": {"public_base_url": "https://npm.example"},
                "oci": {"bucket": "custom-meta", "blobs_bucket": "custom-blobs"}
            }"#,
        )
        .unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.cf_profile, "publisher");
        assert_eq!(config.cargo.bucket, "custom-cargo");
        assert_eq!(config.cargo.public_base_url, "https://crates.17166969.xyz");
        assert_eq!(config.npm.bucket, "stuff-mynpm");
        assert_eq!(config.npm.public_base_url, "https://npm.example");
        assert_eq!(config.pypi, Config::default().pypi);
        assert_eq!(config.oci.bucket, "custom-meta");
        assert_eq!(config.oci.blobs_bucket, "custom-blobs");
        assert_eq!(config.oci.public_base_url, "https://oci.17166969.xyz");
    }

    #[test]
    fn malformed_file_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{not json").unwrap();
        let error = Config::load(&path).unwrap_err();
        assert_eq!(error.classification(), ExitCode::Usage);
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn wrong_field_types_are_usage_errors() {
        for text in [
            r#"[]"#,
            r#"{"cargo": "bucket"}"#,
            r#"{"cf_profile": 1}"#,
            r#"{"cf_profile": ""}"#,
            r#"{"cargo": {"bucket": 7}}"#,
            r#"{"oci": {"blobs_bucket": false}}"#,
        ] {
            let error = Config::parse(text).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{text}");
        }
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let config = Config::parse(r#"{"cargo": {"bucket": "b", "future": 1}, "extra": {}}"#).unwrap();
        assert_eq!(config.cargo.bucket, "b");
    }
}
