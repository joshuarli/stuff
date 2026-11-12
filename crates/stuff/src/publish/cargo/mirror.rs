#![forbid(unsafe_code)]

//! `stuff publish cargo --lockfile`: extend the crates.io mirror view from Cargo.lock files.
//!
//! This ports the Python oracle's `import-lock` and `fetch` (crates/mycrates/mycrates/lockfile.py,
//! acquire.py, upstream.py): every crates.io package a lock pins is taken from the upstream sparse
//! index with its exact index line, its archive is verified against both the lock and the index
//! checksum, and the result is published like any other cargo generation.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use deser_value::Value;
use mynpm::acquire::{Exchange, HttpsExchange};
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};

use super::adopt::{Record, UpstreamRecord, View, reconstruct_line};
use super::crate_file::{self, Limits};
use super::names;
use super::run::{Begun, Generation, begin, publish_generation, rejection_error};
use super::store::Store;
use crate::publish::inventory::library_error;
use crate::publish::{Environment, Options, Outcome, PublishError};

pub const UPSTREAM_INDEX: &str = "https://index.crates.io/";
const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const SUPPORTED_LOCK_VERSIONS: [u64; 2] = [3, 4];
const REDIRECTS: usize = 5;
const DL_PLACEHOLDERS: [&str; 5] = ["{crate}", "{version}", "{prefix}", "{lowerprefix}", "{sha256-checksum}"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedCrate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedLock {
    pub registry: Vec<LockedCrate>,
    pub skipped: Vec<String>,
}

fn is_crates_io_source(source: &str) -> bool {
    source.starts_with("registry+") && (source.contains("index.crates.io") || source == CRATES_IO_SOURCE)
}

pub fn parse_lockfile(text: &str) -> Result<ParsedLock, PublishError> {
    let value: Value =
        deser_toml::from_str(text).map_err(|error| PublishError::Usage(format!("Cargo.lock is not valid TOML: {error}")))?;
    let document = value
        .as_map()
        .ok_or_else(|| PublishError::Usage("Cargo.lock must be a table".to_string()))?;
    if let Some(version) = document.get("version") {
        let version = version
            .as_u64()
            .ok_or_else(|| PublishError::Usage("lock version must be an integer".to_string()))?;
        if !SUPPORTED_LOCK_VERSIONS.contains(&version) {
            return Err(PublishError::Usage(format!("unknown lock version {version}")));
        }
    }
    let mut parsed = ParsedLock::default();
    let packages = match document.get("package") {
        Some(packages) => packages
            .as_seq()
            .ok_or_else(|| PublishError::Usage("lock package must be an array of tables".to_string()))?,
        None => return Ok(parsed),
    };
    for package in packages {
        let package = package
            .as_map()
            .ok_or_else(|| PublishError::Usage("lock package must be a table".to_string()))?;
        let text = |key: &str| package.get(key).and_then(|value| value.as_str());
        let (Some(name), Some(version)) = (text("name"), text("version")) else {
            return Err(PublishError::Usage("lock package needs a string name and version".to_string()));
        };
        let label = format!("{name} {version}");
        match text("source") {
            None => {}
            Some(source) if is_crates_io_source(source) => {
                let checksum = text("checksum").ok_or_else(|| {
                    PublishError::Integrity(format!("package {label} missing checksum in Cargo.lock"))
                })?;
                parsed.registry.push(LockedCrate {
                    name: name.to_string(),
                    version: version.to_string(),
                    checksum: checksum.to_string(),
                });
            }
            Some(source) if source.starts_with("git+") => parsed.skipped.push(format!("git: {label}")),
            Some(source) if source.starts_with("registry+") => {
                parsed.skipped.push(format!("alternate-registry ({source}): {label}"));
            }
            Some(source) if source.starts_with("path+") || source.contains("(path+") => {
                parsed.skipped.push(format!("path: {label}"));
            }
            Some(_) => parsed.skipped.push(format!("unsupported: {label}")),
        }
    }
    Ok(parsed)
}

fn fetch(exchange: &dyn Exchange, raw: &str) -> Result<Vec<u8>, PublishError> {
    let mut current = stuff_url::Url::parse(raw)
        .map_err(|_| PublishError::Integrity(format!("invalid upstream URL {raw}")))?;
    for _ in 0..=REDIRECTS {
        if current.scheme() != "https" {
            return Err(PublishError::Integrity(format!("upstream URL is not HTTPS: {current}")));
        }
        let response = exchange.fetch(&current).map_err(library_error)?;
        if response.status == 200 {
            return Ok(response.body);
        }
        let location = response
            .location
            .filter(|location| !location.is_empty())
            .ok_or_else(|| PublishError::Transport(format!("upstream returned HTTP {} for {current}", response.status)))?;
        current = current
            .join(&location)
            .map_err(|_| PublishError::Integrity(format!("invalid upstream redirect {location}")))?;
    }
    Err(PublishError::Transport(format!("upstream exceeded {REDIRECTS} redirects for {raw}")))
}

fn dl_template(exchange: &dyn Exchange, index: &str) -> Result<String, PublishError> {
    let body = fetch(exchange, &format!("{}/config.json", index.trim_end_matches('/')))?;
    let text = String::from_utf8(body)
        .map_err(|_| PublishError::Integrity("upstream config.json is not UTF-8".to_string()))?;
    let value: Value = deser_json::from_str(&text)
        .map_err(|error| PublishError::Integrity(format!("invalid config.json from upstream: {error}")))?;
    value
        .as_map()
        .and_then(|map| map.get("dl"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| PublishError::Integrity("missing dl in upstream config.json".to_string()))
}

fn download_url(template: &str, locked: &LockedCrate) -> Result<String, PublishError> {
    let shard = names::shard_for_name(&names::lower_name(&locked.name))
        .ok_or_else(|| PublishError::Usage(format!("crate name is not ASCII: {}", locked.name)))?;
    let prefix = shard.rsplit_once('/').map_or("", |(prefix, _)| prefix);
    let template = if DL_PLACEHOLDERS.iter().any(|placeholder| template.contains(placeholder)) {
        template.to_string()
    } else {
        format!("{}/{{crate}}/{{version}}/download", template.trim_end_matches('/'))
    };
    let expanded = template
        .replace("{crate}", &locked.name)
        .replace("{version}", &locked.version)
        .replace("{prefix}", prefix)
        .replace("{lowerprefix}", &prefix.to_lowercase())
        .replace("{sha256-checksum}", &locked.checksum);
    if expanded.contains('{') && expanded.contains('}') {
        return Err(PublishError::Usage(format!("unsupported placeholder in dl template: {expanded}")));
    }
    Ok(expanded)
}

fn index_record(exchange: &dyn Exchange, index: &str, locked: &LockedCrate) -> Result<UpstreamRecord, PublishError> {
    let shard = names::shard_for_name(&names::lower_name(&locked.name))
        .ok_or_else(|| PublishError::Usage(format!("crate name is not ASCII: {}", locked.name)))?;
    let body = fetch(exchange, &format!("{}/{shard}", index.trim_end_matches('/')))?;
    let text = String::from_utf8(body)
        .map_err(|_| PublishError::Transport(format!("upstream index for {} is not UTF-8", locked.name)))?;
    for line in text.split('\n').map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(adopted) = reconstruct_line(View::Upstream, &shard, line) else {
            continue;
        };
        let Record::Upstream(record) = adopted.record else {
            continue;
        };
        if record.name == locked.name && record.vers == locked.version {
            return Ok(record);
        }
    }
    Err(PublishError::Usage(format!(
        "crate {} version {} not found in upstream index",
        locked.name, locked.version
    )))
}

fn acquire_one(
    exchange: &dyn Exchange,
    index: &str,
    template: &str,
    locked: &LockedCrate,
    store: &Store,
    now: u64,
) -> Result<(), PublishError> {
    let mut record = index_record(exchange, index, locked)?;
    if record.checksum != locked.checksum {
        return Err(PublishError::Integrity(format!(
            "trust chain violation for {} {}: lock checksum {} != index cksum {}",
            locked.name, locked.version, locked.checksum, record.checksum
        )));
    }
    let archive = fetch(exchange, &download_url(template, locked)?)?;
    let actual = crate::publish::lower_hex(Sha256::digest(&archive));
    if actual != locked.checksum {
        return Err(PublishError::Integrity(format!(
            "download checksum mismatch for {} {}: expected {}, got {actual}",
            locked.name, locked.version, locked.checksum
        )));
    }
    crate_file::scan(&archive, &locked.name, &locked.version, &Limits::default()).map_err(rejection_error)?;
    record.acquired_at = now;
    store
        .with_lock(|| {
            if store.ingest_blob(archive.as_slice())? != locked.checksum {
                return Err(super::Rejection::new(super::RejectionKind::Integrity, "ingested checksum mismatch"));
            }
            store.put_upstream_record(&record).map(drop)
        })
        .map_err(rejection_error)
}

/// Locked crates.io packages that neither the local state nor the live generation holds yet.
fn missing(
    locked: Vec<LockedCrate>,
    known: impl IntoIterator<Item = UpstreamRecord>,
) -> Result<Vec<LockedCrate>, PublishError> {
    let known: FxHashMap<(String, String), String> = known
        .into_iter()
        .map(|record| ((names::lower_name(&record.name), record.vers), record.checksum))
        .collect();
    let mut wanted: FxHashMap<(String, String), LockedCrate> = FxHashMap::default();
    for crate_ in locked {
        let key = (names::lower_name(&crate_.name), crate_.version.clone());
        if let Some(existing) = wanted.get(&key)
            && existing.checksum != crate_.checksum
        {
            return Err(PublishError::Integrity(format!(
                "lockfiles disagree on the checksum of {} {}",
                crate_.name, crate_.version
            )));
        }
        match known.get(&key) {
            Some(checksum) if *checksum == crate_.checksum => {}
            Some(checksum) => {
                return Err(PublishError::Integrity(format!(
                    "{} {} is mirrored with checksum {checksum}, but a lockfile pins {}",
                    crate_.name, crate_.version, crate_.checksum
                )));
            }
            None => {
                wanted.insert(key, crate_);
            }
        }
    }
    let mut wanted: Vec<LockedCrate> = wanted.into_values().collect();
    wanted.sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
    Ok(wanted)
}

fn read_lockfile(raw: &str) -> Result<ParsedLock, PublishError> {
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| PublishError::Config(format!("could not resolve the working directory: {error}")))?
            .join(path)
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|error| PublishError::Usage(format!("could not read {}: {error}", path.display())))?;
    parse_lockfile(&text).map_err(|error| match error {
        PublishError::Usage(detail) => PublishError::Usage(format!("{}: {detail}", path.display())),
        other => other,
    })
}

