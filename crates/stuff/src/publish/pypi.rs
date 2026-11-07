#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use deser_value::Value;

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::client::{Credentials, R2Client};
use crate::publish::r2::credentials::{self, Environment};
use crate::publish::r2::promote;
use crate::publish::r2::pull::{self, PART_SIZE};
use crate::publish::transaction::{Transaction, TransactionOptions};
use crate::publish::{Options, Outcome, PublishError, PypiSpec, Registry};

const PYTHON_SPECIFIER: &str = ">=3.15,<3.16";
const LINUX_REPAIR_POLICY: &str = "manylinux_2_28";

pub fn run(specs: &[PypiSpec], options: &Options) -> Result<Outcome, PublishError> {
    let environment = Environment::from_process();
    let paths = Paths::from_env(|name| environment.get(name).map(str::to_string));
    let config = Config::load(&paths.config_file())?;
    let settings = project_settings(&config.pypi.public_base_url)?;
    let catalog = package_catalog(specs)?;
    let credentials = credentials::resolve(&environment, &config.pypi.bucket)?;
    publish(
        credentials,
        &config,
        &settings,
        &catalog,
        &paths.state_dir("pypi"),
        &paths.cache_dir("pypi"),
        options,
    )
}

pub fn project_settings(
    public_base_url: &str,
) -> Result<mypypi::config::ProjectSettings, PublishError> {
    let platform = mypypi::validate::local_platform().ok_or_else(|| {
        PublishError::Config(
            "pypi publish requires a supported host platform for wheel validation".to_string(),
        )
    })?;
    let source = format!(
        "[project]\nrequires-python = \"{PYTHON_SPECIFIER}\"\n\n[tool.mypypi]\npublic_base_url = \"{}\"\n\n[tool.mypypi.targets]\nplatforms = [\"{platform}\"]\nlinux_repair_policy = \"{LINUX_REPAIR_POLICY}\"\n",
        toml_string(public_base_url)
    );
    mypypi::config::parse_project_settings(&source).map_err(map_mypypi)
}

pub fn package_catalog(
    specs: &[PypiSpec],
) -> Result<mypypi::catalog::Catalog, PublishError> {
    let mut source = String::new();
    for spec in specs {
        source.push_str("[[packages]]\n");
        source.push_str(&format!("name = \"{}\"\n", toml_string(&spec.name)));
        source.push_str(&format!("version = \"{}\"\n", toml_string(&spec.version)));
    }
    mypypi::catalog::parse_catalog(&source).map_err(map_mypypi)
}

fn publish(
    credentials: Credentials,
    config: &Config,
    settings: &mypypi::config::ProjectSettings,
    catalog: &mypypi::catalog::Catalog,
    state_dir: &Path,
    cache_dir: &Path,
    options: &Options,
) -> Result<Outcome, PublishError> {
    let client = R2Client::new(credentials.clone());
    let site = Staging::create(cache_dir)?;
    pull_live(&client, &config.pypi.bucket, site.path())?;
    mypypi::publish::publish(&mypypi::publish::PublishOptions {
        settings,
        output: site.path(),
        state_dir,
        catalog,
        platforms: &[],
    })
    .map_err(map_mypypi)?;
    let transaction = Transaction::open(
        credentials,
        Registry::Pypi,
        config,
        site.path().to_path_buf(),
        state_dir.to_path_buf(),
        cache_dir.to_path_buf(),
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    Ok(transaction.run()?.outcome())
}

fn pull_live(client: &R2Client, bucket: &str, site: &Path) -> Result<(), PublishError> {
    let Some(marker) = promote::read_marker(client, bucket)? else {
        return Ok(());
    };
    let generation = marker.generation;
    let objects = pull::list_all(client, bucket)?;
    let prefix = format!("generations/{generation}/");
    for object in objects
        .iter()
        .filter(|object| object.key.starts_with(&prefix))
    {
        pull::download(client, bucket, site, object, PART_SIZE)?;
    }
    let generation_dir = site.join("generations").join(&generation);
    if !generation_dir.is_dir() {
        return Err(PublishError::Integrity(format!(
            "live generation {generation} has no objects in {bucket}"
        )));
    }
    let current = site.join("current");
    std::os::unix::fs::symlink(format!("generations/{generation}"), &current).map_err(
        |error| {
            PublishError::Config(format!("could not link {}: {error}", current.display()))
        },
    )?;
    let wheels = site.join("wheels");
    std::fs::create_dir_all(&wheels).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", wheels.display()))
    })?;
    let alias = generation_dir.join("wheels");
    if alias.symlink_metadata().is_err() {
        std::os::unix::fs::symlink("../../wheels", &alias).map_err(|error| {
            PublishError::Config(format!("could not link {}: {error}", alias.display()))
        })?;
    }
    for filename in published_filenames(&generation_dir)? {
        for key in [
            format!("wheels/{filename}"),
            format!("wheels/{filename}.metadata"),
        ] {
            let object = objects.iter().find(|object| object.key == key).ok_or_else(|| {
                PublishError::Integrity(format!(
                    "live generation {generation} references missing {key}"
                ))
            })?;
            pull::download(client, bucket, site, object, PART_SIZE)?;
        }
    }
    Ok(())
}

