#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use crate::index::{IndexArtifact, prepare_index_artifacts, project_name, render_project_page, render_root_page};
use crate::models::{Target, ValidatedArtifact, WheelIdentity};
use sha2::{Digest, Sha256};
use stuff_store::{write_file_durable, FileLock, Site};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn pub_error(kind: Kind, detail: impl Into<String>, path: Option<&Path>, item: Option<&IndexArtifact>) -> Error {
    let mut detail = detail.into();
    if let Some(p) = path {
        detail = format!("{detail}: {}", p.display());
    }
    let mut e = Error::new(kind, detail, Stage::Publication);
    if let Some(it) = item {
        e = e.with_package(format!("{}=={}", it.package_name, it.package_version));
        if let Some(t) = it.targets.iter().min_by(|a, b| a.platform.cmp(&b.platform)) {
            e = e.with_target(&t.python_version.to_string(), &t.platform);
        }
    }
    e
}

fn ensure_output_root(output: &Path) -> Result<PathBuf> {
    let output = std::path::absolute(output)
        .map_err(|e| pub_error(Kind::Io, format!("could not inspect output root: {e}"), Some(output), None))?;
    match std::fs::symlink_metadata(&output) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(pub_error(Kind::InvalidInput, "output root must not be a symlink", Some(&output), None));
            }
            if !md.file_type().is_dir() {
                return Err(pub_error(Kind::InvalidInput, "output root must be a directory", Some(&output), None));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&output)
                .map_err(|e| pub_error(Kind::Io, format!("could not inspect output root: {e}"), Some(&output), None))?;
        }
        Err(e) => return Err(pub_error(Kind::Io, format!("could not inspect output root: {e}"), Some(&output), None)),
    }
    Ok(output)
}

fn lock_site(site: &Site) -> Result<FileLock> {
    let path = site.site_dir.join(".publish.lock");
    site.lock(Duration::from_secs(600)).map_err(|e| match e {
        stuff_store::Error::Conflict(_) => {
            pub_error(Kind::Conflict, "another publisher holds the output lock", Some(&path), None)
        }
        other => pub_error(Kind::Io, other.to_string(), Some(&path), None),
    })
}

fn ensure_directory(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|e| pub_error(Kind::Io, format!("could not create publication directory: {e}"), Some(path), None))?;
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| pub_error(Kind::Io, format!("could not create publication directory: {e}"), Some(path), None))?;
    if !md.file_type().is_dir() || md.file_type().is_symlink() {
        return Err(pub_error(Kind::InvalidInput, "publication path must be a real directory", Some(path), None));
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    stuff_store::stream_file_hash(path, stuff_store::HashAlg::Sha256)
        .map_err(|e| pub_error(Kind::Io, format!("could not read stored wheel: {e}"), Some(path), None))
}

fn existing_wheel_sha256(path: &Path, item: &IndexArtifact) -> Result<String> {
    let md = std::fs::symlink_metadata(path)
        .map_err(|e| pub_error(Kind::Io, format!("could not inspect stored wheel: {e}"), Some(path), Some(item)))?;
    if !md.file_type().is_file() {
        return Err(pub_error(Kind::Integrity, "stored wheel path is not a regular file", Some(path), Some(item)));
    }
    file_sha256(path).map_err(|_| pub_error(Kind::Io, "could not read stored wheel", Some(path), Some(item)))
}

