use crate::error::Result;
use crate::index::{mirror_config_json, native_config_json, serialize_index_line, shard_for_name};
use crate::model::CrateName;
use crate::store::Store;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use stuff_store::{sync_immutable_file, HashAlg, Site, sync_tree, write_file_durable};

const RENDERER_VERSION: u32 = 2;

#[derive(Debug, Clone)]
pub struct PublishResult {
    pub generation_id: String,
    pub is_noop: bool,
}

pub struct Publisher<'a> {
    pub store: &'a Store,
    pub site_dir: PathBuf,
    pub public_base_url: String,
}

impl<'a> Publisher<'a> {
    pub fn new(store: &'a Store, site_dir: PathBuf, public_base_url: String) -> Self {
        Self {
            store,
            site_dir,
            public_base_url,
        }
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.site_dir.join("blobs")
    }

    pub fn generations_dir(&self) -> PathBuf {
        self.site_dir.join("generations")
    }

    pub fn current_link(&self) -> PathBuf {
        self.site_dir.join("current")
    }

    fn sync_blob(&self, sha256: &str) -> Result<()> {
        let src = self.store.blobs_dir().join(format!("{sha256}.crate"));
        let dst = self.blobs_dir().join(format!("{sha256}.crate"));
        sync_immutable_file(&src, &dst, sha256, HashAlg::Sha256)?;
        Ok(())
    }

