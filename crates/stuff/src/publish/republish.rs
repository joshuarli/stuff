#![forbid(unsafe_code)]

use std::path::Path;

use deser_value::Value;
use stuff_oci::{parse_blobs_json, Digest};

use crate::publish::cargo::render::{render as render_cargo, sha256_hex, Rendered};
use crate::publish::cargo::snapshot as cargo_snapshot;
use crate::publish::cargo::Rejection;
use crate::publish::config::Config;
use crate::publish::inventory::library_error;
use crate::publish::npm::library_config;
use crate::publish::oci::OciPaths;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::r2::promote;
use crate::publish::r2::pull::{self, PullOptions};
use crate::publish::r2::sync;
use crate::publish::r2::R2Client;
use crate::publish::snapshot::{Snapshot, SharedStore};
use crate::publish::staging::Staging;
use crate::publish::state::CargoLayout;
use crate::publish::transaction::{Transaction, TransactionOptions};
use crate::publish::{Environment, Options, Outcome, PublishError, Registry};

pub fn republish(
    registry: Registry,
    allow_drops: bool,
    manifest: Option<String>,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    match registry {
        Registry::Cargo => cargo(allow_drops, options, environment),
        Registry::Npm => npm(allow_drops, options, environment),
        Registry::Pypi => pypi(allow_drops, options, environment),
        Registry::Oci => oci(allow_drops, options, environment),
        Registry::Mirror => crate::publish::mirror::republish(allow_drops, manifest, options, environment),
    }
}

fn options_for(allow_drops: bool, options: &Options, coordinator: Option<crate::publish::coordinator::Coordinator>) -> TransactionOptions {
    TransactionOptions {
        coordinator,
        dry_run: options.dry_run,
        allow_drops,
        ..TransactionOptions::default()
    }
}

fn cargo(
    allow_drops: bool,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let layout = CargoLayout::from_environment(environment);
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let credentials = credentials::resolve(environment, &config.cf_profile, &config.cargo.bucket)?;
    let staging = Staging::create(&layout.cache_dir, "cargo-republish")?;
    let site_dir = staging.join("site");
    let snapshot_cache = layout.cache_dir.clone();
    let transaction_state = if options.dry_run {
        staging.join("transaction")
    } else {
        layout.state_dir.clone()
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Cargo,
        &config,
        transaction_state,
        snapshot_cache,
        options_for(allow_drops, options, crate::publish::coordinator::Coordinator::configured(environment, Registry::Cargo)?),
    )?
    .against_live_generation();
    build_cargo_site(pending.snapshot(), &config, &site_dir)?;
    let transaction = pending.stage_complete(site_dir)?;
    Ok(transaction.run()?.outcome())
}

