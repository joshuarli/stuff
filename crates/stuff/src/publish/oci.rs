#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use stuff_oci::{parse_blobs_json, Digest, RepositoryName, Tag};

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::r2::pull::{self, PullOptions};
use crate::publish::r2::promote;
use crate::publish::r2::{Credentials, R2Client};
use crate::publish::staging::Staging;
use crate::publish::transaction::{Staged, Transaction, TransactionOptions};
use crate::publish::{Environment, OciSource, Options, Outcome, PublishError, Registry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OciPaths {
    pub state_dir: PathBuf,
    pub site_dir: PathBuf,
    pub transaction_state_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl OciPaths {
    pub fn from_environment(env: &Environment) -> Self {
        let paths = Paths::from_environment(env);
        let base = paths.state_dir("oci");
        OciPaths {
            state_dir: base.join("state"),
            site_dir: base.join("site"),
            transaction_state_dir: base.join("transaction"),
            cache_dir: paths.cache_dir("oci"),
        }
    }
}

pub struct Context {
    pub config: Config,
    pub credentials: Credentials,
    pub paths: OciPaths,
}

pub fn publish(
    source: &OciSource,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = OciPaths::from_environment(environment);
    let config = Config::load(&Paths::from_environment(environment).config_file())?;
    let credentials = credentials::resolve(environment, &config.oci.bucket)?;
    publish_with(
        source,
        options,
        &Context {
            config,
            credentials,
            paths,
        },
    )
}

pub fn publish_with(
    source: &OciSource,
    options: &Options,
    context: &Context,
) -> Result<Outcome, PublishError> {
    let staging = if options.dry_run {
        Some(Staging::create(&context.paths.cache_dir, "oci-dry-run")?)
    } else {
        None
    };
    let (paths, snapshot_cache) = match &staging {
        Some(staging) => (
            OciPaths {
                state_dir: staging.join("state"),
                site_dir: staging.join("site"),
                transaction_state_dir: staging.join("transaction"),
                cache_dir: context.paths.cache_dir.clone(),
            },
            staging.join("cache"),
        ),
        None => (context.paths.clone(), context.paths.cache_dir.clone()),
    };
    let pending = Transaction::begin(
        context.credentials.clone(),
        Registry::Oci,
        &context.config,
        paths.transaction_state_dir.clone(),
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    let report = pending.publish(|pending| {
        sync_live_state(
            pending.client(),
            &context.config.oci.bucket,
            &context.config.oci.blobs_bucket,
            &paths,
        )?;

        let state = myoci::state::State::new(paths.state_dir.clone());
        import_source(&state, source)?;
        myoci::publish::publish(&state, &paths.site_dir, &context.config.oci.public_base_url)
            .map_err(map_myoci_error)?;
        Ok(Staged::new(paths.site_dir.clone()))
    })?;
    let mut outcome = report.outcome();
    outcome.packages = package_ids(source);
    Ok(outcome)
}

fn package_ids(source: &OciSource) -> Vec<String> {
    let mut ids: Vec<String> = match source {
        OciSource::Images(images) => images
            .iter()
            .map(|image| format!("{}:{}", image.repository, image.reference))
            .collect(),
        OciSource::Layout {
            repository, tag, ..
        } => vec![format!(
            "{}:{}",
            repository,
            tag.as_deref().unwrap_or("latest")
        )],
    };
    ids.sort();
    ids.dedup();
    ids
}

fn sync_live_state(
    client: &R2Client,
    metadata_bucket: &str,
    blobs_bucket: &str,
    paths: &OciPaths,
) -> Result<(), PublishError> {
    let Some(marker) = promote::read_marker(client, metadata_bucket)? else {
        return Ok(());
    };
    let current = stuff_store::Site::new(paths.site_dir.clone())
        .current_generation()
        .map_err(|error| PublishError::Integrity(error.to_string()))?;
    if current.as_deref() != Some(marker.generation.as_str()) {
        remove_dir_if_present(&paths.site_dir)?;
        let report = pull::pull(
            client,
            metadata_bucket,
            &paths.site_dir,
            PullOptions::default(),
        )?;
        pull_generation_blobs(client, blobs_bucket, &paths.site_dir, &report.generation)?;
    }

    remove_dir_if_present(&paths.state_dir)?;
    let state = myoci::state::State::new(paths.state_dir.clone());
    myoci::publish::adopt(&state, &paths.site_dir).map_err(map_myoci_error)?;
    Ok(())
}

fn pull_generation_blobs(
    client: &R2Client,
    blobs_bucket: &str,
    site_dir: &Path,
    generation: &str,
) -> Result<(), PublishError> {
    let blobs_path = site_dir
        .join("generations")
        .join(generation)
        .join("blobs.json");
    let text = std::fs::read_to_string(&blobs_path).map_err(|error| {
        PublishError::Integrity(format!(
            "could not read {}: {error}",
            blobs_path.display()
        ))
    })?;
    let digests = parse_blobs_json(&text).map_err(|error| PublishError::Integrity(error.to_string()))?;
    let directory = site_dir.join("blobs/sha256");
    std::fs::create_dir_all(&directory).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            directory.display()
        ))
    })?;
    for digest in digests {
        let bytes = client.get_object(blobs_bucket, &format!("sha256/{}", digest.hex()))?;
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

fn import_source(
    state: &myoci::state::State,
    source: &OciSource,
) -> Result<(), PublishError> {
    match source {
        OciSource::Layout {
            dir,
            repository,
            tag,
        } => {
            let repository = repository_name(repository)?;
            let tag = tag
                .as_deref()
                .map(Tag::parse)
                .transpose()
                .map_err(|error| PublishError::Usage(format!("--tag: {error}")))?;
            myoci::import::import_layout(state, Path::new(dir), &repository, tag.as_ref())
                .map_err(map_myoci_error)?;
        }
        OciSource::Images(images) => {
            for image in images {
                let repository = repository_name(&image.repository)?;
                myoci::import::import_from(state, &image.reference, &repository, None)
                    .map_err(map_myoci_error)?;
            }
        }
    }
    Ok(())
}

fn repository_name(text: &str) -> Result<RepositoryName, PublishError> {
    RepositoryName::parse(text)
        .map_err(|error| PublishError::Usage(format!("--repository: {error}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_scoped_to_the_oci_registry() {
        let paths = OciPaths::from_environment(&Environment::from_pairs([
            ("STUFF_STATE_HOME", "/state"),
            ("STUFF_CACHE_HOME", "/cache"),
        ]));
        assert_eq!(paths.state_dir, PathBuf::from("/state/oci/state"));
        assert_eq!(paths.site_dir, PathBuf::from("/state/oci/site"));
        assert_eq!(
            paths.transaction_state_dir,
            PathBuf::from("/state/oci/transaction")
        );
        assert_eq!(paths.cache_dir, PathBuf::from("/cache/oci"));
    }

    #[test]
    fn myoci_errors_map_to_the_p8_exit_codes() {
        assert_eq!(
            map_myoci_error(myoci::Error::Usage("x".to_string())).exit_code(),
            2
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Input("x".to_string())).exit_code(),
            2
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Conflict("x".to_string())).exit_code(),
            3
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Integrity("x".to_string())).exit_code(),
            4
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Limit("x".to_string())).exit_code(),
            2
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Network("x".to_string())).exit_code(),
            5
        );
        assert_eq!(
            map_myoci_error(myoci::Error::Operation("x".to_string())).exit_code(),
            4
        );
    }
}
