#![forbid(unsafe_code)]

use std::path::Path;

use deser_value::Value;
use mypypi::build::{acquire_or_build, BuildOptions};
use mypypi::catalog::{parse_catalog, Catalog};
use mypypi::cli::create_plan;
use mypypi::models::Target;
use mypypi::planner::PlanOutcome;
use stuff_store::{HashAlg, stream_file_hash};

use crate::publish::config::Config;
use crate::publish::paths::Paths;
use crate::publish::r2::client::{Credentials, R2Client};
use crate::publish::r2::credentials;
use crate::publish::r2::promote;
use crate::publish::r2::pull::{self, PART_SIZE};
use crate::publish::staging::Staging;
use crate::publish::transaction::{Staged, Transaction, TransactionOptions};
use crate::publish::{Environment, Options, Outcome, PublishError, PypiPublish, PypiSpec, Registry};

const PYTHON_SPECIFIER: &str = ">=3.15,<3.16";
const LINUX_REPAIR_POLICY: &str = "manylinux_2_28";
const PYPI_SIMPLE_INDEX: &str = "https://pypi.org/simple";
const SUPPORTED_PLATFORMS: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-arm64",
];

pub fn run(
    publish: &PypiPublish,
    options: &Options,
    environment: &Environment,
) -> Result<Outcome, PublishError> {
    let paths = Paths::from_environment(environment);
    let config = Config::load(&paths.config_file())?;
    let settings = project_settings(&config.pypi.public_base_url)?;
    let credentials = credentials::resolve(environment, &config.cf_profile, &config.pypi.bucket)?;
    publish_internal(
        credentials,
        &config,
        &settings,
        publish,
        &paths.state_dir("pypi"),
        &paths.cache_dir("pypi"),
        options,
    )
}

pub fn project_settings(
    public_base_url: &str,
) -> Result<mypypi::config::ProjectSettings, PublishError> {
    let platform = local_platform()?;
    let platforms = SUPPORTED_PLATFORMS
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "[project]\nrequires-python = \"{PYTHON_SPECIFIER}\"\n\n[tool.mypypi]\npublic_base_url = \"{}\"\n\n[tool.mypypi.targets]\nplatforms = [{platforms}]\nlinux_repair_policy = \"{LINUX_REPAIR_POLICY}\"\n",
        toml_string(public_base_url)
    );
    let settings = mypypi::config::parse_project_settings(&source).map_err(map_mypypi)?;
    if !settings.platforms.iter().any(|name| name == &platform) {
        return Err(PublishError::Config(format!(
            "pypi publish requires a supported host platform for wheel validation: {platform}"
        )));
    }
    Ok(settings)
}

fn local_platform() -> Result<String, PublishError> {
    mypypi::validate::local_platform().ok_or_else(|| {
        PublishError::Config(
            "pypi publish requires a supported host platform for wheel validation".to_string(),
        )
    })
}