pub fn store_wheel(output: &Path, item: &IndexArtifact, source: &Path) -> Result<()> {
    let wheels = output.join("wheels");
    ensure_directory(&wheels)?;
    let destination = wheels.join(&item.wheel_filename);
    match std::fs::symlink_metadata(&destination) {
        Ok(md) => {
            if !md.file_type().is_file() {
                return Err(pub_error(Kind::Integrity, "stored wheel path is not a regular file", Some(&destination), Some(item)));
            }
            if existing_wheel_sha256(&destination, item)? != item.sha256 {
                return Err(pub_error(Kind::Conflict, "wheel filename already exists with different bytes", Some(&destination), Some(item)));
            }
            store_metadata(&wheels, item)?;
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(pub_error(Kind::Io, format!("could not inspect stored wheel: {e}"), Some(&destination), Some(item))),
    }
    stuff_store::sync_immutable_file(source, &destination, &item.sha256, stuff_store::HashAlg::Sha256)
        .map_err(|e| match e {
            stuff_store::Error::Conflict(s) => pub_error(Kind::Conflict, s, Some(&destination), Some(item)),
            stuff_store::Error::Integrity(s) => pub_error(Kind::Integrity, s, Some(&destination), Some(item)),
            other => pub_error(Kind::Io, other.to_string(), Some(&destination), Some(item)),
        })?;
    store_metadata(&wheels, item)?;
    stuff_store::sync_dir(&wheels)
        .map_err(|e| pub_error(Kind::Io, e.to_string(), Some(&wheels), Some(item)))?;
    Ok(())
}

fn store_metadata(wheels: &Path, item: &IndexArtifact) -> Result<()> {
    let destination = wheels.join(format!("{}.metadata", item.wheel_filename));
    match std::fs::symlink_metadata(&destination) {
        Ok(md) => {
            if !md.file_type().is_file() {
                return Err(pub_error(Kind::Integrity, "stored metadata path is not a regular file", Some(&destination), Some(item)));
            }
            let existing = std::fs::read(&destination)
                .map_err(|e| pub_error(Kind::Io, format!("could not read stored metadata: {e}"), Some(&destination), Some(item)))?;
            if existing != item.metadata {
                return Err(pub_error(Kind::Conflict, "wheel metadata sidecar already exists with different bytes", Some(&destination), Some(item)));
            }
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(pub_error(Kind::Io, format!("could not read stored metadata: {e}"), Some(&destination), Some(item))),
    }
    let tmp = wheels.join(format!(".{}.{:x}.metadata.tmp", item.wheel_filename, rand_hex()));
    let write_result: Result<()> = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| pub_error(Kind::Io, format!("could not store wheel metadata: {e}"), Some(&destination), Some(item)))?;
        f.write_all(&item.metadata)
            .map_err(|e| pub_error(Kind::Io, format!("could not store wheel metadata: {e}"), Some(&destination), Some(item)))?;
        f.sync_all()
            .map_err(|e| pub_error(Kind::Io, format!("could not store wheel metadata: {e}"), Some(&destination), Some(item)))?;
        drop(f);
        match std::fs::hard_link(&tmp, &destination) {
            Ok(()) => {}
            Err(_) => {
                let md = std::fs::symlink_metadata(&destination)
                    .map_err(|e| pub_error(Kind::Io, format!("could not inspect stored metadata: {e}"), Some(&destination), Some(item)))?;
                if !md.file_type().is_file()
                    || std::fs::read(&destination).map(|b| b != item.metadata).unwrap_or(true)
                {
                    return Err(pub_error(Kind::Conflict, "wheel metadata sidecar already exists with different bytes", Some(&destination), Some(item)));
                }
            }
        }
        stuff_store::sync_dir(wheels)
            .map_err(|e| pub_error(Kind::Io, e.to_string(), Some(&destination), Some(item)))?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    write_result
}

fn rand_hex() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (nanos as u64) ^ ((std::process::id() as u64).wrapping_mul(0x9E3779B97F4A7C15))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let n = c as u32;
                if n > 0xFFFF {
                    let v = n - 0x10000;
                    out.push_str(&format!("\\u{:04x}\\u{:04x}", 0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)));
                } else {
                    out.push_str(&format!("\\u{n:04x}"));
                }
            }
        }
    }
    out.push('"');
    out
}

fn json_string_or_null(value: Option<&str>) -> String {
    match value {
        Some(s) => json_escape(s),
        None => "null".to_string(),
    }
}

pub fn manifest_bytes(items: &[IndexArtifact]) -> Vec<u8> {
    let mut out = String::from("{\"artifacts\":[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"filename\":");
        out.push_str(&json_escape(&item.wheel_filename));
        out.push_str(",\"metadata_sha256\":");
        out.push_str(&json_escape(&item.metadata_sha256));
        out.push_str(",\"name\":");
        out.push_str(&json_escape(&item.package_name));
        out.push_str(",\"requires_python\":");
        out.push_str(&json_string_or_null(item.requires_python.as_deref()));
        out.push_str(",\"sha256\":");
        out.push_str(&json_escape(&item.sha256));
        out.push_str(",\"targets\":[");
        let mut targets: Vec<String> =
            item.targets.iter().map(|t| format!("{}:{}", t.python_version, t.platform)).collect();
        targets.sort();
        for (j, t) in targets.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            out.push_str(&json_escape(t));
        }
        out.push_str("],\"version\":");
        out.push_str(&json_escape(&item.package_version));
        out.push_str(",\"yanked\":");
        out.push_str(&json_string_or_null(item.yanked_reason.as_deref()));
        out.push('}');
    }
    out.push_str("],\"schema_version\":1}\n");
    out.into_bytes()
}

pub fn project_pages(items: &[IndexArtifact]) -> BTreeMap<String, Vec<u8>> {
    let mut grouped: BTreeMap<String, Vec<IndexArtifact>> = BTreeMap::new();
    for item in items {
        grouped.entry(project_name(item)).or_default().push(item.clone());
    }
    let mut pages = BTreeMap::new();
    pages.insert("simple/index.html".to_string(), render_root_page(items));
    for (name, group) in &grouped {
        pages.insert(format!("simple/{name}/index.html"), render_project_page(group));
    }
    pages
}

