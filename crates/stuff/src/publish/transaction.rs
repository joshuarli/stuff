#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use stuff_store::{FileLock, LockError};

use crate::publish::config::Config;
use crate::publish::r2::client::{Credentials, ObjectMeta, R2Client};
use crate::publish::r2::guard;
use crate::publish::r2::promote;
use crate::publish::r2::put::UploadOptions;
use crate::publish::r2::sync::{self, SitePlan, SyncReport};
use crate::publish::snapshot::{self, Snapshot};
use crate::publish::{Outcome, PublishError, Registry, Status};

pub const LOCK_FILE: &str = "publish.lock";
pub const RECORD_FILE: &str = "promoted-generation";

const DEFAULT_LIST_PAGE: usize = 1000;
const DEFAULT_ATTEMPTS: u32 = 6;
const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct TransactionOptions {
    pub dry_run: bool,
    pub allow_drops: bool,
    pub expected: Option<String>,
    pub upload: UploadOptions,
    pub list_page: usize,
    pub attempts: u32,
    pub lock_wait: Duration,
}

impl Default for TransactionOptions {
    fn default() -> Self {
        TransactionOptions {
            dry_run: false,
            allow_drops: false,
            expected: None,
            upload: UploadOptions::default(),
            list_page: DEFAULT_LIST_PAGE,
            attempts: DEFAULT_ATTEMPTS,
            lock_wait: DEFAULT_LOCK_WAIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionReport {
    pub registry: Registry,
    pub status: Status,
    pub generation: String,
    pub uploaded_objects: usize,
    pub uploaded_bytes: u64,
    pub skipped_objects: usize,
    pub multipart_objects: usize,
    pub dry_run: bool,
    pub aliases: Vec<String>,
}

impl TransactionReport {
    pub fn outcome(&self) -> Outcome {
        Outcome {
            registry: self.registry,
            status: self.status,
            generation: self.generation.clone(),
            dry_run: self.dry_run,
            uploaded_objects: self.uploaded_objects,
            uploaded_bytes: self.uploaded_bytes,
            skipped_objects: self.skipped_objects,
        }
    }
}

pub struct Pending {
    client: R2Client,
    snapshot: Snapshot,
    registry: Registry,
    metadata_bucket: String,
    blobs_bucket: Option<String>,
    state_dir: PathBuf,
    options: TransactionOptions,
    marker: Option<promote::Marker>,
    expected: Option<String>,
    _lock: FileLock,
}

pub struct Transaction {
    client: R2Client,
    registry: Registry,
    metadata_bucket: String,
    blobs: Option<(String, SitePlan)>,
    site: PathBuf,
    state_dir: PathBuf,
    options: TransactionOptions,
    generation: String,
    expected: Option<String>,
    marker: Option<promote::Marker>,
    plan: SitePlan,
    _lock: FileLock,
}

impl Transaction {
    pub fn begin(
        credentials: Credentials,
        registry: Registry,
        config: &Config,
        state_dir: PathBuf,
        cache_dir: PathBuf,
        options: TransactionOptions,
    ) -> Result<Pending, PublishError> {
        std::fs::create_dir_all(&state_dir).map_err(|error| {
            PublishError::Config(format!(
                "could not create {}: {error}",
                state_dir.display()
            ))
        })?;
        let lock = acquire_lock(&state_dir, options.lock_wait)?;
        let client = build_client(&credentials, &options);
        let snapshot_client = build_client(&credentials, &options);
        let snapshot = Snapshot::open(snapshot_client, registry, config, cache_dir)?;
        let metadata_bucket = snapshot::metadata_bucket(registry, config);
        let marker = promote::read_marker(&client, &metadata_bucket)?;
        let expected = options.expected.clone().or(read_record(&state_dir)?);
        let blobs_bucket = (registry == Registry::Oci).then(|| config.oci.blobs_bucket.clone());
        Ok(Pending {
            client,
            snapshot,
            registry,
            metadata_bucket,
            blobs_bucket,
            state_dir,
            options,
            marker,
            expected,
            _lock: lock,
        })
    }
}

impl Pending {
    pub fn client(&self) -> &R2Client {
        &self.client
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn stage_complete(self, site: impl Into<PathBuf>) -> Result<Transaction, PublishError> {
        let site = site.into();
        let generation = sync::active_generation(&site)?;
        if let Some(expected) = &self.expected {
            let current = self.marker.as_ref().map(|marker| marker.generation.as_str());
            if current != Some(expected.as_str()) && current != Some(generation.as_str()) {
                return Err(PublishError::Conflict(format!(
                    "remote current is '{}', expected '{expected}'; refusing to overwrite a competing publication",
                    current.unwrap_or("<missing>")
                )));
            }
        }
        guard::check(
            &self.client,
            &self.metadata_bucket,
            &site,
            &generation,
            self.options.allow_drops,
        )?;
        let remote = metadata_index(&self.snapshot);
        let excludes: &[&str] = if self.registry == Registry::Oci {
            &["blobs/"]
        } else {
            &[]
        };
        let local = sync::collect(&site, &self.options.upload, excludes)?;
        let plan = sync::plan(&local, &remote, &self.options.upload)?;
        let blobs = match &self.blobs_bucket {
            Some(bucket) => {
                let remote = self
                    .snapshot
                    .listing(bucket)
                    .map(index_from_listing)
                    .unwrap_or_default();
                let plan = sync::plan_blobs(&site, &remote, &self.options.upload)?;
                Some((bucket.clone(), plan))
            }
            None => None,
        };
        Ok(Transaction {
            client: self.client,
            registry: self.registry,
            metadata_bucket: self.metadata_bucket,
            blobs,
            site,
            state_dir: self.state_dir,
            options: self.options,
            generation,
            expected: self.expected,
            marker: self.marker,
            plan,
            _lock: self._lock,
        })
    }
}

impl Transaction {
    pub fn run(self) -> Result<TransactionReport, PublishError> {
        let metadata = self.plan.report();
        let blobs = self.blobs.as_ref().map(|(_, plan)| plan.report());
        let mut report = report_for(
            self.registry,
            &self.generation,
            self.options.dry_run,
            &metadata,
            blobs.as_ref(),
        );
        report.aliases = metadata.aliases.clone();
        if self.options.dry_run {
            report.status = if report.uploaded_objects == 0 && self.current_is_generation() {
                Status::Unchanged
            } else {
                Status::Published
            };
            return Ok(report);
        }
        let mut uploaded = metadata.uploaded.len();
        if let Some((bucket, plan)) = &self.blobs {
            let executed = sync::execute(
                &self.client,
                &self.site,
                bucket,
                plan,
                &self.options.upload,
            )?;
            if !executed.uploaded.is_empty() {
                uploaded += executed.uploaded.len();
            }
        }
        let executed = sync::execute(
            &self.client,
            &self.site,
            &self.metadata_bucket,
            &self.plan,
            &self.options.upload,
        )?;
        if !executed.uploaded.is_empty() {
            uploaded += executed.uploaded.len();
        }
        let promotion = promote::promote(
            &self.client,
            &self.metadata_bucket,
            &self.generation,
            self.expected.as_deref(),
        )?;
        let changed = promotion.promoted || uploaded > 0;
        report.status = if changed {
            Status::Published
        } else {
            Status::Unchanged
        };
        if changed {
            write_record(&self.state_dir, &self.generation)?;
        }
        Ok(report)
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn expected(&self) -> Option<&str> {
        self.expected.as_deref()
    }

    fn current_is_generation(&self) -> bool {
        self.marker
            .as_ref()
            .is_some_and(|marker| marker.generation == self.generation)
    }
}

pub fn rollback(
    credentials: Credentials,
    registry: Registry,
    bucket: &str,
    state_dir: PathBuf,
    generation: &str,
    dry_run: bool,
    lock_wait: Duration,
) -> Result<TransactionReport, PublishError> {
    promote::validate_generation(generation)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        PublishError::Config(format!(
            "could not create {}: {error}",
            state_dir.display()
        ))
    })?;
    let _lock = acquire_lock(&state_dir, lock_wait)?;
    let client = R2Client::new(credentials);
    let marker = promote::read_marker(&client, bucket)?;
    let would_change = marker
        .as_ref()
        .map(|marker| marker.generation.as_str())
        != Some(generation);
    let promoted = if dry_run {
        promote::verify_generation(&client, bucket, generation)?;
        false
    } else {
        let promotion = promote::promote(&client, bucket, generation, None)?;
        write_record(&state_dir, generation)?;
        promotion.promoted
    };
    let status = if dry_run {
        if would_change {
            Status::Published
        } else {
            Status::Unchanged
        }
    } else if promoted {
        Status::Published
    } else {
        Status::Unchanged
    };
    Ok(TransactionReport {
        registry,
        status,
        generation: generation.to_string(),
        uploaded_objects: 0,
        uploaded_bytes: 0,
        skipped_objects: 0,
        multipart_objects: 0,
        dry_run,
        aliases: Vec::new(),
    })
}

pub fn read_record(state_dir: &Path) -> Result<Option<String>, PublishError> {
    match std::fs::read_to_string(state_dir.join(RECORD_FILE)) {
        Ok(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PublishError::Config(format!(
            "could not read {}: {error}",
            state_dir.join(RECORD_FILE).display()
        ))),
    }
}

pub fn write_record(state_dir: &Path, generation: &str) -> Result<(), PublishError> {
    let path = state_dir.join(RECORD_FILE);
    let temporary = state_dir.join(format!("{RECORD_FILE}.tmp.{}", std::process::id()));
    std::fs::write(&temporary, format!("{generation}\n")).map_err(|error| {
        PublishError::Config(format!(
            "could not write {}: {error}",
            temporary.display()
        ))
    })?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        PublishError::Config(format!("could not rename {}: {error}", path.display()))
    })
}

fn acquire_lock(state_dir: &Path, wait: Duration) -> Result<FileLock, PublishError> {
    let lock_path = state_dir.join(LOCK_FILE);
    FileLock::acquire(&lock_path, wait).map_err(|error| match error {
        LockError::TimedOut => PublishError::Conflict(format!(
            "another publish holds {}",
            lock_path.display()
        )),
        LockError::Io(error) => PublishError::Config(format!(
            "could not lock {}: {error}",
            lock_path.display()
        )),
    })
}

fn build_client(credentials: &Credentials, options: &TransactionOptions) -> R2Client {
    R2Client::new(credentials.clone())
        .with_list_page(options.list_page)
        .with_attempts(options.attempts)
}

fn metadata_index(snapshot: &Snapshot) -> std::collections::BTreeMap<String, ObjectMeta> {
    index_from_listing(snapshot.metadata())
}

fn index_from_listing(
    objects: &std::collections::BTreeMap<String, crate::publish::r2::ListedObject>,
) -> std::collections::BTreeMap<String, ObjectMeta> {
    objects
        .iter()
        .map(|(key, object)| {
            (
                key.clone(),
                ObjectMeta {
                    size: object.size,
                    etag: object.etag.clone(),
                    content_type: None,
                },
            )
        })
        .collect()
}

fn report_for(
    registry: Registry,
    generation: &str,
    dry_run: bool,
    metadata: &SyncReport,
    blobs: Option<&SyncReport>,
) -> TransactionReport {
    let uploaded_objects =
        metadata.uploaded_objects() + blobs.map(SyncReport::uploaded_objects).unwrap_or(0);
    let uploaded_bytes =
        metadata.uploaded_bytes + blobs.map(|report| report.uploaded_bytes).unwrap_or(0);
    let skipped_objects =
        metadata.skipped_objects() + blobs.map(SyncReport::skipped_objects).unwrap_or(0);
    let multipart_objects =
        metadata.multipart.len() + blobs.map(|report| report.multipart.len()).unwrap_or(0);
    TransactionReport {
        registry,
        status: Status::Unchanged,
        generation: generation.to_string(),
        uploaded_objects,
        uploaded_bytes,
        skipped_objects,
        multipart_objects,
        dry_run,
        aliases: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::r2::put::PutAction;

    #[test]
    fn the_record_round_trips_and_empty_files_are_absent() {
        let work = tempfile::tempdir().unwrap();
        assert_eq!(read_record(work.path()).unwrap(), None);
        write_record(work.path(), "g1").unwrap();
        assert_eq!(read_record(work.path()).unwrap(), Some("g1".to_string()));
        assert_eq!(
            std::fs::read_to_string(work.path().join(RECORD_FILE)).unwrap(),
            "g1\n"
        );
        std::fs::write(work.path().join(RECORD_FILE), "\n").unwrap();
        assert_eq!(read_record(work.path()).unwrap(), None);
    }

    #[test]
    fn reports_count_metadata_and_shared_blob_uploads_together() {
        let plan = SitePlan {
            generation: "g1".to_string(),
            entries: vec![
                crate::publish::r2::sync::PlanEntry {
                    key: "a".to_string(),
                    size: 3,
                    action: PutAction::Put,
                    payload: crate::publish::r2::sync::Payload::Memory(Vec::new()),
                },
                crate::publish::r2::sync::PlanEntry {
                    key: "b".to_string(),
                    size: 5,
                    action: PutAction::Multipart,
                    payload: crate::publish::r2::sync::Payload::Memory(Vec::new()),
                },
                crate::publish::r2::sync::PlanEntry {
                    key: "c".to_string(),
                    size: 7,
                    action: PutAction::Skip,
                    payload: crate::publish::r2::sync::Payload::Memory(Vec::new()),
                },
            ],
            aliases: vec!["alias".to_string()],
        };
        let report = report_for(
            Registry::Cargo,
            "g1",
            true,
            &plan.report(),
            Some(&plan.report()),
        );
        assert_eq!(report.uploaded_objects, 4);
        assert_eq!(report.uploaded_bytes, 16);
        assert_eq!(report.skipped_objects, 2);
        assert_eq!(report.multipart_objects, 2);
        assert!(report.dry_run);
        assert_eq!(
            report.outcome(),
            Outcome {
                registry: Registry::Cargo,
                status: Status::Unchanged,
                generation: "g1".to_string(),
                dry_run: true,
                uploaded_objects: 4,
                uploaded_bytes: 16,
                skipped_objects: 2,
            }
        );
    }

    #[test]
    fn options_default_to_the_documented_knobs() {
        let options = TransactionOptions::default();
        assert!(!options.dry_run);
        assert!(!options.allow_drops);
        assert!(options.expected.is_none());
        assert!(!options.upload.repair);
        assert_eq!(options.list_page, DEFAULT_LIST_PAGE);
        assert_eq!(options.attempts, DEFAULT_ATTEMPTS);
        assert_eq!(options.lock_wait, DEFAULT_LOCK_WAIT);
    }
}
