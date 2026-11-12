#![forbid(unsafe_code)]

use std::path::PathBuf;

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::pypi::{map_mypypi, project_settings, pull_live, remove_dir_if_present};
use crate::publish::r2::credentials;
use crate::publish::staging::Staging;
use crate::publish::transaction::{Staged, Transaction, TransactionOptions};
use crate::publish::{Environment, Options, Outcome, PublishError, Registry};

fn resolve(wheel: &str) -> Result<PathBuf, PublishError> {
    let path = PathBuf::from(wheel);
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| PublishError::Config(format!("could not resolve the working directory: {error}")))
}

pub fn publish(
    wheels: &[String],
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let wheels: Vec<PathBuf> = wheels.iter().map(|wheel| resolve(wheel)).collect::<Result<_, _>>()?;
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let settings = project_settings(&config.pypi.public_base_url)?;
    let artifacts = mypypi::publish::first_party_artifacts(&settings, &wheels).map_err(map_mypypi)?;
    let credentials = credentials::resolve(environment, &config.cf_profile, &config.pypi.bucket)?;
    let state_dir = paths.publish_state_dir(Registry::Pypi);
    let cache_dir = paths.cache_dir("pypi");
    let staging = Staging::create(&cache_dir, "pypi-first-party")?;
    let site = staging.path().join("site");
    let (transaction_state, snapshot_cache) = if options.dry_run {
        (staging.join("transaction"), staging.join("cache"))
    } else {
        (state_dir.clone(), cache_dir.clone())
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Pypi,
        &config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    let report = pending.publish(|pending| {
        remove_dir_if_present(&site)?;
        std::fs::create_dir_all(&site).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", site.display()))
        })?;
        pull_live(pending.client(), &config.pypi.bucket, &site, &state_dir)?;
        mypypi::publish::publish_first_party(&mypypi::publish::FirstPartyOptions {
            settings: &settings,
            output: &site,
            artifacts: &artifacts,
        })
        .map_err(map_mypypi)?;
        Ok(Staged::new(site.clone()))
    })?;
    let mut outcome = report.outcome();
    let mut packages: Vec<String> = artifacts.iter().map(|artifact| artifact.wheel.package.to_string()).collect();
    packages.sort();
    outcome.packages = packages;
    Ok(outcome)
}