pub fn generation_id(
    manifest: &[u8],
    pages: &BTreeMap<String, Vec<u8>>,
    items: &[IndexArtifact],
    public_base_url: &str,
) -> String {
    let mut h = Sha256::new();
    h.update(b"mypypi-simple-html-v5\0");
    h.update(manifest);
    let mut names: Vec<&String> = pages.keys().collect();
    names.sort();
    for name in names {
        h.update(name.as_bytes());
        h.update(b"\0");
        h.update(&pages[name]);
    }
    for item in items {
        h.update(&item.metadata);
    }
    let catalog = catalog_for(stuff_catalog::FINGERPRINT_GENERATION, items);
    h.update(stuff_catalog::catalog_fingerprint(&catalog, public_base_url).as_bytes());
    format!("{:x}", h.finalize())
}

pub fn catalog_for(generation: &str, items: &[IndexArtifact]) -> stuff_catalog::Catalog {
    let inputs = items
        .iter()
        .map(|item| stuff_catalog::PypiInput {
            name: item.package_name.clone(),
            version: item.package_version.clone(),
            yanked: item.yanked_reason.is_some(),
            dependencies: stuff_catalog::Dependencies::runtime_only(crate::wheel_metadata::requires_dist_names(&item.metadata)),
        })
        .collect();
    stuff_catalog::pypi_catalog(generation, inputs).expect("index artifacts form a valid catalog")
}

fn verify_generation(
    generation: &Path,
    manifest: &[u8],
    pages: &BTreeMap<String, Vec<u8>>,
    items: &[IndexArtifact],
    id: &str,
    public_base_url: &str,
) -> Result<()> {
    let inv = |d: &str, p: &Path| pub_error(Kind::Integrity, d, Some(p), None);
    let wheels_link = generation.join("wheels");
    let md = std::fs::symlink_metadata(&wheels_link).map_err(|_| inv("existing package index wheel URL path is invalid", &wheels_link))?;
    if !md.file_type().is_symlink() || std::fs::read_link(&wheels_link).map(|t| t != Path::new("../../wheels")).unwrap_or(true) {
        return Err(inv("existing package index wheel URL path is invalid", &wheels_link));
    }
    let simple = generation.join("simple");
    let md = std::fs::symlink_metadata(&simple).map_err(|_| inv("existing package index simple path is invalid", generation))?;
    if md.file_type().is_symlink() || !md.file_type().is_dir() {
        return Err(inv("existing package index simple path is invalid", generation));
    }
    let manifest_path = generation.join("manifest.json");
    let md = std::fs::symlink_metadata(&manifest_path).map_err(|_| inv("existing package index generation has an invalid manifest", generation))?;
    if !md.file_type().is_file()
        || std::fs::read(&manifest_path).map(|b| b != manifest).unwrap_or(true)
    {
        return Err(inv("existing package index generation has an invalid manifest", generation));
    }
    for (rel, expected) in pages {
        let page = generation.join(rel);
        let parent = page.parent().unwrap_or(generation);
        let md = std::fs::symlink_metadata(parent).map_err(|_| inv("existing package index project path is invalid", parent))?;
        if md.file_type().is_symlink() || !md.file_type().is_dir() {
            return Err(inv("existing package index project path is invalid", parent));
        }
        let md = std::fs::symlink_metadata(&page).map_err(|_| inv("existing package index generation has an invalid project page", &page))?;
        if !md.file_type().is_file() || std::fs::read(&page).map(|b| b != *expected).unwrap_or(true) {
            return Err(inv("existing package index generation has an invalid project page", &page));
        }
    }
    let catalog = catalog_for(id, items);
    let expected_json = stuff_catalog::render_catalog_json(&catalog);
    let catalog_path = generation.join("catalog.json");
    let md = std::fs::symlink_metadata(&catalog_path).map_err(|_| inv("existing package index generation has an invalid catalog", generation))?;
    if !md.file_type().is_file() || std::fs::read(&catalog_path).map(|b| b != expected_json.as_bytes()).unwrap_or(true) {
        return Err(inv("existing package index generation has an invalid catalog", generation));
    }
    let expected_html = stuff_catalog::render_html(&catalog);
    let index_path = generation.join("index.html");
    let md = std::fs::symlink_metadata(&index_path).map_err(|_| inv("existing package index generation has an invalid catalog page", generation))?;
    if !md.file_type().is_file() || std::fs::read(&index_path).map(|b| b != expected_html.as_bytes()).unwrap_or(true) {
        return Err(inv("existing package index generation has an invalid catalog page", generation));
    }
    let expected_llms = stuff_catalog::render_llms_txt(&catalog, public_base_url);
    let llms_path = generation.join("llms.txt");
    let md = std::fs::symlink_metadata(&llms_path).map_err(|_| inv("existing package index generation has an invalid llms.txt", generation))?;
    if !md.file_type().is_file() || std::fs::read(&llms_path).map(|b| b != expected_llms.as_bytes()).unwrap_or(true) {
        return Err(inv("existing package index generation has an invalid llms.txt", generation));
    }
    Ok(())
}

