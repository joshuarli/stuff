use crate::config::Config;
use crate::error::{Error, Result};
use crate::pins::Pins;
use crate::store::{Store, UpstreamRecord};
use crate::upstream::UpstreamClient;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

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
    let _guard = store.lock_exclusive()?;

    let items = plan(state_dir)?;
    if items.is_empty() {
        return Ok(FetchSummary::default());
    }

    let client = UpstreamClient::new(config.upstream.index_url.clone(), config.limits.clone())?;
    let dl_template = client.fetch_dl_template()?;

    let mut summary = FetchSummary::default();
    let mut bytes_budget = config.limits.fetch_bytes;

    for item in items {
        let (raw_line, index_entry) = client.fetch_index_entry(&item.name, &item.version)?;

        if index_entry.cksum != item.checksum {
            return Err(Error::Integrity(format!(
                "trust chain violation for {} {}: pin checksum {} != index cksum {}",
                item.name, item.version, item.checksum, index_entry.cksum
            )));
        }

        let dl_url = client.expand_dl_url(&dl_template, &item.name, &item.version, &item.checksum)?;

        let staged = client.download_crate(&dl_url, &item.checksum, &item.name, &item.version)?;
        let byte_len = staged.as_file().metadata().map(|m| m.len()).unwrap_or(0);

        if byte_len > bytes_budget {
            return Err(Error::Limit("fetch byte budget exceeded".to_string()));
        }
        bytes_budget -= byte_len;
        summary.total_bytes += byte_len;

        let staged_path = staged.path().to_path_buf();
        let ingested_sha = store.ingest_staged_file(&staged_path, &item.checksum)?;
        if ingested_sha != item.checksum {
            return Err(Error::Integrity("ingested checksum mismatch".to_string()));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = UpstreamRecord {
            name: item.name,
            vers: item.version,
            checksum: item.checksum,
            raw_index_line: raw_line,
            acquired_at: now,
        };
        store.put_upstream_record(&record)?;
        summary.fetched_count += 1;
    }

    Ok(summary)
}

pub fn verify(state_dir: &Path) -> Result<()> {
    let store = Store::new(state_dir.to_path_buf());
    store.verify()
}
