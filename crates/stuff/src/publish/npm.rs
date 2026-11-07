#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use mynpm::config::{Config as LibraryConfig, DarwinTarget, LinuxTarget, Policy, Target};
use mynpm::inventory::SourceKind;
use mynpm::inventory_store::load_inventory;
use mynpm::publish::{publish_site, PublishOptions};

use crate::publish::config::Config;
use crate::publish::inventory::{library_error, prepare, PrepareOptions};
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::staging::Staging;
use crate::publish::transaction::{Staged, Transaction, TransactionOptions};
use crate::publish::{Environment, NpmPublish, Options, Outcome, PublishError, Registry};

const NODE_VERSION: &str = "26.12.0";
const SITE_DIRECTORY: &str = "site";

pub fn library_config(config: &Config) -> LibraryConfig {
    let mut public_base_url = config.npm.public_base_url.clone();
    if !public_base_url.ends_with('/') {
        public_base_url.push('/');
    }
    LibraryConfig {
        public_base_url,
        upstream_registry: "https://registry.npmjs.org/".to_string(),
        targets: vec![
            Target::Darwin(DarwinTarget {
                id: "mac-arm64".to_string(),
                cpu: "arm64".to_string(),
                node: NODE_VERSION.to_string(),
            }),
            Target::Darwin(DarwinTarget {
                id: "mac-x64".to_string(),
                cpu: "x64".to_string(),
                node: NODE_VERSION.to_string(),
            }),
            Target::Linux(LinuxTarget {
                id: "linux-x64-gnu".to_string(),
                cpu: "x64".to_string(),
                libc: "glibc".to_string(),
                node: NODE_VERSION.to_string(),
            }),
            Target::Linux(LinuxTarget {
                id: "linux-x64-musl".to_string(),
                cpu: "x64".to_string(),
                libc: "musl".to_string(),
                node: NODE_VERSION.to_string(),
            }),
            Target::Linux(LinuxTarget {
                id: "linux-arm64-gnu".to_string(),
                cpu: "arm64".to_string(),
                libc: "glibc".to_string(),
                node: NODE_VERSION.to_string(),
            }),
            Target::Linux(LinuxTarget {
                id: "linux-arm64-musl".to_string(),
                cpu: "arm64".to_string(),
                libc: "musl".to_string(),
                node: NODE_VERSION.to_string(),
            }),
        ],
        policy: Policy { allow_legacy_sha1_only: false },
    }
}

fn resolve_lockfile(lockfile: &str) -> Result<PathBuf, PublishError> {
    let path = PathBuf::from(lockfile);
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| PublishError::Config(format!("could not resolve the working directory: {error}")))
}