fn write_generation(
    site: &Site,
    items: &[IndexArtifact],
    manifest: &[u8],
    public_base_url: &str,
) -> Result<()> {
    let pages = project_pages(items);
    let generation = generation_id(manifest, &pages, items, public_base_url);
    let build = |staging: &Path| -> stuff_store::Result<()> {
        std::fs::create_dir(staging.join("simple"))?;
        std::os::unix::fs::symlink(Path::new("../../wheels"), staging.join("wheels"))?;
        for (rel, content) in &pages {
            write_file_durable(&staging.join(rel), content)?;
        }
        write_file_durable(&staging.join("manifest.json"), manifest)?;
        let catalog = catalog_for(&generation, items);
        write_file_durable(
            &staging.join("catalog.json"),
            stuff_catalog::render_catalog_json(&catalog).as_bytes(),
        )?;
        write_file_durable(
            &staging.join("index.html"),
            stuff_catalog::render_html(&catalog).as_bytes(),
        )?;
        write_file_durable(
            &staging.join("llms.txt"),
            stuff_catalog::render_llms_txt(&catalog, public_base_url).as_bytes(),
        )?;
        Ok(())
    };
    let verify = |generation_dir: &Path| -> stuff_store::Result<bool> {
        Ok(verify_generation(
            generation_dir,
            manifest,
            &pages,
            items,
            &generation,
            public_base_url,
        )
        .is_ok())
    };
    site.publish(&generation, &build, &verify).map_err(|e| {
        let kind = match &e {
            stuff_store::Error::Input(_) => Kind::InvalidInput,
            stuff_store::Error::Integrity(_) => Kind::Integrity,
            stuff_store::Error::Conflict(_) => Kind::Conflict,
            stuff_store::Error::Limit(_) => Kind::Limit,
            stuff_store::Error::Network(_) => Kind::Network,
            stuff_store::Error::Operation(_) => Kind::Io,
        };
        pub_error(kind, format!("could not publish package index generation: {e}"), None, None)
    })?;
    Ok(())
}