    fn generation_matches(&self, gen_dir: &Path, expected_manifest: &BTreeMap<(String, String), String>) -> Result<bool> {
        // Blob existence needs no check here: publish syncs every blob from
        // the store before staging, so a missing blob errors out first.
        for ((view, rel), sha) in expected_manifest {
            let p = if view.is_empty() {
                gen_dir.join(rel)
            } else {
                gen_dir.join(view).join(rel)
            };
            let Ok(bytes) = fs::read(&p) else {
                return Ok(false);
            };
            let mut h = Sha256::new();
            h.update(&bytes);
            if format!("{:x}", h.finalize_reset()) != *sha {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn publish(&self) -> Result<PublishResult> {
        self.store.recover_staging()?;
        let site = Site::new(self.site_dir.clone());
        site.recover_staging()?;
        site.check_no_symlinks_in_path()?;
        site.ensure_layout()?;

        fs::create_dir_all(self.blobs_dir())?;
        stuff_store::sync_dir(&self.blobs_dir())?;

        let native_records = self.store.load_native_records()?;
        let upstream_records = self.store.load_upstream_records()?;

        for rec in &native_records {
            self.sync_blob(&rec.checksum)?;
        }
        for rec in &upstream_records {
            self.sync_blob(&rec.checksum)?;
        }

        let staged = site.stage()?;
        let staging_dir = staged.path().to_path_buf();
        let _guard = staged;

        let staging_registry = staging_dir.join("registry");
        let staging_mirror = staging_dir.join("crates-io");
        fs::create_dir_all(&staging_registry)?;
        fs::create_dir_all(&staging_mirror)?;

        let mut manifest: BTreeMap<(String, String), String> = BTreeMap::new();

        let native_cfg = native_config_json(&self.public_base_url);
        let native_cfg_path = staging_registry.join("config.json");
        write_file_durable(&native_cfg_path, native_cfg.as_bytes())?;
        let mut h = Sha256::new();
        h.update(native_cfg.as_bytes());
        manifest.insert(("registry".to_string(), "config.json".to_string()), format!("{:x}", h.finalize_reset()));

        let mirror_cfg = mirror_config_json(&self.public_base_url);
        let mirror_cfg_path = staging_mirror.join("config.json");
        write_file_durable(&mirror_cfg_path, mirror_cfg.as_bytes())?;
        let mut h = Sha256::new();
        h.update(mirror_cfg.as_bytes());
        manifest.insert(("crates-io".to_string(), "config.json".to_string()), format!("{:x}", h.finalize_reset()));

        let mut native_by_crate: BTreeMap<String, Vec<&crate::store::NativeRecord>> = BTreeMap::new();
        for rec in &native_records {
            let lower = CrateName::new(&rec.name)?.lower();
            native_by_crate.entry(lower).or_default().push(rec);
        }

        for (crate_lower, mut recs) in native_by_crate {
            recs.sort_by(|a, b| {
                let va = crate::version::Version::parse(&a.vers);
                let vb = crate::version::Version::parse(&b.vers);
                match (va, vb) {
                    (Ok(a_ver), Ok(b_ver)) => a_ver.cmp(&b_ver),
                    _ => a.vers.cmp(&b.vers),
                }
            });

            let shard = shard_for_name(&crate_lower);
            let out_path = staging_registry.join(&shard);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut lines = Vec::new();
            for r in recs {
                let line = serialize_index_line(&r.entry)?;
                lines.push(line);
            }
            let content = format!("{}\n", lines.join("\n"));
            write_file_durable(&out_path, content.as_bytes())?;

            let mut h = Sha256::new();
            h.update(content.as_bytes());
            manifest.insert(("registry".to_string(), shard), format!("{:x}", h.finalize_reset()));
        }

        let mut upstream_by_crate: BTreeMap<String, Vec<&crate::store::UpstreamRecord>> = BTreeMap::new();
        for rec in &upstream_records {
            let lower = CrateName::new(&rec.name)?.lower();
            upstream_by_crate.entry(lower).or_default().push(rec);
        }

        for (crate_lower, mut recs) in upstream_by_crate {
            recs.sort_by(|a, b| {
                let va = crate::version::Version::parse(&a.vers);
                let vb = crate::version::Version::parse(&b.vers);
                match (va, vb) {
                    (Ok(a_ver), Ok(b_ver)) => a_ver.cmp(&b_ver),
                    _ => a.vers.cmp(&b.vers),
                }
            });

            let shard = shard_for_name(&crate_lower);
            let out_path = staging_mirror.join(&shard);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }

            let mut lines = Vec::new();
            for r in recs {
                lines.push(r.raw_index_line.trim().to_string());
            }
            let content = format!("{}\n", lines.join("\n"));
            write_file_durable(&out_path, content.as_bytes())?;

            let mut h = Sha256::new();
            h.update(content.as_bytes());
            manifest.insert(("crates-io".to_string(), shard), format!("{:x}", h.finalize_reset()));
        }

        let mut canonical_text = format!("v:{RENDERER_VERSION}\n");
        for ((view, path), sha) in &manifest {
            canonical_text.push_str(&format!("{view}|{path}|{sha}\n"));
        }
        let mut h = Sha256::new();
        h.update(canonical_text.as_bytes());
        let full_sha = format!("{:x}", h.finalize_reset());
        let gen_id = full_sha[..24].to_string();

        let catalog_inputs = catalog_inputs(&native_records, &upstream_records);
        let catalog = stuff_catalog::cargo_catalog(&gen_id, catalog_inputs)
            .map_err(|e| crate::error::Error::Operation(format!("could not build package catalog: {e}")))?;
        let catalog_json = stuff_catalog::render_catalog_json(&catalog);
        let catalog_html = stuff_catalog::render_html(&catalog);
        write_file_durable(&staging_dir.join("catalog.json"), catalog_json.as_bytes())?;
        write_file_durable(&staging_dir.join("index.html"), catalog_html.as_bytes())?;
        let mut h = Sha256::new();
        h.update(catalog_json.as_bytes());
        manifest.insert((String::new(), "catalog.json".to_string()), format!("{:x}", h.finalize_reset()));
        let mut h = Sha256::new();
        h.update(catalog_html.as_bytes());
        manifest.insert((String::new(), "index.html".to_string()), format!("{:x}", h.finalize_reset()));

        sync_tree(&staging_dir)?;
        let activation = site.activate(staging_dir, &gen_id, &|gen_dir| {
            self.generation_matches(gen_dir, &manifest)
                .map_err(stuff_store::Error::from)
        })?;

        Ok(PublishResult {
            generation_id: activation.generation_id,
            is_noop: activation.is_noop,
        })
    }
}

fn catalog_inputs(
    native: &[crate::store::NativeRecord],
    upstream: &[crate::store::UpstreamRecord],
) -> Vec<stuff_catalog::CargoInput> {
    use std::collections::BTreeSet;
    let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
    let mut out = Vec::with_capacity(native.len() + upstream.len());
    for rec in native {
        seen.insert((rec.name.as_str(), rec.vers.as_str()));
        out.push(stuff_catalog::CargoInput {
            name: rec.name.clone(),
            version: rec.vers.clone(),
            yanked: rec.yanked,
            source: stuff_catalog::CargoSource::Native,
        });
    }
    for rec in upstream {
        if !seen.insert((rec.name.as_str(), rec.vers.as_str())) {
            continue;
        }
        let yanked = crate::json::from_str::<crate::model::IndexEntry>(&rec.raw_index_line)
            .map(|e| e.yanked)
            .unwrap_or(false);
        out.push(stuff_catalog::CargoInput {
            name: rec.name.clone(),
            version: rec.vers.clone(),
            yanked,
            source: stuff_catalog::CargoSource::Upstream,
        });
    }
    out
}
