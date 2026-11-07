use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};
use stuff_store::{HashAlg, Site, sync_dir, sync_immutable_file, write_file_durable};

use super::adopt::{NativeRecord, UpstreamRecord};
use super::render::{render, sha256_hex};
use super::server::{PublishServer, ServerConfig};
use super::snapshot::{self, LiveSnapshot, merge_native, merge_upstream};
use super::store::Store;
use super::{Limits, Rejection, RejectionKind};
use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials::resolve;
use crate::publish::staging::Staging;
use crate::publish::state::CargoLayout;
use crate::publish::transaction::{Transaction, TransactionOptions};
use crate::publish::{CargoPublish, Environment, Options, Outcome, PublishError, Registry};

pub const DEFAULT_REGISTRY: &str = "stuff";
const SERVER_SITE_DIRECTORY: &str = "server-site";

pub fn publish(
    request: &CargoPublish,
    options: Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    if request.packages.is_empty() {
        return Err(PublishError::Usage(
            "publish cargo requires at least one -p NAME".to_string(),
        ));
    }
    let registry = request.registry.as_deref().unwrap_or(DEFAULT_REGISTRY);
    validate_registry_name(registry)?;

    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let layout = CargoLayout::from_environment(environment);
    layout.prepare(environment)?;
    let credentials = resolve(environment, &config.cargo.bucket)?;
    let staging = if options.dry_run {
        Some(Staging::create(&layout.cache_dir, "cargo-dry-run")?)
    } else {
        None
    };
    let (work_state, site_dir, server_site, transaction_state, snapshot_cache, cargo_home) =
        match &staging {
            Some(staging) => (
                staging.join("state"),
                staging.join("site"),
                staging.join(SERVER_SITE_DIRECTORY),
                staging.join("transaction"),
                staging.join("cache"),
                staging.join("cargo-home"),
            ),
            None => (
                layout.state_dir.clone(),
                layout.site_dir.clone(),
                layout.cache_dir.join(SERVER_SITE_DIRECTORY),
                layout.state_dir.clone(),
                layout.cache_dir.clone(),
                layout.cargo_home.clone(),
            ),
        };
    fs::create_dir_all(&cargo_home).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            cargo_home.display()
        ))
    })?;

    let pending = Transaction::begin(
        credentials,
        Registry::Cargo,
        &config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    let live = snapshot::load(pending.snapshot(), &config)?;
    let shared = pending
        .snapshot()
        .shared_stores()
        .first()
        .cloned()
        .ok_or_else(|| PublishError::Config("cargo has no shared blob store".to_string()))?;
    live.seed_server_site(&server_site, &config.cargo.public_base_url, false)?;

    let server = PublishServer::start(ServerConfig {
        state_dir: work_state.clone(),
        site_dir: server_site,
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
            environment,
        )?;
    }
    drop(server);

    let store = Store::new(&layout.state_dir);
    let work_store = (options.dry_run && work_state != layout.state_dir)
        .then(|| Store::new(&work_state));
    let mut blob_stores = vec![store.clone()];
    if let Some(work_store) = &work_store {
        blob_stores.push(work_store.clone());
    }
    store
        .with_lock(|| {
            let mut local_native = store.load_native_records()?;
            if let Some(work_store) = &work_store {
                local_native.extend(work_store.load_native_records()?);
            }
            let native = merge_native(local_native, live.native().to_vec())
                .map_err(publish_to_rejection)?;
            let upstream = merge_upstream(store.load_upstream_records()?, live.upstream().to_vec())
                .map_err(publish_to_rejection)?;
            SiteBuild {
                snapshot: pending.snapshot(),
                shared: &shared,
                site_dir: &site_dir,
                native: &native,
                upstream: &upstream,
                stores: &blob_stores,
                live: &live,
                public_base_url: &config.cargo.public_base_url,
                auth_required: false,
            }
            .run()
            .map_err(publish_to_rejection)
        })
        .map_err(rejection_error)?;

    let transaction = pending.stage_complete(site_dir)?;
    Ok(transaction.run()?.outcome())
}

struct SiteBuild<'a> {
    snapshot: &'a crate::publish::snapshot::Snapshot,
    shared: &'a crate::publish::snapshot::SharedStore,
    site_dir: &'a Path,
    native: &'a [NativeRecord],
    upstream: &'a [UpstreamRecord],
    stores: &'a [Store],
    live: &'a LiveSnapshot,
    public_base_url: &'a str,
    auth_required: bool,
}

