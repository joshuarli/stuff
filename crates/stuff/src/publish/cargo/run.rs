use rustc_hash::FxHashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};
use stuff_store::{HashAlg, Site, sync_dir, sync_immutable_file, write_file_durable};

use super::adopt::{NativeRecord, UpstreamRecord};
use super::render::{CargoUrls, render, sha256_hex};
use super::server::{PublishServer, ServerConfig};
use super::snapshot::{self, LiveSnapshot, merge_native, merge_upstream, reset_directory};
use super::store::Store;
use super::{Limits, Rejection, RejectionKind};
use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::client::ObjectMeta;
use crate::publish::r2::credentials::resolve;
use crate::publish::staging::Staging;
use crate::publish::state::CargoLayout;
use crate::publish::snapshot::SharedStore;
use crate::publish::transaction::{Pending, Staged, Transaction, TransactionOptions};
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

    let Begun { config, layout, staging, work_state, site_dir, pending, live, shared } =
        begin(options, environment)?;
    let (server_site, cargo_home) = match &staging {
        Some(staging) => (staging.join(SERVER_SITE_DIRECTORY), staging.join("cargo-home")),
        None => (layout.cache_dir.join(SERVER_SITE_DIRECTORY), layout.cargo_home.clone()),
    };
    fs::create_dir_all(&cargo_home).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            cargo_home.display()
        ))
    })?;
    reset_directory(&server_site)?;
    let published_store = Store::new(&layout.state_dir);
    published_store
        .with_lock(|| {
            let native = merge_native(
                published_store.load_native_records()?,
                live.native().to_vec(),
            )
            .map_err(publish_to_rejection)?;
            let upstream = merge_upstream(
                published_store.load_upstream_records()?,
                live.upstream().to_vec(),
            )
            .map_err(publish_to_rejection)?;
            SiteBuild {
                snapshot: pending.snapshot(),
                shared: &shared,
                site_dir: &server_site,
                native: &native,
                upstream: &upstream,
                stores: std::slice::from_ref(&published_store),
                live: &live,
                urls: &config.cargo.urls(),
                auth_required: false,
            }
            .run()
            .map_err(publish_to_rejection)
            .map(drop)
        })
        .map_err(rejection_error)?;

    let server = PublishServer::start(ServerConfig {
        state_dir: work_state.clone(),
        published_state_dir: layout.state_dir.clone(),
        site_dir: server_site,
        urls: config.cargo.urls(),
        auth_required: false,
        token: generate_token(),
        limits: Limits::default(),
    })?;
    let command = CargoPublishCommand {
        manifest_path: request.manifest_path.as_deref(),
        registry,
        base_url: server.base_url(),
        token: server.token(),
        cargo_home: &cargo_home,
        environment,
    };
    let workspace = command.workspace()?;
    let lockfile = workspace.root.join("Cargo.lock");
    let lock_before = match fs::read(&lockfile) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(PublishError::Config(format!(
                "could not read {}: {error}",
                lockfile.display()
            )));
        }
    };
    let mut released_earlier: Vec<(String, String)> = Vec::new();
    let mut published = Ok(());
    for package in &request.packages {
        published = command.run(package, &released_earlier);
        if published.is_err() {
            break;
        }
        if let Some(name) = crate_name_of(package)
            && let Some(version) = workspace.versions.get(name)
        {
            released_earlier.push((name.to_string(), version.clone()));
        }
    }
    if let Some(bytes) = lock_before {
        let restored = fs::write(&lockfile, bytes).map_err(|error| {
            PublishError::Config(format!("could not restore {}: {error}", lockfile.display()))
        });
        published = published.and(restored);
    }
    published?;
    drop(server);

    let store = Store::new(&layout.state_dir);
    let work_store = (options.dry_run && work_state != layout.state_dir)
        .then(|| Store::new(&work_state));
    publish_generation(
        pending,
        Generation {
            config: &config,
            shared: &shared,
            site_dir: &site_dir,
            store: &store,
            work_store: work_store.as_ref(),
        },
        |native, _| package_ids(native, &request.packages),
    )
}

