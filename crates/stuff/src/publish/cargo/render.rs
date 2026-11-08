use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use stuff_catalog::{
    CargoInput, CargoSource, Catalog, Dependencies, cargo_catalog, catalog_fingerprint,
    render_catalog_json, render_html, render_llms_txt,
};

use super::adopt::{NativeRecord, UpstreamRecord};
use super::model::{DependencyKind, IndexEntry};
use super::semver::compare_text;
use super::wirejson::json_string;
use super::{Rejection, RejectionKind, integrity, names};

pub const RENDERER_VERSION: u32 = 4;
pub const GENERATION_ID_LENGTH: usize = 24;
pub const FINGERPRINT_GENERATION: &str = "fingerprint";

pub type FileKey = (String, String);
pub type Files = BTreeMap<FileKey, Vec<u8>>;
pub type Manifest = BTreeMap<FileKey, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendered {
    pub generation_id: String,
    pub files: Files,
    pub manifest: Manifest,
}

pub fn native_config_json(public_base_url: &str, auth_required: bool) -> String {
    let dl = format!(
        "{}/blobs/{{sha256-checksum}}.crate",
        public_base_url.trim_end_matches('/')
    );
    format!(
        "{{\"dl\":{},\"api\":{},\"auth-required\":{}}}",
        json_string(&dl),
        json_string(public_base_url),
        if auth_required { "true" } else { "false" }
    )
}

pub fn mirror_config_json(public_base_url: &str) -> String {
    let dl = format!(
        "{}/blobs/{{sha256-checksum}}.crate",
        public_base_url.trim_end_matches('/')
    );
    format!("{{\"dl\":{}}}", json_string(&dl))
}

pub fn render(
    native: &[NativeRecord],
    upstream: &[UpstreamRecord],
    public_base_url: &str,
    auth_required: bool,
) -> Result<Rendered, Rejection> {
    let mut files = render_index(native, upstream, public_base_url, auth_required)?;
    let mut catalog = build_catalog(native, upstream)?;
    let fingerprint = catalog_fingerprint(&catalog, public_base_url);

    let mut manifest: Manifest = files
        .iter()
        .map(|(key, data)| (key.clone(), sha256_hex(data)))
        .collect();
    let mut canonical = format!("v:{RENDERER_VERSION}\ncatalog:{fingerprint}\n");
    for ((view, relative), digest) in &manifest {
        canonical.push_str(&format!("{view}|{relative}|{digest}\n"));
    }
    let generation_id = sha256_hex(canonical.as_bytes())[..GENERATION_ID_LENGTH].to_string();

    catalog.generation = generation_id.clone();
    let rendered_catalog = [
        ("catalog.json", render_catalog_json(&catalog)),
        ("index.html", render_html(&catalog)),
        ("llms.txt", render_llms_txt(&catalog, public_base_url)),
    ];
    for (name, text) in rendered_catalog {
        let key = (String::new(), name.to_string());
        let data = text.into_bytes();
        manifest.insert(key.clone(), sha256_hex(&data));
        files.insert(key, data);
    }

    Ok(Rendered {
        generation_id,
        files,
        manifest,
    })
}

fn render_index(
    native: &[NativeRecord],
    upstream: &[UpstreamRecord],
    public_base_url: &str,
    auth_required: bool,
) -> Result<Files, Rejection> {
    let mut files = Files::new();
    files.insert(
        key("registry", "config.json"),
        native_config_json(public_base_url, auth_required).into_bytes(),
    );
    files.insert(
        key("crates-io", "config.json"),
        mirror_config_json(public_base_url).into_bytes(),
    );

    let mut native_by_crate: BTreeMap<String, Vec<&NativeRecord>> = BTreeMap::new();
    for record in native {
        names::validate_crate_name(&record.entry.name)?;
        native_by_crate
            .entry(names::lower_name(&record.entry.name))
            .or_default()
            .push(record);
    }
    for (crate_name, records) in &native_by_crate {
        let mut records = records.clone();
        records.sort_by(|left, right| compare_text(&left.entry.vers, &right.entry.vers));
        let lines: Vec<String> = records
            .iter()
            .map(|record| record.entry.index_line())
            .collect();
        let shard = shard(crate_name)?;
        let text = format!("{}\n", lines.join("\n"));
        files.insert(key("registry", &shard), text.into_bytes());
    }

    let mut upstream_by_crate: BTreeMap<String, Vec<&UpstreamRecord>> = BTreeMap::new();
    for record in upstream {
        names::validate_crate_name(&record.name)?;
        upstream_by_crate
            .entry(names::lower_name(&record.name))
            .or_default()
            .push(record);
    }
    for (crate_name, records) in &upstream_by_crate {
        let mut records = records.clone();
        records.sort_by(|left, right| compare_text(&left.vers, &right.vers));
        let lines: Vec<&str> = records
            .iter()
            .map(|record| record.raw_index_line.trim())
            .collect();
        let shard = shard(crate_name)?;
        let text = format!("{}\n", lines.join("\n"));
        files.insert(key("crates-io", &shard), text.into_bytes());
    }

    Ok(files)
}

fn build_catalog(
    native: &[NativeRecord],
    upstream: &[UpstreamRecord],
) -> Result<Catalog, Rejection> {
    let mut inputs = Vec::with_capacity(native.len() + upstream.len());
    for record in native {
        let entry = &record.entry;
        inputs.push(CargoInput {
            name: entry.name.clone(),
            version: entry.vers.clone(),
            yanked: entry.yanked,
            source: CargoSource::Native,
            dependencies: catalog_dependencies(entry),
        });
    }
    for record in upstream {
        let entry = IndexEntry::from_json_line(&record.raw_index_line).ok();
        inputs.push(CargoInput {
            name: record.name.clone(),
            version: record.vers.clone(),
            yanked: entry.as_ref().is_some_and(|entry| entry.yanked),
            source: CargoSource::Upstream,
            dependencies: entry
                .as_ref()
                .map(catalog_dependencies)
                .unwrap_or_default(),
        });
    }
    cargo_catalog(FINGERPRINT_GENERATION, inputs).map_err(|message| {
        Rejection::new(
            RejectionKind::Operation,
            format!("could not build package catalog: {message}"),
        )
    })
}

fn catalog_dependencies(entry: &IndexEntry) -> Dependencies {
    let mut dependencies = Dependencies::default();
    for dep in &entry.deps {
        let name = dep
            .package
            .clone()
            .filter(|package| !package.is_empty())
            .unwrap_or_else(|| dep.name.clone());
        match dep.kind.as_ref() {
            None | Some(DependencyKind::Normal) => dependencies.runtime.push(name),
            Some(DependencyKind::Build) => dependencies.build.push(name),
            Some(DependencyKind::Dev) => dependencies.dev.push(name),
            Some(_) => {}
        }
    }
    dependencies
}

fn shard(crate_lower: &str) -> Result<String, Rejection> {
    names::shard_for_name(crate_lower)
        .ok_or_else(|| integrity(format!("crate name is not ASCII: {crate_lower}")))
}

fn key(view: &str, relative: &str) -> FileKey {
    (view.to_string(), relative.to_string())
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    crate::publish::lower_hex(hasher.finalize())
}
