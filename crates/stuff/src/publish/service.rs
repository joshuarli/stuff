#![forbid(unsafe_code)]

use std::io::Read;
use std::path::PathBuf;
use deser_value::Value;
use crate::publish::{Environment, PublishError};
use crate::publish::paths::Paths;

pub struct PublishingService {
    pub endpoint: String,
    pub coordinator_token: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub region: String,
}

impl PublishingService {
    pub fn configured(env: &Environment) -> Result<Option<Self>, PublishError> {
        let names = ["STUFF_COORDINATOR_URL", "STUFF_COORDINATOR_TOKEN", "STUFF_S3_ACCESS_KEY_ID", "STUFF_S3_SECRET_ACCESS_KEY"];
        if names.iter().any(|name| env.get(name).is_some()) {
            let required = |name| env.non_empty(name).map(str::to_string)
                .ok_or_else(|| PublishError::Config(format!("{name} is required for the publishing service")));
            let endpoint = required(names[0])?;
            validate_endpoint(&endpoint)?;
            if let Some(gateway) = env.non_empty("STUFF_S3_GATEWAY_URL")
                && gateway.trim_end_matches('/') != format!("{}/_s3", endpoint.trim_end_matches('/')) {
                return Err(PublishError::Config("STUFF_S3_GATEWAY_URL must be the publishing endpoint's /_s3 route".into()));
            }
            return Ok(Some(Self {
                endpoint: endpoint.trim_end_matches('/').into(),
                coordinator_token: required(names[1])?, access_key_id: required(names[2])?,
                secret_access_key: required(names[3])?, region: env.non_empty("STUFF_S3_REGION").unwrap_or("auto").into(),
            }));
        }
        let path = env.non_empty("STUFF_CREDENTIAL_FILE").map(PathBuf::from)
            .unwrap_or_else(|| Paths::from_environment(env).publishing_credentials_file());
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && env.get("STUFF_CREDENTIAL_FILE").is_none() => return Ok(None),
            Err(_) => return Err(PublishError::Config(format!("cannot read publishing credentials at {}", path.display()))),
        };
        let metadata = file.metadata().map_err(|_| PublishError::Config("cannot inspect publishing credential file".into()))?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(PublishError::Config("publishing credential file must be readable only by its owner (mode 600)".into()));
            }
        }
        if !metadata.is_file() || metadata.len() > 65_536 {
            return Err(PublishError::Config("invalid publishing credential file".into()));
        }
        let mut bytes = Vec::new();
        file.take(65_537).read_to_end(&mut bytes).map_err(|_| PublishError::Config("cannot read publishing credential file".into()))?;
        if bytes.len() > 65_536 { return Err(PublishError::Config("publishing credential file exceeds 64 KiB".into())); }
        let value: Value = deser_json::from_slice(&bytes).map_err(|_| PublishError::Config("invalid publishing credential JSON".into()))?;
        let map = value.as_map().ok_or_else(|| PublishError::Config("publishing credentials must be an object".into()))?;
        if map.keys().any(|key| !["schema", "endpoint", "coordinator_token", "access_key_id", "secret_access_key", "region"].contains(&key.as_str().unwrap_or("")))
            || map.get("schema").and_then(|v| v.kind().as_u64()) != Some(1) {
            return Err(PublishError::Config("unsupported publishing credential schema".into()));
        }
        let required = |key: &str| map.get(key).and_then(|v| v.kind().as_str()).filter(|v| !v.is_empty()).map(str::to_string)
            .ok_or_else(|| PublishError::Config(format!("publishing credentials require {key}")));
        let endpoint = required("endpoint")?;
        validate_endpoint(&endpoint)?;
        Ok(Some(Self { endpoint: endpoint.trim_end_matches('/').into(), coordinator_token: required("coordinator_token")?, access_key_id: required("access_key_id")?, secret_access_key: required("secret_access_key")?, region: required("region")? }))
    }
}

fn validate_endpoint(endpoint: &str) -> Result<(), PublishError> {
    let uri: h12tiny_client_sync::http::Uri = endpoint.parse().map_err(|_| PublishError::Config("invalid publishing endpoint".into()))?;
    let loopback = matches!(uri.host(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if uri.host().is_none() || uri.authority().is_some_and(|v| v.as_str().contains('@'))
        || uri.query().is_some() || uri.path() != "/" && !uri.path().is_empty()
        || uri.scheme_str() != Some("https") && !(loopback && uri.scheme_str() == Some("http")) {
        return Err(PublishError::Config("publishing endpoint requires HTTPS without credentials, path or query".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn owner_credentials_load_without_global_environment_and_reject_leaks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials.json");
        let env = Environment::from_pairs([("STUFF_CONFIG_HOME", directory.path().to_str().unwrap())]);
        assert!(PublishingService::configured(&env).unwrap().is_none());
        let secret = "test-secret-must-not-appear";
        fs::write(&path, format!(r#"{{"schema":1,"endpoint":"https://stuff.example","coordinator_token":"{secret}","access_key_id":"service","secret_access_key":"{secret}","region":"auto"}}"#)).unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            let error = PublishingService::configured(&env).err().unwrap().to_string();
            assert!(error.contains("mode 600"));
            assert!(!error.contains(secret));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let service = PublishingService::configured(&env).unwrap().unwrap();
        assert_eq!(service.endpoint, "https://stuff.example");
        assert_eq!(service.coordinator_token, secret);
        fs::write(&path, format!(r#"{{"schema":9,"secret_access_key":"{secret}"}}"#)).unwrap();
        let error = PublishingService::configured(&env).err().unwrap().to_string();
        assert!(!error.contains(secret));
        assert!(error.contains("schema"));
        fs::write(&path, format!(r#"{{"schema":1,"endpoint":"https://user:{secret}@stuff.example","coordinator_token":"t","access_key_id":"a","secret_access_key":"s","region":"auto"}}"#)).unwrap();
        assert!(!PublishingService::configured(&env).err().unwrap().to_string().contains(secret));
    }
    #[test]
    fn partial_service_configuration_cannot_fall_back_to_backend_credentials() {
        let env = Environment::from_pairs([("STUFF_COORDINATOR_URL", "https://stuff.example"), ("R2_S3_SECRET_ACCESS_KEY", "backend")]);
        assert!(PublishingService::configured(&env).is_err());
        for url in ["http://stuff.example", "https://stuff.example/path", "https://stuff.example/?token=secret"] { assert!(validate_endpoint(url).is_err()); }
    }
}
