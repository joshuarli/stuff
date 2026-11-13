#![forbid(unsafe_code)]

use std::path::PathBuf;

use mymirror::{Acquire, HttpsAcquire};

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::staging::{prune_stale_generations, Staging};
use crate::publish::transaction::{Staged, Transaction, TransactionOptions};
use crate::publish::{Environment, MirrorPublish, Options, Outcome, PublishError, Registry};

const SITE_DIRECTORY: &str = "site";
const DEFAULT_MANIFEST: &str = "mirror/manifest.toml";

pub fn publish(
    request: &MirrorPublish,
    options: Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let acquire = HttpsAcquire::new();
    publish_with_acquire(request, options, environment, &acquire)
}

pub fn publish_with_acquire(
    request: &MirrorPublish,
    options: Options,
    environment: &Environment,
    acquire: &dyn Acquire,
) -> Result<Outcome, PublishError> {
    run_publication(request, options, environment, acquire, false)
}

pub(crate) fn republish(
    allow_drops: bool,
    manifest: Option<String>,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let acquire = HttpsAcquire::new();
    republish_with_acquire(allow_drops, manifest, options, environment, &acquire)
}

pub fn republish_with_acquire(
    allow_drops: bool,
    manifest: Option<String>,
    options: &Options,
    environment: &Environment,
    acquire: &dyn Acquire,
) -> Result<Outcome, PublishError> {
    let request = MirrorPublish { manifest, allow_drops };
    run_publication(&request, *options, environment, acquire, true)
}

fn run_publication(
    request: &MirrorPublish,
    options: Options,
    environment: &Environment,
    acquire: &dyn Acquire,
    against_live: bool,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let bucket = config.mirror.bucket.clone();
    let base_url = config.mirror.public_base_url.trim_end_matches('/').to_string();
    let manifest_path = resolve_manifest(request.manifest.as_deref())?;
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|error| {
        PublishError::Config(format!("could not read {}: {error}", manifest_path.display()))
    })?;
    let manifest = mymirror::manifest::Manifest::parse(&manifest_text)
        .map_err(|error| PublishError::Usage(error.to_string()))?;
    let credentials = credentials::resolve(environment, &config.cf_profile, &bucket)?;

    let cache_dir = paths.cache_dir("mirror");
    let state_dir = paths.publish_state_dir(Registry::Mirror);
    let dry_run_staging = if options.dry_run {
        Some(Staging::create(&cache_dir, "mirror-dry-run")?)
    } else {
        None
    };
    let (site_dir, snapshot_cache, transaction_state, artifacts_dir) = match &dry_run_staging {
        Some(staging) => (
            staging.join(SITE_DIRECTORY),
            staging.join("cache"),
            staging.join("transaction"),
            staging.join("artifacts"),
        ),
        None => (
            cache_dir.join(SITE_DIRECTORY),
            cache_dir.clone(),
            state_dir.clone(),
            cache_dir.join("artifacts"),
        ),
    };
    if artifacts_dir.exists() {
        std::fs::remove_dir_all(&artifacts_dir).map_err(|error| {
            PublishError::Config(format!("could not remove {}: {error}", artifacts_dir.display()))
        })?;
    }
    let objects = cache_dir.join("objects");
    let pending = Transaction::begin(
        credentials,
        Registry::Mirror,
        &config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            allow_drops: request.allow_drops,
            ..TransactionOptions::default()
        },
    )?;
    let pending = if against_live {
        pending.against_live_generation()
    } else {
        pending
    };
    let mut packages = Vec::new();
    let report = pending.publish(|_pending| {
        let built = mymirror::build(&manifest, &base_url, acquire, Some(&objects), &artifacts_dir)
            .map_err(mirror_error)?;
        mymirror::assemble(&site_dir, &built).map_err(mirror_error)?;
        packages = built.artifacts.iter().map(|artifact| artifact.id.clone()).collect();
        prune_stale_generations(&site_dir)?;
        Ok(Staged::new(site_dir.clone()))
    })?;
    let mut outcome = report.outcome();
    outcome.packages = packages;
    Ok(outcome)
}

fn resolve_manifest(manifest: Option<&str>) -> Result<PathBuf, PublishError> {
    let path = PathBuf::from(manifest.unwrap_or(DEFAULT_MANIFEST));
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| {
                PublishError::Config(format!("could not resolve the working directory: {error}"))
            })?
            .join(path)
    };
    if !absolute.is_file() {
        return Err(PublishError::Config(format!(
            "mirror manifest not found at {}; pass --manifest PATH",
            absolute.display()
        )));
    }
    Ok(absolute)
}

fn mirror_error(error: mymirror::Error) -> PublishError {
    match error {
        mymirror::Error::Manifest(detail) => PublishError::Usage(detail),
        mymirror::Error::Transport(detail) => PublishError::Transport(detail),
        mymirror::Error::Integrity(detail) => PublishError::Integrity(detail),
        mymirror::Error::Archive(detail) => PublishError::Integrity(detail),
        mymirror::Error::Operation(detail) => PublishError::Integrity(detail),
    }
}