fn published_filenames(generation: &Path) -> Result<Vec<String>, PublishError> {
    let path = generation.join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|error| {
        PublishError::Integrity(format!("could not read {}: {error}", path.display()))
    })?;
    let value: Value = deser_json::from_str(&text).map_err(|error| {
        PublishError::Integrity(format!("{} is not valid JSON: {error}", path.display()))
    })?;
    let artifacts = value
        .as_map()
        .and_then(|map| map.get("artifacts"))
        .and_then(|artifacts| artifacts.as_seq())
        .ok_or_else(|| {
            PublishError::Integrity(format!(
                "{} is not a mypypi publication manifest",
                path.display()
            ))
        })?;
    let mut filenames = Vec::with_capacity(artifacts.len());
    for artifact in artifacts.iter() {
        let filename = artifact
            .as_map()
            .and_then(|map| map.get("filename"))
            .and_then(|filename| filename.as_str())
            .ok_or_else(|| {
                PublishError::Integrity(format!(
                    "{} has an artifact without a filename",
                    path.display()
                ))
            })?;
        filenames.push(filename.to_string());
    }
    Ok(filenames)
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

fn toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

struct Staging {
    path: PathBuf,
}

impl Staging {
    fn create(cache: &Path) -> Result<Self, PublishError> {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        std::fs::create_dir_all(cache).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", cache.display()))
        })?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        for _ in 0..100 {
            let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
            let path = cache.join(format!(
                "pypi-site-{}-{nanos}-{counter}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Staging { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(PublishError::Config(format!(
                        "could not create {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(PublishError::Config(format!(
            "could not create a staging site under {}",
            cache.display()
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_select_the_local_platform() {
        let Some(platform) = mypypi::validate::local_platform() else {
            return;
        };
        let settings = project_settings("https://pypi.example.test").unwrap();
        assert_eq!(settings.platforms, vec![platform]);
        assert_eq!(settings.python_version.to_string(), "3.15");
        assert_eq!(settings.public_base_url, "https://pypi.example.test");
        assert_eq!(settings.linux_repair_policy, LINUX_REPAIR_POLICY);
    }

    #[test]
    fn package_catalog_keeps_one_entry_per_spec() {
        let catalog = package_catalog(&[
            PypiSpec {
                name: "alpha".to_string(),
                version: "1.0".to_string(),
            },
            PypiSpec {
                name: "beta".to_string(),
                version: "2.0".to_string(),
            },
        ])
        .unwrap();
        assert_eq!(catalog.entries.len(), 2);
        assert_eq!(catalog.entries[0].package.to_string(), "alpha==1.0");
        assert_eq!(catalog.entries[1].package.to_string(), "beta==2.0");
    }

    #[test]
    fn package_catalog_escapes_quotes_instead_of_splitting_entries() {
        let catalog = package_catalog(&[PypiSpec {
            name: "weird\"name".to_string(),
            version: "1.0".to_string(),
        }])
        .unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].package.name, "weird\"name");
    }

    #[test]
    fn non_canonical_specs_are_usage_errors() {
        let error = package_catalog(&[PypiSpec {
            name: "Not_Canonical".to_string(),
            version: "1.0".to_string(),
        }])
        .unwrap_err();
        assert_eq!(error.exit_code(), 2, "{error}");
    }

    #[test]
    fn mypypi_errors_map_to_the_documented_exit_codes() {
        let cases = [
            (mypypi::error::Kind::Conflict, 3),
            (mypypi::error::Kind::Integrity, 4),
            (mypypi::error::Kind::Network, 5),
            (mypypi::error::Kind::Configuration, 2),
            (mypypi::error::Kind::Io, 2),
            (mypypi::error::Kind::Validation, 2),
        ];
        for (kind, code) in cases {
            let error = map_mypypi(mypypi::error::Error::new(
                kind,
                "detail",
                mypypi::error::Stage::Publication,
            ));
            assert_eq!(error.exit_code(), code, "{kind:?}");
        }
    }

    #[test]
    fn a_staging_site_is_unique_and_removed() {
        let work = tempfile::tempdir().unwrap();
        let first = Staging::create(work.path()).unwrap();
        let second = Staging::create(work.path()).unwrap();
        assert_ne!(first.path(), second.path());
        let path = first.path().to_path_buf();
        drop(first);
        assert!(!path.exists());
    }
}
