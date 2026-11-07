use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

use super::publish::Publisher;
use super::server::{PublishServer, ServerConfig};
use super::store::Store;
use super::{Limits, Rejection, RejectionKind};
use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials::{Environment, resolve};
use crate::publish::transaction::{Transaction, TransactionOptions};
use crate::publish::{CargoPublish, Options, Outcome, PublishError, Registry};

pub const DEFAULT_REGISTRY: &str = "stuff";

pub fn publish(request: &CargoPublish, options: Options) -> Result<Outcome, PublishError> {
    if request.packages.is_empty() {
        return Err(PublishError::Usage(
            "publish cargo requires at least one -p NAME".to_string(),
        ));
    }
    let registry = request.registry.as_deref().unwrap_or(DEFAULT_REGISTRY);
    validate_registry_name(registry)?;

    let paths = Paths::from_env(|name| std::env::var(name).ok());
    let config = Config::load(&paths.config_file())?;
    let state_dir = paths.state_dir(Registry::Cargo.as_str());
    let cache_dir = paths.cache_dir(Registry::Cargo.as_str());
    let site_dir = cache_dir.join("site");
    let r2_cache = cache_dir.join("r2");
    let cargo_home = cache_dir.join("cargo-home");
    std::fs::create_dir_all(&cargo_home).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            cargo_home.display()
        ))
    })?;

    let server = PublishServer::start(ServerConfig {
        state_dir: state_dir.clone(),
        site_dir: site_dir.clone(),
        public_base_url: config.cargo.public_base_url.clone(),
        auth_required: false,
        token: generate_token(),
        limits: Limits::default(),
    })?;
    for package in &request.packages {
        cargo_publish(
            package,
            request.manifest_path.as_deref(),
            registry,
            server.base_url(),
            server.token(),
            &cargo_home,
        )?;
    }
    drop(server);

    let store = Store::new(&state_dir);
    store
        .with_lock(|| {
            Publisher::new(
                &store,
                &site_dir,
                &config.cargo.public_base_url,
                false,
            )
            .publish()
        })
        .map_err(rejection_error)?;

    let credentials = resolve(&Environment::from_process(), &config.cargo.bucket)?;
    let transaction = Transaction::open(
        credentials,
        Registry::Cargo,
        &config,
        site_dir,
        state_dir,
        r2_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    Ok(transaction.run()?.outcome())
}

fn cargo_publish(
    package: &str,
    manifest_path: Option<&str>,
    registry: &str,
    base_url: &str,
    token: &str,
    cargo_home: &Path,
) -> Result<(), PublishError> {
    let suffix = registry.to_ascii_uppercase().replace('-', "_");
    let mut command = Command::new("cargo");
    command
        .arg("publish")
        .arg("--registry")
        .arg(registry)
        .arg("-p")
        .arg(package);
    if let Some(manifest_path) = manifest_path {
        command.arg("--manifest-path").arg(manifest_path);
    }
    command
        .env(
            format!("CARGO_REGISTRIES_{suffix}_INDEX"),
            format!("sparse+{base_url}/index/"),
        )
        .env(format!("CARGO_REGISTRIES_{suffix}_TOKEN"), token)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = command.output().map_err(|error| {
        PublishError::Usage(format!("could not run cargo publish: {error}"))
    })?;
    if output.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if already_published(&text, registry) {
        return Ok(());
    }
    Err(PublishError::Usage(format!(
        "cargo publish {package} failed:\n{}",
        text.trim_end()
    )))
}

fn already_published(text: &str, registry: &str) -> bool {
    let Some((_, rest)) = text.split_once("already exists on") else {
        return false;
    };
    rest.contains(registry) && rest.contains(" index")
}

fn validate_registry_name(registry: &str) -> Result<(), PublishError> {
    let valid = !registry.is_empty()
        && registry
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(PublishError::Usage(format!(
            "--registry must be a Cargo registry name, got {registry:?}"
        )))
    }
}

fn generate_token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let seed = format!("{}.{nanos}.cargo-publish", std::process::id());
    format!("{:x}", Sha256::digest(seed.as_bytes()))
}

fn rejection_error(rejection: Rejection) -> PublishError {
    let detail = message_of(&rejection);
    match rejection.kind {
        RejectionKind::Input | RejectionKind::Limit => PublishError::Usage(detail),
        RejectionKind::Integrity => PublishError::Integrity(detail),
        RejectionKind::Conflict => PublishError::Conflict(detail),
        RejectionKind::Network => PublishError::Transport(detail),
        RejectionKind::Operation => PublishError::Integrity(detail),
    }
}

fn message_of(rejection: &Rejection) -> String {
    let prefix = format!("{}: ", rejection.kind.prefix());
    rejection
        .detail
        .strip_prefix(&prefix)
        .unwrap_or(&rejection.detail)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_index_version_is_treated_as_a_skip() {
        let output = "error: crate `fixture` version `0.1.0` already exists on registry `stuff-e2e` index\n";
        assert!(already_published(output, "stuff-e2e"));
        assert!(!already_published(output, "other"));
        assert!(!already_published("error: failed to select a version", "stuff-e2e"));
    }

    #[test]
    fn registry_names_are_cargo_names() {
        for good in ["stuff", "stuff-e2e", "MYCRATES", "a_b"] {
            assert!(validate_registry_name(good).is_ok(), "{good}");
        }
        for bad in ["", "a b", "a/b", "a.b"] {
            let error = validate_registry_name(bad).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{bad}");
        }
    }
}