fn local_targets(
    settings: &mypypi::config::ProjectSettings,
) -> Result<Vec<Target>, PublishError> {
    let platform = local_platform()?;
    let targets: Vec<Target> = settings
        .targets()
        .into_iter()
        .filter(|target| target.platform == platform)
        .collect();
    if targets.is_empty() {
        return Err(PublishError::Config(format!(
            "pypi publish requires the host platform {platform} in the target configuration"
        )));
    }
    Ok(targets)
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

fn publish_internal(
    credentials: Credentials,
    config: &Config,
    settings: &mypypi::config::ProjectSettings,
    publish: &PypiPublish,
    state_dir: &Path,
    cache_dir: &Path,
    options: &Options,
) -> Result<Outcome, PublishError> {
    let catalog = resolved_catalog(&publish.specs, settings, cache_dir)?;
    let _platform = local_platform()?;
    let targets = settings.targets();
    let platforms: Vec<String> = targets.iter().map(|target| target.platform.clone()).collect();
    let staging = Staging::create(cache_dir, "pypi-site")?;
    let site = staging.path().join("site");
    let transaction_state = if options.dry_run {
        staging.join("transaction")
    } else {
        state_dir.to_path_buf()
    };
    let snapshot_cache = if options.dry_run {
        staging.join("cache")
    } else {
        cache_dir.to_path_buf()
    };
    let pending = Transaction::begin(
        credentials,
        Registry::Pypi,
        config,
        transaction_state,
        snapshot_cache,
        TransactionOptions {
            dry_run: options.dry_run,
            ..TransactionOptions::default()
        },
    )?;
    let mut acquired = false;
    let report = pending.publish(|pending| {
        remove_dir_if_present(&site)?;
        std::fs::create_dir_all(&site).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", site.display()))
        })?;
        pull_live(pending.client(), &config.pypi.bucket, &site, state_dir)?;
        if !acquired {
            acquire_wheels(&catalog, settings, &targets, &site, state_dir, publish.allow_sdist)?;
            acquired = true;
        }
        mypypi::publish::publish(&mypypi::publish::PublishOptions {
            settings,
            output: &site,
            state_dir,
            catalog: &catalog,
            platforms: &platforms,
        })
        .map_err(map_mypypi)?;
        Ok(Staged::new(site.clone()))
    })?;
    let mut outcome = report.outcome();
    outcome.packages = publish
        .specs
        .iter()
        .map(|spec| format!("{}=={}", spec.name, spec.version))
        .collect();
    Ok(outcome)
}

fn resolved_catalog(
    specs: &[PypiSpec],
    settings: &mypypi::config::ProjectSettings,
    cache_dir: &Path,
) -> Result<Catalog, PublishError> {
    let staging = Staging::create(cache_dir, "pypi-catalog")?;
    let path = staging.path().join("packages.toml");
    let mut source = String::new();
    let mut requirements = Vec::with_capacity(specs.len());
    for spec in specs {
        source.push_str("[[packages]]\n");
        source.push_str(&format!("name = \"{}\"\n", toml_string(&spec.name)));
        source.push_str(&format!("version = \"{}\"\n", toml_string(&spec.version)));
        requirements.push(format!("{}=={}", spec.name, spec.version));
    }
    std::fs::write(&path, source).map_err(|error| {
        PublishError::Config(format!("could not write {}: {error}", path.display()))
    })?;
    mypypi::add::add_packages(
        &path,
        &requirements,
        &settings.targets(),
        &mypypi::platforms::python_binary(),
    )
    .map_err(map_mypypi)?;
    let text = std::fs::read_to_string(&path).map_err(|error| {
        PublishError::Config(format!("could not read {}: {error}", path.display()))
    })?;
    parse_catalog(&text).map_err(map_mypypi)
}