/// A started cargo publication: the transaction against the live generation, and where this
/// run writes. A dry run writes its state and site under `staging`, which is dropped afterwards.
pub(super) struct Begun {
    pub config: Config,
    pub layout: CargoLayout,
    pub staging: Option<Staging>,
    pub work_state: PathBuf,
    pub site_dir: PathBuf,
    pub pending: Pending,
    pub live: LiveSnapshot,
    pub shared: SharedStore,
}

pub(super) fn begin(options: Options, environment: &Environment) -> Result<Begun, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let layout = CargoLayout::from_environment(environment);
    layout.prepare(environment)?;
    let credentials = resolve(environment, &config.cf_profile, &config.cargo.bucket)?;
    let staging = if options.dry_run {
        Some(Staging::create(&layout.cache_dir, "cargo-dry-run")?)
    } else {
        None
    };
    let (work_state, site_dir, transaction_state, snapshot_cache) = match &staging {
        Some(staging) => (
            staging.join("state"),
            staging.join("site"),
            staging.join("transaction"),
            staging.join("cache"),
        ),
        None => (
            layout.state_dir.clone(),
            layout.site_dir.clone(),
            layout.state_dir.clone(),
            layout.cache_dir.clone(),
        ),
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Cargo,
        &config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            coordinator: crate::publish::coordinator::Coordinator::configured(environment, Registry::Cargo)?,
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
    Ok(Begun { config, layout, staging, work_state, site_dir, pending, live, shared })
}

/// The stores and site a cargo publication renders from: the canonical state, plus the dry-run
/// work store whose records and blobs never reach the canonical state.
pub(super) struct Generation<'a> {
    pub config: &'a Config,
    pub shared: &'a SharedStore,
    pub site_dir: &'a Path,
    pub store: &'a Store,
    pub work_store: Option<&'a Store>,
}

/// Render the merged local and live records into a generation and promote it through `pending`.
pub(super) fn publish_generation(
    pending: Pending,
    generation: Generation<'_>,
    packages: impl Fn(&[NativeRecord], &[UpstreamRecord]) -> Vec<String>,
) -> Result<Outcome, PublishError> {
    let Generation { config, shared, site_dir, store, work_store } = generation;
    let mut blob_stores = vec![store.clone()];
    if let Some(work_store) = work_store {
        blob_stores.push(work_store.clone());
    }
    let mut published = Vec::new();
    let report = pending.publish(|pending| {
        let live = snapshot::load(pending.snapshot(), config)?;
        let known = store
            .with_lock(|| {
                let mut local_native = store.load_native_records()?;
                let mut local_upstream = store.load_upstream_records()?;
                if let Some(work_store) = work_store {
                    local_native.extend(work_store.load_native_records()?);
                    local_upstream.extend(work_store.load_upstream_records()?);
                }
                let native = merge_native(local_native, live.native().to_vec())
                    .map_err(publish_to_rejection)?;
                let upstream = merge_upstream(local_upstream, live.upstream().to_vec())
                    .map_err(publish_to_rejection)?;
                published = packages(&native, &upstream);
                SiteBuild {
                    snapshot: pending.snapshot(),
                    shared,
                    site_dir,
                    native: &native,
                    upstream: &upstream,
                    stores: &blob_stores,
                    live: &live,
                    urls: &config.cargo.urls(),
                    auth_required: false,
                }
                .run()
                .map_err(publish_to_rejection)
            })
            .map_err(rejection_error)?;
        Ok(Staged {
            site: site_dir.to_path_buf(),
            known,
        })
    })?;
    let mut outcome = report.outcome();
    outcome.packages = published;
    Ok(outcome)
}

fn package_ids(native: &[NativeRecord], requested: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = native
        .iter()
        .filter(|record| {
            requested
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&record.entry.name))
        })
        .map(|record| format!("{}@{}", record.entry.name, record.entry.vers))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

struct SiteBuild<'a> {
    snapshot: &'a crate::publish::snapshot::Snapshot,
    shared: &'a crate::publish::snapshot::SharedStore,
    site_dir: &'a Path,
    native: &'a [NativeRecord],
    upstream: &'a [UpstreamRecord],
    stores: &'a [Store],
    live: &'a LiveSnapshot,
    urls: &'a CargoUrls,
    auth_required: bool,
}