pub fn publish(lockfiles: &[String], options: Options, environment: &Environment) -> Result<Outcome, PublishError> {
    publish_with_exchange(lockfiles, options, environment, &HttpsExchange::new(), UPSTREAM_INDEX)
}

pub fn publish_with_exchange(
    lockfiles: &[String],
    options: Options,
    environment: &Environment,
    exchange: &dyn Exchange,
    index: &str,
) -> Result<Outcome, PublishError> {
    if lockfiles.is_empty() {
        return Err(PublishError::Usage("publish cargo --lockfile requires a PATH".to_string()));
    }
    let mut locked = Vec::new();
    for lockfile in lockfiles {
        let parsed = read_lockfile(lockfile)?;
        if options.verbose {
            for skipped in &parsed.skipped {
                eprintln!("skipping {lockfile}: {skipped}");
            }
        }
        locked.extend(parsed.registry);
    }
    let Begun { config, layout, staging: _staging, work_state, site_dir, pending, live, shared } =
        begin(options, environment)?;
    let store = Store::new(&layout.state_dir);
    let work_store = (options.dry_run && work_state != layout.state_dir).then(|| Store::new(&work_state));
    let known = store.with_lock(|| store.load_upstream_records()).map_err(rejection_error)?;
    let needed = missing(locked, known.into_iter().chain(live.upstream().iter().cloned()))?;
    if !needed.is_empty() {
        let target = work_store.as_ref().unwrap_or(&store);
        let template = dl_template(exchange, index)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
        for locked in &needed {
            acquire_one(exchange, index, &template, locked, target, now)?;
        }
    }
    let ids: Vec<String> = needed.iter().map(|locked| format!("{}@{}", locked.name, locked.version)).collect();
    publish_generation(
        pending,
        Generation {
            config: &config,
            shared: &shared,
            site_dir: &site_dir,
            store: &store,
            work_store: work_store.as_ref(),
        },
        |_, _| ids.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::{LockedCrate, ParsedLock, download_url, parse_lockfile};
    use crate::publish::ExitCode;

    #[test]
    fn only_crates_io_packages_are_mirrored() {
        let parsed = parse_lockfile(
            r#"version = 4

[[package]]
name = "demo"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"

[[package]]
name = "sparse"
version = "2.0.0"
source = "sparse+https://index.crates.io/"
checksum = "bb"

[[package]]
name = "native"
version = "0.3.0"
source = "sparse+https://crates.example/current/registry/"
checksum = "cc"

[[package]]
name = "forked"
version = "1.0.0"
source = "git+https://github.com/acme/forked?rev=abc#abc"

[[package]]
name = "local"
version = "1.0.0"
source = "path+file:///tmp/local"
"#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            ParsedLock {
                registry: vec![
                    LockedCrate { name: "serde".into(), version: "1.0.0".into(), checksum: "aa".into() },
                ],
                skipped: vec![
                    "unsupported: sparse 2.0.0".into(),
                    "unsupported: native 0.3.0".into(),
                    "git: forked 1.0.0".into(),
                    "path: local 1.0.0".into(),
                ],
            }
        );
    }

    #[test]
    fn a_crates_io_package_without_checksum_is_an_integrity_error() {
        let error = parse_lockfile(
            "[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        )
        .unwrap_err();
        assert_eq!(error.classification(), ExitCode::Integrity, "{error}");
    }

    #[test]
    fn unknown_lock_versions_are_rejected() {
        assert_eq!(parse_lockfile("version = 9\n").unwrap_err().classification(), ExitCode::Usage);
    }

    #[test]
    fn download_urls_follow_cargo_template_rules() {
        let locked = LockedCrate { name: "Serde".into(), version: "1.0.0".into(), checksum: "ab".into() };
        assert_eq!(
            download_url("https://static.crates.io/crates", &locked).unwrap(),
            "https://static.crates.io/crates/Serde/1.0.0/download"
        );
        assert_eq!(
            download_url("https://dl.example/{lowerprefix}/{crate}/{sha256-checksum}", &locked).unwrap(),
            "https://dl.example/se/rd/Serde/ab"
        );
    }
}
