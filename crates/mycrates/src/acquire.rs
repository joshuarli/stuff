use crate::config::Config;
use crate::error::{Error, Result};
use crate::pins::Pins;
use crate::store::{Store, UpstreamRecord};
use crate::upstream::UpstreamClient;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

pub fn plan(state_dir: &Path) -> Result<Vec<PlanItem>> {
    let store = Store::new(state_dir.to_path_buf());
    let mut needed = Vec::new();

    let pins_dir = store.pins_dir();
    if !pins_dir.exists() {
        return Ok(needed);
    }

    for entry in fs::read_dir(&pins_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            let content = fs::read_to_string(&path)?;
            let pins: Pins = crate::toml::from_str(&content)
                .map_err(|e| Error::Input(format!("failed to parse {}: {e}", path.display())))?;
            for pin in pins.registry {
                let rec_path = store
                    .upstream_dir()
                    .join(pin.name.to_lowercase())
                    .join(format!("{}.json", pin.version));
                let blob_path = store
                    .blobs_dir()
                    .join(format!("{}.crate", pin.checksum));
                if !rec_path.exists() || !blob_path.exists() {
                    let item = PlanItem {
                        name: pin.name,
                        version: pin.version,
                        checksum: pin.checksum,
                    };
                    if !needed.contains(&item) {
                        needed.push(item);
                    }
                }
            }
        }
    }

    needed.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));
    Ok(needed)
}

#[derive(Debug, Default)]
pub struct FetchSummary {
    pub fetched_count: usize,
    pub total_bytes: u64,
}

pub fn fetch(state_dir: &Path, config: &Config) -> Result<FetchSummary> {
    let store = Store::new(state_dir.to_path_buf());

    let items = plan(state_dir)?;
    if items.is_empty() {
        return Ok(FetchSummary::default());
    }

    let client = UpstreamClient::new(config.upstream.index_url.clone(), config.limits.clone())?;
    let dl_template = client.fetch_dl_template()?;

    let deadline = Instant::now() + Duration::from_secs(config.limits.fetch_timeout_secs);
    let workers = config.limits.concurrent_requests.clamp(1, items.len());
    let next = AtomicUsize::new(0);
    let aborted = AtomicBool::new(false);
    let shared = Mutex::new(FetchState {
        summary: FetchSummary::default(),
        bytes_budget: config.limits.fetch_bytes,
        error: None,
    });

    thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while !aborted.load(Ordering::Relaxed) {
                    let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        return;
                    };
                    let outcome = if Instant::now() >= deadline {
                        Err(Error::Limit("fetch_timeout_secs exceeded".to_string()))
                    } else {
                        fetch_item(&store, &client, &dl_template, item, &shared)
                    };
                    if let Err(e) = outcome {
                        aborted.store(true, Ordering::Relaxed);
                        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
                        state.error.get_or_insert(e);
                        return;
                    }
                }
            });
        }
    });

    let state = shared.into_inner().unwrap_or_else(|p| p.into_inner());
    match state.error {
        Some(e) => Err(e),
        None => Ok(state.summary),
    }
}

struct FetchState {
    summary: FetchSummary,
    bytes_budget: u64,
    error: Option<Error>,
}

fn fetch_item(
    store: &Store,
    client: &UpstreamClient,
    dl_template: &str,
    item: &PlanItem,
    shared: &Mutex<FetchState>,
) -> Result<()> {
    let (raw_line, index_entry) = client.fetch_index_entry(&item.name, &item.version)?;

    if index_entry.cksum != item.checksum {
        return Err(Error::Integrity(format!(
            "trust chain violation for {} {}: pin checksum {} != index cksum {}",
            item.name, item.version, item.checksum, index_entry.cksum
        )));
    }

    let dl_url = client.expand_dl_url(dl_template, &item.name, &item.version, &item.checksum)?;

    let staged = client.download_crate(&dl_url, &item.checksum, &item.name, &item.version)?;
    let byte_len = staged.as_file().metadata().map(|m| m.len()).unwrap_or(0);

    {
        let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
        if byte_len > state.bytes_budget {
            return Err(Error::Limit("fetch byte budget exceeded".to_string()));
        }
        state.bytes_budget -= byte_len;
        state.summary.total_bytes += byte_len;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let record = UpstreamRecord {
        name: item.name.clone(),
        vers: item.version.clone(),
        checksum: item.checksum.clone(),
        raw_index_line: raw_line,
        acquired_at: now,
    };
    {
        let _guard = store.lock_exclusive()?;
        let ingested_sha = store.ingest_staged_file(staged.path(), &item.checksum)?;
        if ingested_sha != item.checksum {
            return Err(Error::Integrity("ingested checksum mismatch".to_string()));
        }
        store.put_upstream_record(&record)?;
    }
    let mut state = shared.lock().unwrap_or_else(|p| p.into_inner());
    state.summary.fetched_count += 1;
    Ok(())
}

pub fn verify(state_dir: &Path) -> Result<()> {
    let store = Store::new(state_dir.to_path_buf());
    store.verify()
}