fn current_items(site: &Site) -> Result<Vec<IndexArtifact>> {
    let current = site.current_link();
    let md = match std::fs::symlink_metadata(&current) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(pub_error(Kind::Io, format!("could not inspect current index path: {e}"), Some(&current), None)),
        Ok(m) => m,
    };
    if !md.file_type().is_symlink() {
        return Err(pub_error(Kind::Conflict, "current index path is not a symlink", Some(&current), None));
    }
    let generation = site
        .current_generation()
        .map_err(|e| pub_error(Kind::Integrity, format!("published index has an invalid current link: {e}"), Some(&current), None))?
        .ok_or_else(|| pub_error(Kind::Integrity, "published index has an invalid current link", Some(&current), None))?;
    if !is_lower_hex64(&generation) {
        return Err(pub_error(Kind::Integrity, "published index has an invalid current link", Some(&current), None));
    }
    let generation = site.generations_dir().join(generation);
    let manifest_path = generation.join("manifest.json");
    let gen_md = std::fs::symlink_metadata(&generation)
        .map_err(|_| pub_error(Kind::Integrity, "could not read current publication manifest", Some(&manifest_path), None))?;
    if gen_md.file_type().is_symlink() || !gen_md.file_type().is_dir() {
        return Err(pub_error(Kind::Integrity, "could not read current publication manifest", Some(&manifest_path), None));
    }
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|_| pub_error(Kind::Integrity, "could not read current publication manifest", Some(&manifest_path), None))?;
    let rows = parse_manifest(&text, &manifest_path)?;
    let wheels = site.site_dir.join("wheels");
    let wheels_md = std::fs::symlink_metadata(&wheels)
        .map_err(|_| pub_error(Kind::Integrity, "current wheel storage is missing", Some(&wheels), None))?;
    if wheels_md.file_type().is_symlink() || !wheels_md.file_type().is_dir() {
        return Err(pub_error(Kind::Integrity, "current wheel storage is not a real directory", Some(&wheels), None));
    }
    let mut artifacts = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if !seen.insert(row.filename.clone()) {
            return Err(pub_error(Kind::Integrity, format!("duplicate wheel filename {}", row.filename), Some(&manifest_path), None));
        }
        let identity = WheelIdentity::parse(&row.filename).map_err(|e| {
            pub_error(Kind::Integrity, format!("invalid published artifact: {}", e.detail), Some(&manifest_path), None)
        })?;
        if row.name != identity.package.name || row.version != identity.package.version.to_string() {
            return Err(pub_error(Kind::Integrity, "manifest identity differs from wheel filename", Some(&manifest_path), None));
        }
        if !is_lower_hex64(&row.sha256) {
            return Err(pub_error(Kind::Integrity, "invalid wheel hash", Some(&manifest_path), None));
        }
        if !is_lower_hex64(&row.metadata_sha256) {
            return Err(pub_error(Kind::Integrity, "invalid metadata hash", Some(&manifest_path), None));
        }
        let wheel = wheels.join(&row.filename);
        let wheel_md = std::fs::symlink_metadata(&wheel)
            .map_err(|_| pub_error(Kind::Integrity, format!("published wheel is missing or changed: {}", row.filename), Some(&manifest_path), None))?;
        if !wheel_md.file_type().is_file() || file_sha256(&wheel).unwrap_or_default() != row.sha256 {
            return Err(pub_error(Kind::Integrity, format!("published wheel is missing or changed: {}", row.filename), Some(&manifest_path), None));
        }
        let sidecar = wheels.join(format!("{}.metadata", row.filename));
        let sidecar_md = std::fs::symlink_metadata(&sidecar)
            .map_err(|_| pub_error(Kind::Integrity, format!("published metadata is missing or invalid: {}", row.filename), Some(&manifest_path), None))?;
        if !sidecar_md.file_type().is_file() {
            return Err(pub_error(Kind::Integrity, format!("published metadata is missing or invalid: {}", row.filename), Some(&manifest_path), None));
        }
        let sidecar_bytes = std::fs::read(&sidecar)
            .map_err(|_| pub_error(Kind::Integrity, format!("published metadata is missing or invalid: {}", row.filename), Some(&manifest_path), None))?;
        let mut mh = Sha256::new();
        mh.update(&sidecar_bytes);
        if format!("{:x}", mh.finalize()) != row.metadata_sha256 {
            return Err(pub_error(Kind::Integrity, format!("published metadata changed: {}", row.filename), Some(&manifest_path), None));
        }
        if row.targets.is_empty() {
            return Err(pub_error(Kind::Integrity, "invalid validation targets", Some(&manifest_path), None));
        }
        {
            let mut seen_targets = BTreeSet::new();
            for t in &row.targets {
                if !seen_targets.insert(t.clone()) {
                    return Err(pub_error(Kind::Integrity, "invalid validation targets", Some(&manifest_path), None));
                }
            }
        }
        let mut targets = Vec::new();
        for value in &row.targets {
            let Some((py_text, platform)) = value.split_once(':') else {
                return Err(pub_error(Kind::Integrity, "invalid validation target", Some(&manifest_path), None));
            };
            if platform.is_empty() {
                return Err(pub_error(Kind::Integrity, "invalid validation target", Some(&manifest_path), None));
            }
            let python_version: pep440_rs::Version = py_text.parse().map_err(|_| {
                pub_error(Kind::Integrity, "invalid validation target", Some(&manifest_path), None)
            })?;
            targets.push(Target { python_version, platform: platform.to_string() });
        }
        let artifact = ValidatedArtifact::new(
            identity,
            wheel.clone(),
            row.sha256.clone(),
            targets.clone(),
        )
        .map_err(|e| pub_error(Kind::Integrity, format!("invalid published artifact: {}", e.detail), Some(&manifest_path), None))?;
        let prepared = prepare_index_artifacts(std::slice::from_ref(&artifact), &BTreeMap::new())
            .map_err(|_| pub_error(Kind::Integrity, format!("published metadata disagrees with manifest: {}", row.filename), Some(&manifest_path), None))?;
        let item = prepared.into_iter().next().unwrap();
        if item.metadata != sidecar_bytes || item.requires_python.as_deref() != row.requires_python.as_deref() {
            return Err(pub_error(Kind::Integrity, format!("published metadata disagrees with manifest: {}", row.filename), Some(&manifest_path), None));
        }
        let mut item = item;
        item.yanked_reason = row.yanked;
        item.targets = targets;
        artifacts.push(item);
    }
    Ok(artifacts)
}

fn is_lower_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

struct ManifestRow {
    filename: String,
    name: String,
    version: String,
    sha256: String,
    metadata_sha256: String,
    requires_python: Option<String>,
    yanked: Option<String>,
    targets: Vec<String>,
}

