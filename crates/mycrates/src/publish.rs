use crate::error::Result;
use crate::index::{mirror_config_json, native_config_json, serialize_index_line, shard_for_name};
use crate::model::CrateName;
use crate::store::Store;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use stuff_store::{sync_immutable_file, HashAlg, Site, sync_tree, write_file_durable};

const RENDERER_VERSION: u32 = 4;

#[derive(Debug, Clone)]
pub struct PublishResult {
    pub generation_id: String,
    pub is_noop: bool,
}

pub struct Publisher<'a> {
    pub store: &'a Store,
    pub site_dir: PathBuf,
    pub public_base_url: String,
    pub auth_required: bool,
}

impl<'a> Publisher<'a> {
    pub fn new(store: &'a Store, site_dir: PathBuf, public_base_url: String) -> Self {
        Self {
            store,
            site_dir,
            public_base_url,
            auth_required: false,
        }
    }

    pub fn with_auth_required(mut self, auth_required: bool) -> Self {
        self.auth_required = auth_required;
        self
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

        let rendered = self.render_index(&native_records, &upstream_records)?;
        let mut manifest = rendered.manifest;
        let mut files = rendered.files;
        let mut catalog = rendered.catalog;
        let mut canonical_text = format!("v:{RENDERER_VERSION}\ncatalog:{}\n", rendered.catalog_fingerprint);
        for ((view, path), sha) in &manifest {
            canonical_text.push_str(&format!("{view}|{path}|{sha}\n"));
        }
        let mut h = Sha256::new();
        h.update(canonical_text.as_bytes());
        let full_sha = format!("{:x}", h.finalize_reset());
        let gen_id = full_sha[..24].to_string();

        catalog.generation = gen_id.clone();
        push_rendered_file(
            &mut manifest,
            &mut files,
            "",
            "catalog.json".to_string(),
            stuff_catalog::render_catalog_json(&catalog).into_bytes(),
        );
        push_rendered_file(
            &mut manifest,
            &mut files,
            "",
            "index.html".to_string(),
            stuff_catalog::render_html(&catalog).into_bytes(),
        );
        push_rendered_file(
            &mut manifest,
            &mut files,
            "",
            "llms.txt".to_string(),
            stuff_catalog::render_llms_txt(&catalog, &self.public_base_url).into_bytes(),
        );

        if site.current_generation()?.as_deref() == Some(gen_id.as_str())
            && self.generation_matches(&site.generations_dir().join(&gen_id), &manifest)?
        {
            return Ok(PublishResult {
                generation_id: gen_id,
                is_noop: true,
            });
        }

        let staged = site.stage()?;
        let staging_dir = staged.path().to_path_buf();
        let _guard = staged;
        for file in &files {
            let path = if file.view.is_empty() {
                staging_dir.join(&file.path)
            } else {
                staging_dir.join(file.view).join(&file.path)
            };
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            write_file_durable(&path, &file.bytes)?;
        }

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

    fn render_index(
        &self,
        native_records: &[crate::store::NativeRecord],
        upstream_records: &[crate::store::UpstreamRecord],
    ) -> Result<RenderedIndex> {
        let mut manifest = BTreeMap::new();
        let mut files = Vec::new();

        let native_cfg = native_config_json(&self.public_base_url, self.auth_required);
        push_rendered_file(
            &mut manifest,
            &mut files,
            "registry",
            "config.json".to_string(),
            native_cfg.into_bytes(),
        );
        let mirror_cfg = mirror_config_json(&self.public_base_url);
        push_rendered_file(
            &mut manifest,
            &mut files,
            "crates-io",
            "config.json".to_string(),
            mirror_cfg.into_bytes(),
        );

        let mut native_by_crate: BTreeMap<String, Vec<&crate::store::NativeRecord>> = BTreeMap::new();
        for rec in native_records {
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

            let mut lines = Vec::new();
            for r in recs {
                lines.push(serialize_index_line(&r.entry)?);
            }
            let content = format!("{}\n", lines.join("\n"));
            push_rendered_file(
                &mut manifest,
                &mut files,
                "registry",
                shard_for_name(&crate_lower),
                content.into_bytes(),
            );
        }

        let mut upstream_by_crate: BTreeMap<String, Vec<&crate::store::UpstreamRecord>> = BTreeMap::new();
        for rec in upstream_records {
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

            let mut lines = Vec::new();
            for r in recs {
                lines.push(r.raw_index_line.trim().to_string());
            }
            let content = format!("{}\n", lines.join("\n"));
            push_rendered_file(
                &mut manifest,
                &mut files,
                "crates-io",
                shard_for_name(&crate_lower),
                content.into_bytes(),
            );
        }

        let catalog = stuff_catalog::cargo_catalog(
            stuff_catalog::FINGERPRINT_GENERATION,
            catalog_inputs(native_records, upstream_records),
        )
        .map_err(|e| crate::error::Error::Operation(format!("could not build package catalog: {e}")))?;
        let catalog_fingerprint = stuff_catalog::catalog_fingerprint(&catalog, &self.public_base_url);
        Ok(RenderedIndex {
            manifest,
            files,
            catalog,
            catalog_fingerprint,
        })
    }
}

struct RenderedFile {
    view: &'static str,
    path: String,
    bytes: Vec<u8>,
}

struct RenderedIndex {
    manifest: BTreeMap<(String, String), String>,
    files: Vec<RenderedFile>,
    catalog: stuff_catalog::Catalog,
    catalog_fingerprint: String,
}

fn push_rendered_file(
    manifest: &mut BTreeMap<(String, String), String>,
    files: &mut Vec<RenderedFile>,
    view: &'static str,
    path: String,
    bytes: Vec<u8>,
) {
    let mut h = Sha256::new();
    h.update(&bytes);
    manifest.insert((view.to_string(), path.clone()), format!("{:x}", h.finalize()));
    files.push(RenderedFile { view, path, bytes });
}

fn catalog_dependencies(entry: &crate::model::IndexEntry) -> stuff_catalog::Dependencies {
    let mut dependencies = stuff_catalog::Dependencies::default();
    for dep in &entry.deps {
        let name = dep.package.clone().unwrap_or_else(|| dep.name.clone());
        match dep.kind.as_deref() {
            None | Some("normal") => dependencies.runtime.push(name),
            Some("build") => dependencies.build.push(name),
            Some("dev") => dependencies.dev.push(name),
            Some(_) => {}
        }
    }
    dependencies
}

fn catalog_inputs(
    native: &[crate::store::NativeRecord],
    upstream: &[crate::store::UpstreamRecord],
) -> Vec<stuff_catalog::CargoInput> {
    let mut out = Vec::with_capacity(native.len() + upstream.len());
    for rec in native {
        out.push(stuff_catalog::CargoInput {
            name: rec.name.clone(),
            version: rec.vers.clone(),
            yanked: rec.yanked,
            source: stuff_catalog::CargoSource::Native,
            dependencies: catalog_dependencies(&rec.entry),
        });
    }
    for rec in upstream {
        let entry = crate::json::from_str::<crate::model::IndexEntry>(&rec.raw_index_line).ok();
        out.push(stuff_catalog::CargoInput {
            name: rec.name.clone(),
            version: rec.vers.clone(),
            yanked: entry.as_ref().is_some_and(|e| e.yanked),
            source: stuff_catalog::CargoSource::Upstream,
            dependencies: entry.as_ref().map(catalog_dependencies).unwrap_or_default(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_dependencies_keep_each_kind_under_the_real_crate_name() {
        let line = r#"{"name":"app","vers":"1.0.0","deps":[
            {"name":"serde","req":"^1","features":[],"optional":false,"default_features":true,"kind":"normal"},
            {"name":"json","req":"^1","features":[],"optional":true,"default_features":true,"package":"serde_json"},
            {"name":"tempfile","req":"^3","features":[],"optional":false,"default_features":true,"kind":"dev"},
            {"name":"cc","req":"^1","features":[],"optional":false,"default_features":true,"kind":"build"}
        ],"cksum":"00","features":{},"yanked":false}"#;
        let entry = crate::json::from_str::<crate::model::IndexEntry>(line).unwrap();
        let dependencies = catalog_dependencies(&entry);
        assert_eq!(dependencies.runtime, ["serde", "serde_json"]);
        assert_eq!(dependencies.dev, ["tempfile"]);
        assert_eq!(dependencies.build, ["cc"]);
        assert!(dependencies.peer.is_empty());
    }
}