fn build_cargo_site(
    snapshot: &Snapshot,
    config: &Config,
    site_dir: &Path,
) -> Result<(), PublishError> {
    if !snapshot.metadata().contains_key("current") {
        return Err(PublishError::Usage(format!(
            "{} has no live publication to republish",
            config.cargo.bucket
        )));
    }
    let live = cargo_snapshot::load(snapshot, config)?;
    let shared = snapshot
        .shared_stores()
        .first()
        .cloned()
        .ok_or_else(|| PublishError::Config("cargo has no shared blob store".to_string()))?;
    let site = stuff_store::Site::new(site_dir.to_path_buf());
    site.recover_staging().map_err(store_error)?;
    site.check_no_symlinks_in_path().map_err(store_error)?;
    site.ensure_layout().map_err(store_error)?;
    let blobs_dir = site_dir.join("blobs");
    std::fs::create_dir_all(&blobs_dir).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", blobs_dir.display()))
    })?;
    for checksum in live
        .native()
        .iter()
        .map(|record| record.entry.cksum.as_str())
        .chain(live.upstream().iter().map(|record| record.checksum.as_str()))
    {
        materialize_cargo_blob(snapshot, &shared, &blobs_dir, checksum)?;
    }
    let rendered = render_cargo(
        live.native(),
        live.upstream(),
        &config.cargo.urls(),
        false,
    )
    .map_err(rejection_error)?;
    let generation_id = rendered.generation_id.clone();
    let Rendered { files, manifest, .. } = rendered;
    site.publish(
        &generation_id,
        &|staging| {
            for ((view, relative), data) in &files {
                let target = if view.is_empty() {
                    staging.join(relative)
                } else {
                    staging.join(view).join(relative)
                };
                stuff_store::write_file_durable(&target, data)?;
            }
            Ok(())
        },
        &|generation| {
            for ((view, relative), expected) in &manifest {
                let path = if view.is_empty() {
                    generation.join(relative)
                } else {
                    generation.join(view).join(relative)
                };
                match std::fs::read(&path) {
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

fn materialize_cargo_blob(
    snapshot: &Snapshot,
    shared: &SharedStore,
    blobs_dir: &Path,
    checksum: &str,
) -> Result<(), PublishError> {
    let destination = blobs_dir.join(format!("{checksum}.crate"));
    if destination.is_file() {
        let actual = stuff_store::stream_file_hash(&destination, stuff_store::HashAlg::Sha256)
            .map_err(store_error)?;
        if actual != checksum {
            return Err(PublishError::Integrity(format!(
                "served blob {} is corrupt: expected {checksum}, got {actual}",
                destination.display()
            )));
        }
        return Ok(());
    }
    if !snapshot
        .shared_objects(shared)
        .any(|object| object.key == format!("{}{checksum}.crate", shared.prefix))
    {
        return Err(PublishError::Integrity(format!(
            "live index references a blob that is not in the shared store: {checksum}"
        )));
    }
    let key = format!("{}{checksum}.crate", shared.prefix);
    let bytes = snapshot.fetch_shared(shared, &key)?;
    if sha256_hex(&bytes) != checksum {
        return Err(PublishError::Integrity(format!(
            "shared blob {key} does not hash to its name"
        )));
    }
    stuff_store::write_file_durable(&destination, &bytes).map_err(store_error)
}

fn npm(
    allow_drops: bool,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let bucket = config.npm.bucket.clone();
    let credentials = credentials::resolve(environment, &config.cf_profile, &bucket)?;
    let library = library_config(&config);
    let cache_dir = paths.cache_dir("npm");
    let state_dir = paths.publish_state_dir(Registry::Npm);
    let staging = Staging::create(&cache_dir, "npm-republish")?;
    let site_dir = staging.join("site");
    let work_state = staging.join("state");
    let snapshot_cache = cache_dir.clone();
    let transaction_state = if options.dry_run {
        staging.join("transaction")
    } else {
        state_dir
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Npm,
        &config,
        transaction_state,
        snapshot_cache,
        options_for(allow_drops, options, crate::publish::coordinator::Coordinator::configured(environment, Registry::Npm)?),
    )?
    .against_live_generation();
    let mirror = site_dir.join("mirror");
    pull::pull(pending.client(), &bucket, &mirror, PullOptions::default())?;
    materialize_objects(&mirror.join("tarballs"), &work_state)?;
    materialize_npm_aliases(&mirror)?;
    mynpm::publish::publish_site(&mynpm::publish::PublishOptions {
        state_directory: &work_state,
        site_directory: &site_dir,
        config: &library,
        render_base_url: None,
    })
    .map_err(library_error)?;
    let transaction = pending.stage_complete(&mirror)?;
    Ok(transaction.run()?.outcome())
}

fn materialize_objects(tarballs: &Path, state_dir: &Path) -> Result<(), PublishError> {
    let objects = state_dir.join("objects").join("sha512");
    std::fs::create_dir_all(&objects).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", objects.display()))
    })?;
    let entries = match std::fs::read_dir(tarballs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(PublishError::Config(format!(
                "could not read {}: {error}",
                tarballs.display()
            )));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            PublishError::Config(format!("could not read {}: {error}", tarballs.display()))
        })?;
        let name = entry.file_name();
        if !entry.path().is_file() || !name.to_string_lossy().ends_with(".tgz") {
            continue;
        }
        let target = objects.join(&name);
        if target.symlink_metadata().is_ok() {
            continue;
        }
        match std::fs::hard_link(entry.path(), &target) {
            Ok(()) => {}
            Err(_) => {
                std::fs::copy(entry.path(), &target).map(|_| ()).map_err(|error| {
                    PublishError::Config(format!(
                        "could not copy {} to {}: {error}",
                        entry.path().display(),
                        target.display()
                    ))
                })?;
            }
        }
    }
    Ok(())
}