impl SiteBuild<'_> {
    fn run(&self) -> Result<FxHashMap<String, ObjectMeta>, PublishError> {
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
        let mut known = FxHashMap::default();
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
                    let listed = self.snapshot.metadata().get(&key).ok_or_else(|| {
                        PublishError::Integrity(format!(
                            "live generation references a blob that is not in the listing: {key}"
                        ))
                    })?;
                    known.insert(
                        key,
                        ObjectMeta {
                            size: listed.size,
                            etag: listed.etag.clone(),
                            content_type: None,
                        },
                    );
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
            self.urls,
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
        Ok(known)
    }
}

struct Workspace {
    root: PathBuf,
    versions: FxHashMap<String, String>,
}

struct CargoPublishCommand<'a> {
    manifest_path: Option<&'a str>,
    registry: &'a str,
    base_url: &'a str,
    token: &'a str,
    cargo_home: &'a Path,
    environment: &'a Environment,
}

impl CargoPublishCommand<'_> {
    fn registry_environment(&self, command: &mut Command) {
        let suffix = self.registry.to_ascii_uppercase().replace('-', "_");
        command
            .env(
                format!("CARGO_REGISTRIES_{suffix}_INDEX"),
                format!("sparse+{}/index/", self.base_url),
            )
            .env(format!("CARGO_REGISTRIES_{suffix}_TOKEN"), self.token)
            .env("CARGO_HOME", self.cargo_home)
            .env("CARGO_TERM_COLOR", "never");
    }

    fn workspace(&self) -> Result<Workspace, PublishError> {
        let mut command = Command::new("cargo");
        command
            .env_clear()
            .envs(self.environment.variables())
            .args(["metadata", "--no-deps", "--format-version", "1"]);
        self.registry_environment(&mut command);
        if let Some(manifest_path) = self.manifest_path {
            command.arg("--manifest-path").arg(manifest_path);
        }
        let output = command
            .stdin(Stdio::null())
            .output()
            .map_err(|error| PublishError::Usage(format!("could not run cargo metadata: {error}")))?;
        if !output.status.success() {
            return Err(PublishError::Usage(format!(
                "cargo metadata failed:\n{}",
                String::from_utf8_lossy(&output.stderr).trim_end()
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let malformed = || PublishError::Integrity("cargo metadata output is malformed".to_string());
        let value: deser_value::Value = deser_json::from_str(&text).map_err(|_| malformed())?;
        let packages = value
            .as_map()
            .and_then(|map| map.get("packages"))
            .and_then(|packages| packages.as_seq())
            .ok_or_else(malformed)?;
        let root = value
            .as_map()
            .and_then(|map| map.get("workspace_root"))
            .and_then(|root| root.as_str())
            .map(PathBuf::from)
            .ok_or_else(malformed)?;
        let mut versions = FxHashMap::default();
        for package in packages {
            let package = package.as_map().ok_or_else(malformed)?;
            let field = |name: &str| {
                package
                    .get(name)
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
                    .ok_or_else(malformed)
            };
            versions.insert(field("name")?, field("version")?);
        }
        Ok(Workspace { root, versions })
    }

    fn run(&self, package: &str, released_earlier: &[(String, String)]) -> Result<(), PublishError> {
        let registry = self.registry;
        let mut command = Command::new("cargo");
        command
            .env_clear()
            .envs(self.environment.variables())
            .arg("publish")
            .arg("--registry")
            .arg(registry)
            .arg("-p")
            .arg(package);
        for (name, version) in released_earlier {
            command
                .arg("--config")
                .arg(format!("patch.crates-io.{name}.registry=\"{registry}\""))
                .arg("--config")
                .arg(format!("patch.crates-io.{name}.version=\"={version}\""));
        }
        if let Some(manifest_path) = self.manifest_path {
            command.arg("--manifest-path").arg(manifest_path);
        }
        self.registry_environment(&mut command);
        command
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
}

fn crate_name_of(package_spec: &str) -> Option<&str> {
    let name = package_spec
        .split_once('@')
        .map_or(package_spec, |(name, _)| name);
    super::names::validate_crate_name(name).ok()?;
    Some(name)
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
    crate::publish::lower_hex(Sha256::digest(seed.as_bytes()))
}

pub(super) fn publish_to_rejection(error: PublishError) -> Rejection {
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

pub(super) fn rejection_error(rejection: Rejection) -> PublishError {
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
