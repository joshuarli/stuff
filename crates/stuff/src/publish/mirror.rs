#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use mymirror::{Acquire, HttpsAcquire};

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::r2::promote;
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
    let report = pending.publish(|pending| {
        let generation = mymirror::generation_id(&manifest, &base_url)
            .map_err(|error| PublishError::Usage(error.to_string()))?;
        let marker = promote::read_marker(pending.client(), &bucket)?;
        let live = marker.as_ref().map(|marker| marker.generation.as_str());
        let expected_keys = expected_generation_keys(&manifest, &generation);
        let remote = pending.snapshot().metadata();
        if live == Some(generation.as_str())
            && expected_keys.iter().all(|key| remote.contains_key(key))
        {
            ensure_current(&site_dir, &generation)?;
            prune_stale_generations(&site_dir)?;
            packages = manifest
                .artifacts
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect();
            let known = expected_keys
                .into_iter()
                .filter_map(|key| {
                    remote.get(&key).map(|object| {
                        (
                            key,
                            crate::publish::r2::client::ObjectMeta {
                                size: object.size,
                                etag: object.etag.clone(),
                                content_type: None,
                            },
                        )
                    })
                })
                .collect();
            return Ok(Staged {
                site: site_dir.clone(),
                known,
            });
        }
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

fn expected_generation_keys(manifest: &mymirror::manifest::Manifest, generation: &str) -> Vec<String> {
    let mut keys = vec![
        format!("generations/{generation}/catalog.json"),
        format!("generations/{generation}/SHA256SUMS"),
        format!("manifests/{generation}.json"),
    ];
    for artifact in &manifest.artifacts {
        keys.push(format!("generations/{generation}/{}.zip", artifact.id));
    }
    keys
}

fn ensure_current(site_dir: &Path, generation: &str) -> Result<(), PublishError> {
    let generations = site_dir.join("generations").join(generation);
    std::fs::create_dir_all(&generations).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", generations.display()))
    })?;
    let current = site_dir.join("current");
    let target = format!("generations/{generation}");
    let up_to_date = std::fs::read_link(&current)
        .map(|existing| existing.to_string_lossy() == target)
        .unwrap_or(false);
    if up_to_date {
        return Ok(());
    }
    if let Err(error) = std::fs::remove_file(&current)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(PublishError::Config(format!(
            "could not remove {}: {error}",
            current.display()
        )));
    }
    std::os::unix::fs::symlink(&target, &current).map_err(|error| {
        PublishError::Config(format!("could not link {} to {}: {error}", current.display(), target))
    })?;
    Ok(())
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