fn parse_manifest(text: &str, manifest_path: &Path) -> Result<Vec<ManifestRow>> {
    let fail = |d: &str| pub_error(Kind::Integrity, d, Some(manifest_path), None);
    // minimal JSON parse sufficient for our own manifest shape
    let value = crate::json::parse(text).map_err(|e| fail(&format!("could not read current publication manifest: {e}")))?;
    let obj = value.as_obj().ok_or_else(|| fail("current publication manifest has an unknown schema"))?;
    if obj.len() != 2 {
        return Err(fail("current publication manifest has an unknown schema"));
    }
    let schema = obj.iter().find(|(k, _)| *k == "schema_version").ok_or_else(|| fail("current publication manifest has an unknown schema"))?;
    if schema.1.as_num() != Some("1") {
        return Err(fail("current publication manifest has an unknown schema"));
    }
    let artifacts = obj.iter().find(|(k, _)| *k == "artifacts").ok_or_else(|| fail("current publication manifest has an unknown schema"))?;
    let arr = artifacts.1.as_arr().ok_or_else(|| fail("current publication manifest has an unknown schema"))?;
    let mut rows = Vec::new();
    for entry in arr {
        let map = entry.as_obj().ok_or_else(|| fail("invalid published artifact"))?;
        if map.len() != 8 {
            return Err(fail("manifest artifact has unknown or missing fields"));
        }
        for key in ["filename", "name", "version", "sha256", "metadata_sha256", "requires_python", "yanked", "targets"] {
            if !map.iter().any(|(k, _)| k == key) {
                return Err(fail("manifest artifact has unknown or missing fields"));
            }
        }
        let get_str = |key: &str| -> Result<String> {
            map.iter()
                .find(|(k, _)| *k == key)
                .and_then(|(_, v)| v.as_str().map(|s| s.to_string()))
                .ok_or_else(|| fail("invalid published artifact"))
        };
        let get_opt = |key: &str| -> Result<Option<String>> {
            let v = map.iter().find(|(k, _)| *k == key).ok_or_else(|| fail("invalid published artifact"))?;
            if v.1.is_null() {
                Ok(None)
            } else if let Some(s) = v.1.as_str() {
                Ok(Some(s.to_string()))
            } else {
                Err(fail("invalid published artifact"))
            }
        };
        let filename = get_str("filename")?;
        if Path::new(&filename).file_name().and_then(|s| s.to_str()) != Some(filename.as_str()) {
            return Err(fail("invalid wheel filename"));
        }
        let targets_val = map.iter().find(|(k, _)| *k == "targets").unwrap().1.clone();
        let targets_arr = targets_val.as_arr().ok_or_else(|| fail("invalid validation targets"))?;
        let mut targets = Vec::new();
        for t in targets_arr {
            targets.push(t.as_str().ok_or_else(|| fail("invalid validation target"))?.to_string());
        }
        let yanked = get_opt("yanked")?;
        if let Some(ref y) = yanked
            && y.trim().is_empty() {
                return Err(fail("invalid yanked reason"));
            }
        rows.push(ManifestRow {
            filename,
            name: get_str("name")?,
            version: get_str("version")?,
            sha256: get_str("sha256")?,
            metadata_sha256: get_str("metadata_sha256")?,
            requires_python: get_opt("requires_python")?,
            yanked,
            targets,
        });
        let _ = rows;
    }
    Ok(rows)
}

pub fn published_artifacts(output: &Path) -> Result<Vec<ValidatedArtifact>> {
    let output = std::path::absolute(output)
        .map_err(|e| pub_error(Kind::Io, format!("could not inspect published index: {e}"), Some(output), None))?;
    match std::fs::symlink_metadata(&output) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(pub_error(Kind::Io, format!("could not inspect published index: {e}"), Some(&output), None)),
        Ok(md) => {
            if md.file_type().is_symlink() || !md.file_type().is_dir() {
                return Err(pub_error(Kind::InvalidInput, "published index root must be a real directory", Some(&output), None));
            }
        }
    }
    Ok(current_items(&Site::new(output.clone()))?
        .into_iter()
        .map(|item| {
            let identity = WheelIdentity::parse(&item.wheel_filename).expect("stored filename parses");
            let path = item_target_path(&output, &item);
            ValidatedArtifact::new(identity, path, item.sha256.clone(), item.targets.clone()).expect("stored artifact valid")
        })
        .collect())
}

// helpers to rebuild ValidatedArtifact from IndexArtifact + output root
fn item_target_path(output: &Path, item: &IndexArtifact) -> PathBuf {
    output.join("wheels").join(&item.wheel_filename)
}

