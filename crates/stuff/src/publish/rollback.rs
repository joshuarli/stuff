#![forbid(unsafe_code)]

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::oci;
use crate::publish::r2::{credentials, R2Client};
use crate::publish::snapshot;
use crate::publish::transaction::{self, RollbackTarget, TransactionOptions};
use crate::publish::{Environment, Options, Outcome, PublishError, Registry};

pub fn rollback(
    registry: Registry,
    generation: &str,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let bucket = snapshot::metadata_bucket(registry, &config);
    let credentials = credentials::resolve(environment, &config.cf_profile, &bucket)?;
    let blobs = match registry {
        Registry::Oci => Some(R2Client::new(credentials::resolve(
            environment,
            &config.cf_profile,
            &config.oci.blobs_bucket,
        )?)),
        Registry::Cargo | Registry::Npm | Registry::Pypi => None,
    };
    let verify_target = |metadata: &R2Client| match &blobs {
        Some(blobs) => oci::verify_generation_blobs(
            metadata,
            &bucket,
            blobs,
            &config.oci.blobs_bucket,
            generation,
        ),
        None => Ok(()),
    };
    let report = transaction::rollback(
        credentials,
        RollbackTarget { registry, bucket: &bucket, generation, verify: &verify_target },
        paths.publish_state_dir(registry),
        options.dry_run,
        TransactionOptions::default().lock_wait,
    )?;
    Ok(report.outcome())
}