impl SiteBuild<'_> {
    fn run(&self) -> Result<(), PublishError> {
        let site_dir = self.site_dir;
        let blobs_dir = site_dir.join("blobs");
        let site = Site::new(site_dir.to_path_buf());
        site.recover_staging().map_err(store_error)?;
        site.check_no_symlinks_in_path().map_err(store_error)?;
        site.ensure_layout().map_err(store_error)?;
        fs::create_dir_all(&blobs_dir).map_err(|error| {
            PublishError::Config(format!(
                "could not create {}: {error}",
                blobs_dir.display()
            ))
        })?;
        sync_dir(&blobs_dir).map_err(store_error)?;
        for checksum in self
            .native
            .iter()
            .map(|record| record.entry.cksum.as_str())
            .chain(self.upstream.iter().map(|record| record.checksum.as_str()))
        {
            let destination = blobs_dir.join(format!("{checksum}.crate"));
            if destination.is_file() {
                let actual = stuff_store::stream_file_hash(&destination, HashAlg::Sha256)
                    .map_err(store_error)?;
                if actual != checksum {
                    return Err(PublishError::Integrity(format!(
                        "served blob {} is corrupt: expected {checksum}, got {actual}",
                        destination.display()
                    )));
                }
                continue;
            }
            match self
                .stores
                .iter()
                .map(|store| store.blob_path(checksum))
                .find(|path| path.is_file())
            {
                Some(source) => {
                    sync_immutable_file(&source, &destination, checksum, HashAlg::Sha256)
                        .map_err(store_error)?;
                }
                None if self.live.has_blob(checksum) => {
                    let key = format!("{}{checksum}.crate", self.shared.prefix);
                    let bytes = self.snapshot.fetch_shared(self.shared, &key)?;
                    if sha256_hex(&bytes) != checksum {
                        return Err(PublishError::Integrity(format!(
                            "shared blob {key} does not hash to its name"
                        )));
                    }
                    write_file_durable(&destination, &bytes).map_err(store_error)?;
                }
                None => {
                    return Err(PublishError::Integrity(format!(
                        "missing blob {checksum}: it is neither in the local state nor in the live generation"
                    )));
                }
            }
        }
        let rendered = render(
            self.native,
            self.upstream,
            self.public_base_url,
            self.auth_required,
        )
        .map_err(rejection_error)?;
        site.publish(
            &rendered.generation_id,
            &|staging| {
                for ((view, relative), data) in &rendered.files {
                    let target = if view.is_empty() {
                        staging.join(relative)
                    } else {
                        staging.join(view).join(relative)
                    };
                    write_file_durable(&target, data)?;
                }
                Ok(())
            },
            &|generation| {
                for ((view, relative), expected) in &rendered.manifest {
                    let path = if view.is_empty() {
                        generation.join(relative)
                    } else {
                        generation.join(view).join(relative)
                    };
                    match fs::read(&path) {
                        Ok(data) if &sha256_hex(&data) == expected => {}
                        _ => return Ok(false),
                    }
                }
                Ok(true)
            },
        )
        .map_err(store_error)?;
        Ok(())
    }
}

fn cargo_publish(
    package: &str,
    manifest_path: Option<&str>,
    registry: &str,
    base_url: &str,
    token: &str,
    cargo_home: &Path,
    environment: &Environment,
) -> Result<(), PublishError> {
    let suffix = registry.to_ascii_uppercase().replace('-', "_");
    let mut command = Command::new("cargo");
    command
        .env_clear()
        .envs(environment.variables())
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

fn publish_to_rejection(error: PublishError) -> Rejection {
    match error {
        PublishError::Usage(detail) | PublishError::Config(detail) => {
            Rejection::new(RejectionKind::Input, detail)
        }
        PublishError::Conflict(detail) => Rejection::new(RejectionKind::Conflict, detail),
        PublishError::Integrity(detail) => Rejection::new(RejectionKind::Integrity, detail),
        PublishError::Transport(detail) => Rejection::new(RejectionKind::Network, detail),
        PublishError::Unavailable { .. } => {
            Rejection::new(RejectionKind::Operation, error.to_string())
        }
    }
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

fn store_error(error: stuff_store::Error) -> PublishError {
    use stuff_store::Error;
    match error {
        Error::Input(message) => PublishError::Usage(message),
        Error::Integrity(message) => PublishError::Integrity(message),
        Error::Conflict(message) => PublishError::Conflict(message),
        Error::Limit(message) => PublishError::Usage(message),
        Error::Network(message) => PublishError::Transport(message),
        Error::Operation(message) => PublishError::Integrity(message),
    }
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