fn acquire_wheels(
    catalog: &Catalog,
    settings: &mypypi::config::ProjectSettings,
    targets: &[Target],
    site: &Path,
    state_dir: &Path,
    allow_sdist: bool,
) -> Result<(), PublishError> {
    let wheels = state_dir.join("wheels");
    std::fs::create_dir_all(&wheels).map_err(|error| {
        PublishError::Config(format!("could not create {}: {error}", wheels.display()))
    })?;
    let data = create_plan(catalog, settings, targets, site, state_dir).map_err(map_mypypi)?;
    let entries = catalog.by_package();
    let prebuild_cwd = std::env::current_dir().map_err(|error| {
        PublishError::Config(format!("could not determine the working directory: {error}"))
    })?;
    for outcome in &data.outcomes {
        let (package, target) = match outcome {
            PlanOutcome::DownloadWheel { package, target, .. } => (package, target),
            PlanOutcome::BuildSdist { package, target, .. } => {
                if !allow_sdist {
                    return Err(PublishError::Usage(format!(
                        "{package} has no prebuilt wheel for {}; pass --allow-sdist to permit building from source",
                        target.platform
                    )));
                }
                (package, target)
            }
            _ => continue,
        };
        let entry = entries.get(package).ok_or_else(|| {
            PublishError::Integrity(format!("catalog has no entry for {package}"))
        })?;
        let release = data.releases.get(package).ok_or_else(|| {
            PublishError::Integrity(format!("plan has no upstream release for {package}"))
        })?;
        let ignore: Vec<String> = entry
            .build
            .likely_binary_ignore
            .iter()
            .map(|(package, _)| package.clone())
            .collect();
        let prebuild: Vec<(String, String)> = entry.build.custom_prebuild.clone();
        acquire_or_build(
            release,
            target,
            &wheels,
            &BuildOptions {
                index_url: PYPI_SIMPLE_INDEX,
                likely_binary_ignore: &ignore,
                custom_prebuild: &prebuild,
                linux_repair_policy: &settings.linux_repair_policy,
                build_constraints: None,
                require_build_hashes: false,
                env: None,
                prebuild_cwd: Some(&prebuild_cwd),
            },
            entry.yanked_reason.as_deref(),
        )
        .map_err(map_mypypi)?;
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

fn link_or_copy(source: &Path, target: &Path) -> Result<(), PublishError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            PublishError::Config(format!("could not create {}: {error}", parent.display()))
        })?;
    }
    match std::fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(_) => std::fs::copy(source, target).map(|_| ()).map_err(|error| {
            PublishError::Config(format!(
                "could not copy {} to {}: {error}",
                source.display(),
                target.display()
            ))
        }),
    }
}

fn pull_live(
    client: &R2Client,
    bucket: &str,
    site: &Path,
    state_dir: &Path,
) -> Result<(), PublishError> {
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
    for (filename, sha256) in published_filenames(&generation_dir)? {
        let local = state_dir.join("wheels").join(&filename);
        let matches = local.is_file()
            && stream_file_hash(&local, HashAlg::Sha256)
                .map(|actual| actual == sha256)
                .unwrap_or(false);
        if matches {
            link_or_copy(&local, &wheels.join(&filename))?;
        } else {
            let key = format!("wheels/{filename}");
            let object = objects.iter().find(|object| object.key == key).ok_or_else(|| {
                PublishError::Integrity(format!(
                    "live generation {generation} references missing {key}"
                ))
            })?;
            pull::download(client, bucket, site, object, PART_SIZE)?;
        }
        let metadata_key = format!("wheels/{filename}.metadata");
        let object = objects
            .iter()
            .find(|object| object.key == metadata_key)
            .ok_or_else(|| {
                PublishError::Integrity(format!(
                    "live generation {generation} references missing {metadata_key}"
                ))
            })?;
        pull::download(client, bucket, site, object, PART_SIZE)?;
    }
    Ok(())
}

fn published_filenames(generation: &Path) -> Result<Vec<(String, String)>, PublishError> {
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
        let map = artifact.as_map().ok_or_else(|| {
            PublishError::Integrity(format!(
                "{} has an artifact that is not an object",
                path.display()
            ))
        })?;
        let filename = map
            .get("filename")
            .and_then(|filename| filename.as_str())
            .ok_or_else(|| {
                PublishError::Integrity(format!(
                    "{} has an artifact without a filename",
                    path.display()
                ))
            })?;
        let sha256 = map
            .get("sha256")
            .and_then(|sha256| sha256.as_str())
            .ok_or_else(|| {
                PublishError::Integrity(format!(
                    "{} has an artifact without a sha256",
                    path.display()
                ))
            })?;
        filenames.push((filename.to_string(), sha256.to_string()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_configure_all_platforms_and_select_the_local_one() {
        let Some(platform) = mypypi::validate::local_platform() else {
            return;
        };
        let settings = project_settings("https://pypi.example.test").unwrap();
        assert_eq!(
            settings.platforms,
            vec![
                "linux-x86_64",
                "linux-aarch64",
                "macos-x86_64",
                "macos-arm64"
            ]
        );
        let targets = local_targets(&settings).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].platform, platform);
        assert_eq!(targets[0].python_version.to_string(), "3.15");
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

}