pub fn republish(output: &Path, public_base_url: &str) -> Result<PathBuf> {
    if published_artifacts(output)?.is_empty() {
        return Err(pub_error(Kind::InvalidInput, "no published index to republish", Some(output), None));
    }
    publish(&[], output, &BTreeMap::new(), public_base_url)
}

pub fn publish(
    artifacts: &[ValidatedArtifact],
    output: &Path,
    yanked: &BTreeMap<String, Option<String>>,
    public_base_url: &str,
) -> Result<PathBuf> {
    for (filename, reason) in yanked {
        if filename.is_empty() {
            return Err(pub_error(Kind::InvalidInput, "yanked metadata filenames must be non-empty strings", None, None));
        }
        if let Some(r) = reason
            && r.trim().is_empty()
        {
            return Err(pub_error(Kind::InvalidInput, "yanked reason must be a non-empty string", None, None));
        }
    }
    let new_filenames: BTreeSet<&str> = artifacts.iter().map(|a| a.wheel.filename.as_str()).collect();
    let mut reasons: BTreeMap<String, String> = BTreeMap::new();
    for (filename, reason) in yanked {
        if new_filenames.contains(filename.as_str())
            && let Some(r) = reason
        {
            reasons.insert(filename.clone(), r.clone());
        }
    }
    let new_items = prepare_index_artifacts(artifacts, &reasons).map_err(|e| {
        let kind = match &e.kind {
            Kind::InvalidInput => Kind::InvalidInput,
            Kind::Configuration => Kind::Configuration,
            Kind::MissingArtifact => Kind::MissingArtifact,
            Kind::Integrity => Kind::Integrity,
            Kind::Incompatible => Kind::Incompatible,
            Kind::Dependency => Kind::Dependency,
            Kind::Build => Kind::Build,
            Kind::Validation => Kind::Validation,
            Kind::Conflict => Kind::Conflict,
            Kind::Limit => Kind::Limit,
            Kind::Network => Kind::Network,
            Kind::Io => Kind::Io,
        };
        let mut out = Error::new(kind, e.detail.clone(), Stage::Publication);
        out.package = e.package.clone();
        out.target = e.target.clone();
        out
    })?;
    let output = ensure_output_root(output)?;
    let site = Site::new(output.clone());
    let _lock = lock_site(&site)?;
    site.recover_staging()
        .map_err(|e| pub_error(Kind::Io, e.to_string(), Some(&output), None))?;
    let current = current_items(&site)?;
    let mut merged: BTreeMap<String, IndexArtifact> = BTreeMap::new();
    for item in current {
        merged.insert(item.wheel_filename.clone(), item);
    }
    for item in new_items {
        let filename = item.wheel_filename.clone();
        match merged.get(&filename) {
            None => {
                merged.insert(filename, item);
            }
            Some(existing) => {
                if existing.sha256 != item.sha256 {
                    return Err(pub_error(Kind::Conflict, "wheel filename already exists with different bytes", Some(&output.join("wheels").join(&filename)), Some(&item)));
                }
                if existing.metadata != item.metadata {
                    return Err(pub_error(Kind::Conflict, "wheel metadata differs for identical wheel bytes", Some(&output.join("wheels").join(&filename)), Some(&item)));
                }
                let mut targets = existing.targets.clone();
                for t in &item.targets {
                    if !targets.contains(t) {
                        targets.push(t.clone());
                    }
                }
                let yanked_reason = if yanked.contains_key(&filename) {
                    yanked.get(&filename).cloned().flatten()
                } else {
                    existing.yanked_reason.clone()
                };
                merged.insert(filename, IndexArtifact { targets, yanked_reason, ..item });
            }
        }
    }
    for (filename, reason) in yanked {
        if merged.contains_key(filename) && !new_filenames.contains(filename.as_str())
            && let Some(item) = merged.get_mut(filename) {
                item.yanked_reason = reason.clone();
            }
    }
    let unknown: Vec<&String> = yanked.keys().filter(|k| !merged.contains_key(*k)).collect();
    if !unknown.is_empty() {
        let mut sorted = unknown.clone();
        sorted.sort();
        let list: Vec<&str> = sorted.iter().map(|s| s.as_str()).collect();
        return Err(pub_error(Kind::InvalidInput, format!("yanked metadata names unknown wheels: {}", list.join(", ")), None, None));
    }
    let items: Vec<IndexArtifact> = merged.into_values().collect();
    // store wheels: need source paths for new items
    let by_filename: BTreeMap<&str, &ValidatedArtifact> =
        artifacts.iter().map(|a| (a.wheel.filename.as_str(), a)).collect();
    for item in &items {
        let source: PathBuf = if let Some(src) = by_filename.get(item.wheel_filename.as_str()) {
            src.path.clone()
        } else {
            // already stored; source is the stored wheel itself
            output.join("wheels").join(&item.wheel_filename)
        };
        store_wheel(&output, item, &source)?;
    }
    let manifest = manifest_bytes(&items);
    write_generation(&site, &items, &manifest, public_base_url)?;
    Ok(output.join("current"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PackageVersion;
    use sha2::{Digest, Sha256};

    fn make_validated(dir: &Path, filename: &str, metadata: &[u8], targets: Vec<Target>) -> ValidatedArtifact {
        let identity = WheelIdentity::parse(filename).unwrap();
        let path = dir.join(format!("src-{filename}"));
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut archive = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            let dist_info = format!(
                "{}-{}.dist-info/METADATA",
                identity.package.name.replace('-', "_"),
                identity.package.version
            );
            archive.start_file(&dist_info, options).unwrap();
            use std::io::Write as _;
            archive.write_all(metadata).unwrap();
            archive.finish().unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut h = Sha256::new();
        h.update(&bytes);
        ValidatedArtifact::new(identity, path, format!("{:x}", h.finalize()), targets).unwrap()
    }

    fn target(platform: &str) -> Target {
        Target::new("3.15", platform).unwrap()
    }

    #[test]
    fn publish_round_trip_and_manifest_shape() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let out = dir.path().join("index");
        let a = make_validated(
            &src,
            "sample_pkg-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.0\nRequires-Python: >=3.15,<3.16\n\n",
            vec![target("macos-arm64")],
        );
        let current = publish(std::slice::from_ref(&a), &out, &BTreeMap::new(), stuff_catalog::DEFAULT_PYPI_ORIGIN).unwrap();
        assert!(current.ends_with("current"));
        let manifest = std::fs::read_to_string(out.join("current").join("manifest.json").canonicalize().unwrap_or(out.join("current").join("manifest.json"))).unwrap_or_else(|_| {
            let link = std::fs::read_link(&current).unwrap();
            std::fs::read_to_string(out.join(link).join("manifest.json")).unwrap()
        });
        assert!(manifest.contains("\"schema_version\":1"));
        assert!(manifest.ends_with('\n'));
        let published = published_artifacts(&out).unwrap();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].wheel.filename, "sample_pkg-1.0-py3-none-any.whl");
        // republishing identical bytes is a no-op merge, not a conflict
        publish(std::slice::from_ref(&a), &out, &BTreeMap::new(), stuff_catalog::DEFAULT_PYPI_ORIGIN).unwrap();
    }

    #[test]
    fn republish_keeps_wheels_and_refuses_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let out = dir.path().join("index");
        assert!(republish(&out, stuff_catalog::DEFAULT_PYPI_ORIGIN).is_err());
        assert!(!out.exists());

        let a = make_validated(
            &src,
            "sample_pkg-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.0\nRequires-Dist: other-pkg\n\n",
            vec![target("macos-arm64")],
        );
        publish(std::slice::from_ref(&a), &out, &BTreeMap::new(), stuff_catalog::DEFAULT_PYPI_ORIGIN).unwrap();
        let before = std::fs::read_link(out.join("current")).unwrap();
        republish(&out, "https://mirror.example.test").unwrap();
        let after = std::fs::read_link(out.join("current")).unwrap();
        assert_ne!(before, after);
        let published = published_artifacts(&out).unwrap();
        assert_eq!(published.len(), 1);
        let catalog = std::fs::read_to_string(out.join("current").join("catalog.json")).unwrap();
        assert!(catalog.contains("\"dependencies\": {\"runtime\": [\"other-pkg\"], \"peer\": [], \"build\": [], \"dev\": []}"));
    }

    #[test]
    fn conflicting_bytes_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let out = dir.path().join("index");
        let a = make_validated(
            &src,
            "sample_pkg-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.0\n\n",
            vec![target("macos-arm64")],
        );
        publish(std::slice::from_ref(&a), &out, &BTreeMap::new(), stuff_catalog::DEFAULT_PYPI_ORIGIN).unwrap();
        let mut b = make_validated(
            &src,
            "sample_pkg-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.1\n\n",
            vec![target("macos-arm64")],
        );
        // force same filename with different bytes but keep identity consistent-ish:
        // rewrite bytes directly to simulate conflict
        b.sha256 = "0".repeat(64);
        let err = publish(std::slice::from_ref(&b), &out, &BTreeMap::new(), stuff_catalog::DEFAULT_PYPI_ORIGIN).unwrap_err();
        assert!(matches!(err.kind, Kind::Conflict | Kind::Integrity));
    }

    #[test]
    fn package_version_parses() {
        let _ = PackageVersion::parse("sample-pkg", "1.0").unwrap();
    }

}