fn materialize_npm_aliases(mirror: &Path) -> Result<(), PublishError> {
    let generation = sync::active_generation(mirror)?;
    let generation_dir = mirror.join("generations").join(&generation);
    let manifest_path = generation_dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
        PublishError::Integrity(format!("could not read {}: {error}", manifest_path.display()))
    })?;
    let value: Value = deser_json::from_str(&text).map_err(|error| {
        PublishError::Integrity(format!("{} is not valid JSON: {error}", manifest_path.display()))
    })?;
    let artifacts = value
        .as_map()
        .and_then(|map| map.get("artifacts"))
        .and_then(|artifacts| artifacts.as_seq())
        .ok_or_else(|| {
            PublishError::Integrity(format!("{} has no artifact list", manifest_path.display()))
        })?;
    let mut digests = Vec::with_capacity(artifacts.len());
    for artifact in artifacts.iter() {
        digests.push(
            mynpm::inventory::validate_artifact_record(artifact)
                .map_err(library_error)?
                .artifact
                .sha512,
        );
    }
    for relative in ["tarballs", "-/tarballs"] {
        let directory = generation_dir.join(relative);
        std::fs::create_dir_all(&directory).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", directory.display()))
        })?;
        for digest in &digests {
            let target = directory.join(format!("{digest}.tgz"));
            if target.symlink_metadata().is_ok() {
                continue;
            }
            let source = mirror.join("tarballs").join(format!("{digest}.tgz"));
            match std::fs::hard_link(&source, &target) {
                Ok(()) => {}
                Err(_) => {
                    std::fs::copy(&source, &target).map(|_| ()).map_err(|error| {
                        PublishError::Config(format!(
                            "could not link {} to {}: {error}",
                            source.display(),
                            target.display()
                        ))
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn pypi(
    allow_drops: bool,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let bucket = config.pypi.bucket.clone();
    let credentials = credentials::resolve(environment, &config.cf_profile, &bucket)?;
    let cache_dir = paths.cache_dir("pypi");
    let state_dir = paths.publish_state_dir(Registry::Pypi);
    let staging = Staging::create(&cache_dir, "pypi-republish")?;
    let site = staging.path().join("site");
    let snapshot_cache = cache_dir.clone();
    let transaction_state = if options.dry_run {
        staging.join("transaction")
    } else {
        state_dir
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Pypi,
        &config,
        transaction_state,
        snapshot_cache,
        options_for(allow_drops, options, crate::publish::coordinator::Coordinator::configured(environment, Registry::Pypi)?),
    )?
    .against_live_generation();
    pull::pull(pending.client(), &bucket, &site, PullOptions::default())?;
    mypypi::publish::republish(&mypypi::publish::RepublishOptions {
        output: &site,
        public_url: &config.pypi.public_base_url,
    })
    .map_err(map_mypypi)?;
    let transaction = pending.stage_complete(site)?;
    Ok(transaction.run()?.outcome())
}

fn oci(
    allow_drops: bool,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = OciPaths::from_environment(environment);
    let config = Config::load(&Paths::from_environment(environment).config_file())?;
    let credentials = credentials::resolve(environment, &config.cf_profile, &config.oci.bucket)?;
    let staging = Staging::create(&paths.cache_dir, "oci-republish")?;
    let snapshot_cache = paths.cache_dir.clone();
    let work = if options.dry_run {
        OciPaths {
            state_dir: staging.join("state"),
            site_dir: staging.join("site"),
            transaction_state_dir: staging.join("transaction"),
            cache_dir: paths.cache_dir.clone(),
        }
    } else {
        paths.clone()
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Oci,
        &config,
        work.transaction_state_dir.clone(),
        snapshot_cache,
        options_for(allow_drops, options, crate::publish::coordinator::Coordinator::configured(environment, Registry::Oci)?),
    )?
    .against_live_generation();
    rebuild_oci_state(pending.client(), &config, &work)?;
    let state = myoci::state::State::new(work.state_dir.clone());
    myoci::publish::publish(&state, &work.site_dir, &config.oci.public_base_url)
        .map_err(map_myoci_error)?;
    let transaction = pending.stage_complete(work.site_dir.clone())?;
    Ok(transaction.run()?.outcome())
}

fn rebuild_oci_state(
    client: &R2Client,
    config: &Config,
    paths: &OciPaths,
) -> Result<(), PublishError> {
    if promote::read_marker(client, &config.oci.bucket)?.is_none() {
        return Err(PublishError::Usage(format!(
            "{} has no live publication to republish",
            config.oci.bucket
        )));
    }
    remove_dir_if_present(&paths.site_dir)?;
    let report = pull::pull(
        client,
        &config.oci.bucket,
        &paths.site_dir,
        PullOptions::default(),
    )?;
    pull_generation_blobs(
        client,
        &config.oci.blobs_bucket,
        &paths.site_dir,
        &report.generation,
    )?;
    remove_dir_if_present(&paths.state_dir)?;
    let state = myoci::state::State::new(paths.state_dir.clone());
    myoci::publish::adopt(&state, &paths.site_dir).map_err(map_myoci_error)?;
    Ok(())
}

fn pull_generation_blobs(
    client: &R2Client,
    bucket: &str,
    site_dir: &Path,
    generation: &str,
) -> Result<(), PublishError> {
    let blobs_path = site_dir
        .join("generations")
        .join(generation)
        .join("blobs.json");
    let text = std::fs::read_to_string(&blobs_path).map_err(|error| {
        PublishError::Integrity(format!("could not read {}: {error}", blobs_path.display()))
    })?;
    let digests =
        parse_blobs_json(&text).map_err(|error| PublishError::Integrity(error.to_string()))?;
    let directory = site_dir.join("blobs").join("sha256");
    std::fs::create_dir_all(&directory).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", directory.display()))
    })?;
    for digest in digests {
        let bytes = client.get_object(bucket, &format!("sha256/{}", digest.hex()))?;
        let actual = Digest::from_bytes(&bytes);
        if actual != digest {
            return Err(PublishError::Integrity(format!(
                "blob {digest} hashes to {actual}"
            )));
        }
        let path = directory.join(digest.hex());
        std::fs::write(&path, &bytes).map_err(|error| {
            PublishError::Config(format!("could not write {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

fn remove_dir_if_present(path: &Path) -> Result<(), PublishError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PublishError::Config(format!(
            "could not remove {}: {error}",
            path.display()
        ))),
    }
}

fn map_mypypi(error: mypypi::error::Error) -> PublishError {
    match error.kind {
        mypypi::error::Kind::Conflict => PublishError::Conflict(error.to_string()),
        mypypi::error::Kind::Integrity => PublishError::Integrity(error.to_string()),
        mypypi::error::Kind::Network => PublishError::Transport(error.to_string()),
        mypypi::error::Kind::Io => PublishError::Config(error.to_string()),
        mypypi::error::Kind::InvalidInput
        | mypypi::error::Kind::Configuration
        | mypypi::error::Kind::MissingArtifact
        | mypypi::error::Kind::Incompatible
        | mypypi::error::Kind::Dependency
        | mypypi::error::Kind::Build
        | mypypi::error::Kind::Validation
        | mypypi::error::Kind::Limit => PublishError::Usage(error.to_string()),
    }
}

fn map_myoci_error(error: myoci::Error) -> PublishError {
    match error {
        myoci::Error::Usage(detail) | myoci::Error::Input(detail) => PublishError::Usage(detail),
        myoci::Error::Integrity(detail) => PublishError::Integrity(detail),
        myoci::Error::Conflict(detail) => PublishError::Conflict(detail),
        myoci::Error::Limit(detail) => PublishError::Usage(detail),
        myoci::Error::Network(detail) => PublishError::Transport(detail),
        myoci::Error::Operation(detail) => PublishError::Integrity(detail),
    }
}

fn rejection_error(rejection: Rejection) -> PublishError {
    let detail = message_of(&rejection);
    match rejection.kind {
        crate::publish::cargo::RejectionKind::Input
        | crate::publish::cargo::RejectionKind::Limit => PublishError::Usage(detail),
        crate::publish::cargo::RejectionKind::Integrity => PublishError::Integrity(detail),
        crate::publish::cargo::RejectionKind::Conflict => PublishError::Conflict(detail),
        crate::publish::cargo::RejectionKind::Network => PublishError::Transport(detail),
        crate::publish::cargo::RejectionKind::Operation => PublishError::Integrity(detail),
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
    fn a_missing_directory_is_not_an_error_to_remove() {
        let work = tempfile::tempdir().unwrap();
        let missing = work.path().join("missing");
        remove_dir_if_present(&missing).unwrap();
        let existing = work.path().join("existing");
        std::fs::create_dir_all(&existing).unwrap();
        remove_dir_if_present(&existing).unwrap();
        assert!(!existing.exists());
    }

    #[test]
    fn tarballs_are_materialized_into_the_shared_objects_directory() {
        let work = tempfile::tempdir().unwrap();
        let tarballs = work.path().join("mirror/tarballs");
        std::fs::create_dir_all(&tarballs).unwrap();
        std::fs::write(tarballs.join("abc.tgz"), b"tarball").unwrap();
        std::fs::write(tarballs.join("ignored.txt"), b"nope").unwrap();
        let state = work.path().join("state");
        materialize_objects(&tarballs, &state).unwrap();
        assert_eq!(
            std::fs::read(state.join("objects/sha512/abc.tgz")).unwrap(),
            b"tarball"
        );
        assert!(!state.join("objects/sha512/ignored.txt").exists());
    }

    #[test]
    fn mypypi_and_myoci_errors_keep_the_p8_classification() {
        assert_eq!(
            map_mypypi(mypypi::error::Error::new(
                mypypi::error::Kind::Conflict,
                "x",
                mypypi::error::Stage::Publication,
            ))
            .exit_code(),
            3
        );
        assert_eq!(map_myoci_error(myoci::Error::Network("x".to_string())).exit_code(), 5);
    }
}