fn prune_stale_generations(mirror: &Path) -> Result<(), PublishError> {
    let current = std::fs::read_link(mirror.join("current")).map_err(|error| {
        PublishError::Integrity(format!("could not read the site current link: {error}"))
    })?;
    let target = current.to_string_lossy();
    let generation = target.strip_prefix("generations/").unwrap_or(&target).to_string();
    let directory = mirror.join("generations");
    let entries = std::fs::read_dir(&directory).map_err(|error| {
        PublishError::Config(format!("could not read {}: {error}", directory.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", directory.display()))
        })?;
        if entry.file_name().to_string_lossy() == generation {
            continue;
        }
        if entry
            .file_type()
            .map_err(|error| PublishError::Config(format!("could not inspect {}: {error}", entry.path().display())))?
            .is_dir()
        {
            std::fs::remove_dir_all(entry.path()).map_err(|error| {
                PublishError::Config(format!("could not remove {}: {error}", entry.path().display()))
            })?;
        }
    }
    Ok(())
}

pub fn publish(
    request: &NpmPublish,
    options: Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let bucket = config.npm.bucket.clone();
    let credentials = credentials::resolve(environment, &bucket)?;
    let library = library_config(&config);

    let cache_dir = paths.cache_dir("npm");
    let state_dir = paths.state_dir("npm");
    let staging = if options.dry_run {
        Some(Staging::create(&cache_dir, "npm-dry-run")?)
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
            state_dir.clone(),
            cache_dir.join(SITE_DIRECTORY),
            state_dir.clone(),
            cache_dir,
        ),
    };
    let mirror = site_dir.join("mirror");
    let lockfile = resolve_lockfile(&request.lockfile)?;

    let pending = Transaction::begin(
        credentials,
        Registry::Npm,
        &config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    if options.dry_run {
        copy_inventory(&state_dir, &work_state)?;
    }
    let store = pending
        .snapshot()
        .shared_stores()
        .first()
        .cloned()
        .ok_or_else(|| PublishError::Config("npm has no shared tarball store".to_string()))?;
    let report = pending.publish(|pending| {
        prepare(
            pending.snapshot(),
            &PrepareOptions {
                bucket: &bucket,
                store: &store,
                site_dir: &site_dir,
                state_dir: &work_state,
                lockfile: &lockfile,
                project_id: &request.project,
                library: &library,
            },
        )?;

        publish_site(&PublishOptions {
            state_directory: &work_state,
            site_directory: &site_dir,
            config: &library,
            render_base_url: None,
        })
        .map_err(library_error)?;
        prune_stale_generations(&mirror)?;
        Ok(Staged::new(mirror.clone()))
    })?;
    let mut outcome = report.outcome();
    outcome.packages = package_ids(&work_state, &request.project)?;
    Ok(outcome)
}

fn package_ids(state_dir: &Path, project: &str) -> Result<Vec<String>, PublishError> {
    let inventory = load_inventory(state_dir).map_err(library_error)?;
    let mut ids = Vec::new();
    for record in &inventory.projects {
        if record.id != project {
            continue;
        }
        for package in &record.packages {
            if package.source_kind != SourceKind::Registry {
                continue;
            }
            if let Some(version) = &package.version {
                ids.push(format!("{}@{}", package.name, version));
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn copy_inventory(from: &Path, to: &Path) -> Result<(), PublishError> {
    let source = from.join("inventory.json");
    if !source.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(to).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", to.display()))
    })?;
    let target = to.join("inventory.json");
    std::fs::copy(&source, &target).map(|_| ()).map_err(|error| {
        PublishError::Config(format!(
            "could not copy {} to {}: {error}",
            source.display(),
            target.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_config_ends_the_public_url_with_a_slash() {
        let mut config = Config::default();
        config.npm.public_base_url = "https://npm.example.test".to_string();
        let library = library_config(&config);
        assert_eq!(library.public_base_url, "https://npm.example.test/");
        assert_eq!(library.upstream_registry, "https://registry.npmjs.org/");
        let ids: Vec<&str> = library.targets.iter().map(|target| target.id()).collect();
        assert_eq!(
            ids,
            [
                "mac-arm64",
                "mac-x64",
                "linux-x64-gnu",
                "linux-x64-musl",
                "linux-arm64-gnu",
                "linux-arm64-musl"
            ]
        );
    }

    #[test]
    fn the_public_url_keeps_an_existing_slash() {
        let library = library_config(&Config::default());
        assert_eq!(library.public_base_url, "https://npm.17166969.xyz/");
    }

    #[test]
    fn stale_generations_are_removed_but_the_current_one_survives() {
        let dir = tempfile::tempdir().unwrap();
        let mirror = dir.path();
        for generation in ["keep", "drop"] {
            let directory = mirror.join("generations").join(generation);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("catalog.json"), b"{}").unwrap();
        }
        std::os::unix::fs::symlink("generations/keep", mirror.join("current")).unwrap();
        prune_stale_generations(mirror).unwrap();
        assert!(mirror.join("generations/keep/catalog.json").is_file());
        assert!(!mirror.join("generations/drop").exists());
    }
}
