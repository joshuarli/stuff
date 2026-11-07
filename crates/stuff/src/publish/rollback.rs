#![forbid(unsafe_code)]

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::credentials;
use crate::publish::snapshot;
use crate::publish::transaction::{self, TransactionOptions};
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
    let credentials = credentials::resolve(environment, &bucket)?;
    let report = transaction::rollback(
        credentials,
        registry,
        &bucket,
        paths.state_dir(registry.as_str()),
        generation,
        options.dry_run,
        TransactionOptions::default().lock_wait,
    )?;
    Ok(report.outcome())
}
