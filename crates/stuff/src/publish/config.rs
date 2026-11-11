#![forbid(unsafe_code)]

use std::path::Path;

use deser_value::Value;

use crate::publish::{PublishError, Registry};
use crate::publish::cargo::render::CargoUrls;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryConfig {
    pub bucket: String,
    pub public_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoConfig {
    pub bucket: String,
    pub public_base_url: String,
    pub download_base_url: String,
}

impl CargoConfig {
    pub fn urls(&self) -> CargoUrls {
        CargoUrls {
            public_base_url: self.public_base_url.clone(),
            download_base_url: self.download_base_url.clone(),
        }
    }
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
    pub cargo: CargoConfig,
    pub npm: RegistryConfig,
    pub pypi: RegistryConfig,
    pub oci: OciConfig,
}

impl Config {
    /// The registry whose publication owns `bucket`; the OCI blob bucket
    /// belongs to the OCI registry.
    pub fn registry_for_bucket(&self, bucket: &str) -> Option<Registry> {
        [
            (&self.cargo.bucket, Registry::Cargo),
            (&self.npm.bucket, Registry::Npm),
            (&self.pypi.bucket, Registry::Pypi),
            (&self.oci.bucket, Registry::Oci),
            (&self.oci.blobs_bucket, Registry::Oci),
        ]
        .into_iter()
        .find_map(|(name, registry)| (name == bucket).then_some(registry))
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cf_profile: "stuff".to_string(),
            cargo: CargoConfig {
                bucket: "stuff-mycrates".to_string(),
                public_base_url: "https://crates.17166969.xyz".to_string(),
                download_base_url: "https://crates-blobs.17166969.xyz".to_string(),
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
            config.cargo = cargo_config(section, &config.cargo)?;
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

fn cargo_config(value: &Value, defaults: &CargoConfig) -> Result<CargoConfig, PublishError> {
    let bucket = string_field(value, "bucket", "cargo")?.unwrap_or_else(|| defaults.bucket.clone());
    let public_base_url = string_field(value, "public_base_url", "cargo")?;
    let download_base_url = match (string_field(value, "download_base_url", "cargo")?, &public_base_url) {
        (Some(download), _) => download,
        (None, Some(public)) => public.clone(),
        (None, None) => defaults.download_base_url.clone(),
    };
    Ok(CargoConfig {
        bucket,
        public_base_url: public_base_url.unwrap_or_else(|| defaults.public_base_url.clone()),
        download_base_url,
    })
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
        assert_eq!(config.cargo.download_base_url, "https://crates-blobs.17166969.xyz");
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
        assert_eq!(config.registry_for_bucket("custom-cargo"), Some(Registry::Cargo));
        assert_eq!(config.registry_for_bucket("stuff-mycrates"), None);
        assert_eq!(config.registry_for_bucket("stuff-mynpm"), Some(Registry::Npm));
        assert_eq!(config.registry_for_bucket("custom-meta"), Some(Registry::Oci));
        assert_eq!(config.registry_for_bucket("custom-blobs"), Some(Registry::Oci));
        assert_eq!(config.cargo.public_base_url, "https://crates.17166969.xyz");
        assert_eq!(config.npm.bucket, "stuff-mynpm");
        assert_eq!(config.npm.public_base_url, "https://npm.example");
        assert_eq!(config.pypi, Config::default().pypi);
        assert_eq!(config.oci.bucket, "custom-meta");
        assert_eq!(config.oci.blobs_bucket, "custom-blobs");
        assert_eq!(config.oci.public_base_url, "https://oci.17166969.xyz");
    }

    #[test]
    fn cargo_downloads_follow_a_moved_public_host_unless_set() {
        let moved = Config::parse(r#"{"cargo": {"public_base_url": "https://crates.example"}}"#).unwrap();
        assert_eq!(moved.cargo.download_base_url, "https://crates.example");
        let split = Config::parse(
            r#"{"cargo": {"public_base_url": "https://crates.example", "download_base_url": "https://dl.example"}}"#,
        )
        .unwrap();
        assert_eq!(split.cargo.public_base_url, "https://crates.example");
        assert_eq!(split.cargo.download_base_url, "https://dl.example");
        let bucket_only = Config::parse(r#"{"cargo": {"bucket": "b"}}"#).unwrap();
        assert_eq!(bucket_only.cargo.download_base_url, "https://crates-blobs.17166969.xyz");
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
