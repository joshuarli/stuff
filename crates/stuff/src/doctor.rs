#![forbid(unsafe_code)]

use crate::error::Error;
use crate::source::{fetch_bytes, LocalSite};
use stuff_catalog::{
    DEFAULT_CARGO_ORIGIN, DEFAULT_NPM_ORIGIN, DEFAULT_OCI_ORIGIN, DEFAULT_PYPI_ORIGIN,
};

pub struct RegistrySources {
    pub cargo_base: String,
    pub npm_base: String,
    pub pypi_base: String,
    pub oci_base: String,
    pub cargo_local: Option<LocalSite>,
    pub npm_local: Option<LocalSite>,
    pub pypi_local: Option<LocalSite>,
    pub oci_local: Option<LocalSite>,
}

impl RegistrySources {
    pub fn from_parsed(parsed: &crate::cli::Parsed) -> crate::error::Result<Self> {
        Ok(Self {
            cargo_base: parsed
                .cargo_base
                .clone()
                .or_else(|| std::env::var("STUFF_CARGO_BASE").ok())
                .unwrap_or_else(|| DEFAULT_CARGO_ORIGIN.to_string()),
            npm_base: parsed
                .npm_base
                .clone()
                .or_else(|| std::env::var("STUFF_NPM_BASE").ok())
                .unwrap_or_else(|| DEFAULT_NPM_ORIGIN.to_string()),
            pypi_base: parsed
                .pypi_base
                .clone()
                .or_else(|| std::env::var("STUFF_PYPI_BASE").ok())
                .unwrap_or_else(|| DEFAULT_PYPI_ORIGIN.to_string()),
            oci_base: parsed
                .oci_base
                .clone()
                .or_else(|| std::env::var("STUFF_OCI_BASE").ok())
                .unwrap_or_else(|| DEFAULT_OCI_ORIGIN.to_string()),
            cargo_local: resolve_local(parsed.cargo_dir.as_deref())?,
            npm_local: resolve_local(parsed.npm_dir.as_deref())?,
            pypi_local: resolve_local(parsed.pypi_dir.as_deref())?,
            oci_local: resolve_local(parsed.oci_dir.as_deref())?,
        })
    }

    pub fn selected(&self, filter: Option<&str>) -> Vec<(&'static str, bool)> {
        let all = [
            ("cargo", self.cargo_local.is_some()),
            ("npm", self.npm_local.is_some()),
            ("pypi", self.pypi_local.is_some()),
            ("oci", self.oci_local.is_some()),
        ];
        match filter {
            Some(f) => all.into_iter().filter(|(name, _)| *name == f).collect(),
            None => all.to_vec(),
        }
    }

    fn any_local(&self) -> bool {
        self.cargo_local.is_some()
            || self.npm_local.is_some()
            || self.pypi_local.is_some()
            || self.oci_local.is_some()
    }

    fn local(&self, name: &str) -> Option<&LocalSite> {
        match name {
            "cargo" => self.cargo_local.as_ref(),
            "npm" => self.npm_local.as_ref(),
            "oci" => self.oci_local.as_ref(),
            _ => self.pypi_local.as_ref(),
        }
    }
}

fn resolve_local(input: Option<&str>) -> crate::error::Result<Option<LocalSite>> {
    input.map(|p| LocalSite::resolve(std::path::Path::new(p))).transpose()
}

pub fn load_catalogs(
    sources: &RegistrySources,
    filter: Option<&str>,
) -> crate::error::Result<Vec<stuff_catalog::Catalog>> {
    let any_local = sources.any_local();
    let mut out = Vec::new();
    for (name, is_local) in sources.selected(filter) {
        if filter.is_none() && any_local && !is_local {
            continue;
        }
        let catalog = match (name, sources.local(name)) {
            ("cargo", Some(local)) => local.load_catalog()?,
            ("npm", Some(local)) => local.load_catalog()?,
            ("pypi", Some(local)) => local.load_catalog()?,
            ("oci", Some(local)) => local.load_catalog()?,
            ("cargo", None) => crate::source::fetch_catalog(&sources.cargo_base)?,
            ("npm", None) => crate::source::fetch_catalog(&sources.npm_base)?,
            ("oci", None) => crate::source::fetch_catalog(&sources.oci_base)?,
            (_, None) => crate::source::fetch_catalog(&sources.pypi_base)?,
            _ => unreachable!(),
        };
        let expected = match name {
            "cargo" => stuff_catalog::Registry::Cargo,
            "npm" => stuff_catalog::Registry::Npm,
            "oci" => stuff_catalog::Registry::Oci,
            _ => stuff_catalog::Registry::Pypi,
        };
        if catalog.registry != expected {
            return Err(Error::Conflict(format!(
                "{name} catalog has unexpected registry {}",
                catalog.registry.as_str()
            )));
        }
        out.push(catalog);
    }
    out.sort_by(|a: &stuff_catalog::Catalog, b: &stuff_catalog::Catalog| {
        a.registry.as_str().cmp(b.registry.as_str())
    });
    Ok(out)
}

fn cargo_shard(lower_name: &str) -> Option<String> {
    stuff_cargo_shard::shard_for_name(lower_name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckState {
    Pass,
    Fail,
    Skip,
}

struct Check {
    name: String,
    state: CheckState,
    detail: String,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), state: CheckState::Pass, detail: detail.into() }
    }

    fn fail(name: impl Into<String>, detail: impl std::fmt::Display) -> Self {
        Self { name: name.into(), state: CheckState::Fail, detail: detail.to_string() }
    }

    fn skip(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), state: CheckState::Skip, detail: detail.into() }
    }
}

pub fn run_doctor(parsed: &crate::cli::Parsed) -> crate::error::Result<bool> {
    let checks = doctor_checks(parsed)?;
    report(&checks);
    Ok(checks.iter().all(|c| c.state != CheckState::Fail))
}

fn doctor_checks(parsed: &crate::cli::Parsed) -> crate::error::Result<Vec<Check>> {
    let sources = RegistrySources::from_parsed(parsed)?;
    let filter = parsed.registry_filter();
    let sample = parsed.sample_artifacts;
    let mut checks: Vec<Check> = Vec::new();
    let skipped: Vec<&str> = if filter.is_none() && sources.any_local() {
        sources
            .selected(filter)
            .into_iter()
            .filter(|(_, is_local)| !is_local)
            .map(|(name, _)| name)
            .collect()
    } else {
        Vec::new()
    };
    if parsed.expected_generation.is_some() && !skipped.is_empty() {
        return Err(Error::usage(format!(
            "--expected-generation requires every registry; {} skipped without {}",
            skipped.join(", "),
            skipped
                .iter()
                .map(|name| format!("--{name}-dir"))
                .collect::<Vec<_>>()
                .join("/")
        )));
    }
    for name in &skipped {
        checks.push(Check::skip(
            format!("{name} registry"),
            format!("skipped: pass --{name}-dir to include it"),
        ));
    }
    let catalogs = match load_catalogs(&sources, filter) {
        Ok(c) => c,
        Err(e) => {
            if matches!(&e, Error::Usage(_)) {
                return Err(e);
            }
            checks.push(Check::fail("catalog load", e));
            return Ok(checks);
        }
    };
    if catalogs.is_empty() {
        return Err(Error::usage("no registries selected"));
    }
    for catalog in &catalogs {
        let name = catalog.registry.as_str();
        checks.push(Check::pass(format!("{name} catalog"), format!("generation {}: {} packages", catalog.generation, catalog.package_count())));
        if let Some(expected) = parsed.expected_generation.as_deref()
            && catalog.generation != expected {
                checks.push(Check::fail(
                    format!("{name} generation"),
                    format!("active {} does not match expected {expected}", catalog.generation),
                ));
            }
        if let Some(local) = sources.local(name) {
            let origin = origin_for(name, &sources);
            run_local_checks(&mut checks, name, catalog, local, &origin, sample);
        } else {
            run_remote_checks(&mut checks, name, catalog, &sources, sample);
        }
    }
    Ok(checks)
}

fn origin_for(name: &str, sources: &RegistrySources) -> String {
    let base = match name {
        "cargo" => &sources.cargo_base,
        "npm" => &sources.npm_base,
        "oci" => &sources.oci_base,
        _ => &sources.pypi_base,
    };
    base.trim_end_matches('/').to_string()
}

fn run_local_checks(
    checks: &mut Vec<Check>,
    name: &str,
    catalog: &stuff_catalog::Catalog,
    local: &LocalSite,
    origin: &str,
    sample: bool,
) {
    let site_dir = &local.site_root;
    let gen_dir = &local.generation_dir;
    if catalog.packages.is_empty() {
        checks.push(Check::pass(format!("{name} endpoints"), "empty catalog: nothing further to check"));
        return;
    }
    let expected_llms = stuff_catalog::render_llms_txt(catalog, origin);
    match std::fs::read_to_string(gen_dir.join("llms.txt")) {
        Ok(actual) if actual == expected_llms => {
            checks.push(Check::pass(format!("{name} llms.txt"), "llms.txt matches catalog".to_string()));
        }
        Ok(_) => checks.push(Check::fail(format!("{name} llms.txt"), "llms.txt does not match catalog".to_string())),
        Err(e) => checks.push(Check::fail(format!("{name} llms.txt"), format!("llms.txt missing: {e}"))),
    }
    let first = &catalog.packages[0];
    match name {
        "cargo" => match cargo_shard(&first.name.to_lowercase()) {
            Some(shard) => {
                let candidates = [gen_dir.join("registry").join(&shard), gen_dir.join("crates-io").join(&shard)];
                if candidates.iter().any(|p| p.is_file()) {
                    checks.push(Check::pass(format!("{name} index"), format!("{} present", first.name)));
                } else {
                    checks.push(Check::fail(format!("{name} index"), format!("{} missing ({}:{})", first.name, catalog.generation, shard)));
                }
            }
            None => checks.push(Check::fail(
                format!("{name} index"),
                format!("{} is not an ASCII crate name", first.name),
            )),
        },
        "npm" => {
            let packument = gen_dir.join(&first.name);
            if packument.is_file() {
                checks.push(Check::pass(format!("{name} packument"), format!("{} present", first.name)));
            } else {
                checks.push(Check::fail(format!("{name} packument"), format!("{} missing", first.name)));
            }
            let manifest_path = gen_dir.join("manifest.json");
            if manifest_path.is_file() {
                checks.push(Check::pass(format!("{name} manifest"), "manifest.json present".to_string()));
            } else {
                checks.push(Check::fail(format!("{name} manifest"), "manifest.json missing".to_string()));
            }
        }
        "oci" => {
            checks.extend(oci_local_checks(catalog, local));
        }
        _ => {
            let page = gen_dir.join("simple").join(first.name.to_lowercase().replace(['_', '.'], "-").replace("--", "-")).join("index.html");
            let root = gen_dir.join("simple").join("index.html");
            if root.is_file() {
                checks.push(Check::pass(format!("{name} simple index"), "simple/index.html present".to_string()));
            } else {
                checks.push(Check::fail(format!("{name} simple index"), "simple/index.html missing".to_string()));
            }
            if page.is_file() {
                checks.push(Check::pass(format!("{name} project page"), format!("{} present", first.name)));
            } else {
                checks.push(Check::fail(format!("{name} project page"), format!("{} missing", first.name)));
            }
        }
    }
    if sample && !catalog.packages.is_empty() && name != "oci" {
        checks.push(sample_local(name, catalog, site_dir, gen_dir));
    }
}

fn oci_local_checks(catalog: &stuff_catalog::Catalog, local: &LocalSite) -> Vec<Check> {
    let mut checks = Vec::new();
    checks.push(Check::pass(
        "oci current",
        format!("generation {} is current", catalog.generation),
    ));
    let site = &local.site_root;
    let gen_dir = &local.generation_dir;
    for package in &catalog.packages {
        let repository = match stuff_oci::RepositoryName::parse(&package.name) {
            Ok(repository) => repository,
            Err(error) => {
                checks.push(Check::fail(
                    "oci repository".to_string(),
                    format!("{}: {error}", package.name),
                ));
                continue;
            }
        };
        let mut path = gen_dir.join("repositories");
        for component in repository.as_str().split('/') {
            path.push(component);
        }
        let path = path.join("repository.json");
        let view = match std::fs::read_to_string(&path) {
            Ok(text) => match stuff_oci::parse_repository_json(&repository, &text) {
                Ok(view) => view,
                Err(error) => {
                    checks.push(Check::fail(
                        format!("oci repository {}", repository.as_str()),
                        error,
                    ));
                    continue;
                }
            },
            Err(error) => {
                checks.push(Check::fail(
                    format!("oci repository {}", repository.as_str()),
                    format!("could not read {}: {error}", path.display()),
                ));
                continue;
            }
        };
        for version in &package.versions {
            let label = format!("oci manifest {}:{}", repository.as_str(), version.version);
            let tag = match stuff_oci::Tag::parse(&version.version) {
                Ok(tag) => tag,
                Err(error) => {
                    checks.push(Check::fail(label, error));
                    continue;
                }
            };
            let Some(descriptor) = view.tags.get(&tag) else {
                checks.push(Check::fail(
                    label,
                    format!("tag is missing from {}", path.display()),
                ));
                continue;
            };
            match verify_oci_manifest(site, &descriptor.digest, Some(descriptor.size), 0) {
                Ok(()) => checks.push(Check::pass(
                    label.clone(),
                    format!("{} resolves and hashes", descriptor.digest),
                )),
                Err(error) => {
                    checks.push(Check::fail(label.clone(), error));
                    continue;
                }
            }
            match oci_first_config(site, &descriptor.digest, 0) {
                Ok(config) => {
                    let config_label = format!("oci config {}:{}", repository.as_str(), version.version);
                    match verify_oci_blob(site, &config) {
                        Ok(()) => checks.push(Check::pass(
                            config_label,
                            format!("blob {config} hashes"),
                        )),
                        Err(error) => checks.push(Check::fail(config_label, error)),
                    }
                }
                Err(error) => checks.push(Check::fail(
                    format!("oci config {}:{}", repository.as_str(), version.version),
                    error,
                )),
            }
        }
    }
    checks
}

fn verify_oci_manifest(
    site: &std::path::Path,
    digest: &stuff_oci::Digest,
    expected_size: Option<u64>,
    depth: usize,
) -> std::result::Result<(), String> {
    if depth > 16 {
        return Err("manifest graph is too deep".to_string());
    }
    let path = site.join("oci/manifests/sha256").join(digest.hex());
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if let Some(expected) = expected_size
        && bytes.len() as u64 != expected {
            return Err(format!("manifest {digest} size is {}, expected {expected}", bytes.len()));
        }
    let actual = stuff_oci::Digest::from_bytes(&bytes);
    if actual != *digest {
        return Err(format!("manifest {digest} hashes to {actual}"));
    }
    let image = stuff_oci::parse_image(&bytes).map_err(|error| error.to_string())?;
    if let stuff_oci::Image::Index(index) = &image {
        for child in &index.manifests {
            verify_oci_manifest(site, &child.digest, Some(child.size), depth + 1)?;
        }
    }
    Ok(())
}

fn oci_first_config(
    site: &std::path::Path,
    digest: &stuff_oci::Digest,
    depth: usize,
) -> std::result::Result<stuff_oci::Digest, String> {
    if depth > 16 {
        return Err("manifest graph is too deep".to_string());
    }
    let path = site.join("oci/manifests/sha256").join(digest.hex());
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let image = stuff_oci::parse_image(&bytes).map_err(|error| error.to_string())?;
    match &image {
        stuff_oci::Image::Manifest(manifest) => Ok(manifest.config.digest.clone()),
        stuff_oci::Image::Index(index) => {
            for child in &index.manifests {
                if let Ok(config) = oci_first_config(site, &child.digest, depth + 1) {
                    return Ok(config);
                }
            }
            Err(format!("index {digest} has no manifest with a config"))
        }
    }
}

fn verify_oci_blob(
    site: &std::path::Path,
    digest: &stuff_oci::Digest,
) -> std::result::Result<(), String> {
    let path = site.join("blobs/sha256").join(digest.hex());
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let actual = stuff_oci::Digest::from_bytes(&bytes);
    if actual != *digest {
        return Err(format!("blob {digest} hashes to {actual}"));
    }
    Ok(())
}

fn run_remote_checks(
    checks: &mut Vec<Check>,
    name: &str,
    catalog: &stuff_catalog::Catalog,
    sources: &RegistrySources,
    sample: bool,
) {
    let base = match name {
        "cargo" => sources.cargo_base.trim_end_matches('/').to_string(),
        "npm" => sources.npm_base.trim_end_matches('/').to_string(),
        "oci" => sources.oci_base.trim_end_matches('/').to_string(),
        _ => sources.pypi_base.trim_end_matches('/').to_string(),
    };
    let immutable = format!("{base}/generations/{}/catalog.json", catalog.generation);
    match fetch_bytes(&immutable) {
        Ok((200, body)) => match String::from_utf8(body) {
            Ok(text) => match stuff_catalog::parse_catalog_json(&text) {
                Ok(pinned) if pinned.generation == catalog.generation => {
                    checks.push(Check::pass(format!("{name} immutable catalog"), format!("generations/{}/catalog.json matches", catalog.generation)));
                }
                _ => checks.push(Check::fail(format!("{name} immutable catalog"), "pinned catalog does not match current".to_string())),
            },
            Err(_) => checks.push(Check::fail(format!("{name} immutable catalog"), "pinned catalog is not UTF-8".to_string())),
        },
        Ok((status, _)) => checks.push(Check::fail(format!("{name} immutable catalog"), format!("HTTP {status}"))),
        Err(e) => checks.push(Check::fail(format!("{name} immutable catalog"), e)),
    }
    if catalog.packages.is_empty() {
        return;
    }
    match fetch_bytes(&format!("{base}/current/llms.txt")) {
        Ok((200, body)) => match String::from_utf8(body) {
            Ok(text) if text == stuff_catalog::render_llms_txt(catalog, &base) => {
                checks.push(Check::pass(format!("{name} llms.txt"), "current/llms.txt matches".to_string()));
            }
            Ok(_) => checks.push(Check::fail(format!("{name} llms.txt"), "current/llms.txt does not match catalog".to_string())),
            Err(_) => checks.push(Check::fail(format!("{name} llms.txt"), "current/llms.txt is not UTF-8".to_string())),
        },
        Ok((status, _)) => checks.push(Check::fail(format!("{name} llms.txt"), format!("HTTP {status}"))),
        Err(e) => checks.push(Check::fail(format!("{name} llms.txt"), e)),
    }
    let first = &catalog.packages[0];
    match name {
        "cargo" => match cargo_shard(&first.name.to_lowercase()) {
            Some(shard) => {
                let url = format!("{base}/current/registry/{shard}");
                let mirror_url = format!("{base}/current/crates-io/{shard}");
                match fetch_bytes(&url) {
                    Ok((200, _)) => checks.push(Check::pass(format!("{name} index"), format!("{} reachable", first.name))),
                    Ok(_) => match fetch_bytes(&mirror_url) {
                        Ok((200, _)) => checks.push(Check::pass(format!("{name} index"), format!("{} reachable via mirror", first.name))),
                        Ok((status, _)) => checks.push(Check::fail(format!("{name} index"), format!("HTTP {status}"))),
                        Err(e) => checks.push(Check::fail(format!("{name} index"), e)),
                    },
                    Err(e) => checks.push(Check::fail(format!("{name} index"), e)),
                }
            }
            None => checks.push(Check::fail(
                format!("{name} index"),
                format!("{} is not an ASCII crate name", first.name),
            )),
        }
        "npm" => {
            let route = first.name.replace('/', "%2f");
            let url = format!("{base}/current/{route}");
            match fetch_bytes(&url) {
                Ok((200, _)) => checks.push(Check::pass(format!("{name} packument"), format!("{} reachable", first.name))),
                Ok((status, _)) => checks.push(Check::fail(format!("{name} packument"), format!("HTTP {status}"))),
                Err(e) => checks.push(Check::fail(format!("{name} packument"), e)),
            }
        }
        "oci" => oci_remote_checks(checks, catalog, &base),
        _ => {
            let project = first.name.to_lowercase();
            let url = format!("{base}/current/simple/{project}/");
            match fetch_bytes(&url) {
                Ok((200, _)) => checks.push(Check::pass(format!("{name} project page"), format!("{} reachable", first.name))),
                Ok((status, _)) => checks.push(Check::fail(format!("{name} project page"), format!("HTTP {status}"))),
                Err(e) => checks.push(Check::fail(format!("{name} project page"), e)),
            }
        }
    }
    if sample && !catalog.packages.is_empty() && name != "oci" {
        checks.push(sample_remote(name, catalog, &base));
    }
}

fn oci_remote_checks(
    checks: &mut Vec<Check>,
    catalog: &stuff_catalog::Catalog,
    base: &str,
) {
    match crate::source::fetch_response(&format!("{base}/v2/"), None) {
        Ok(response)
            if response.status == 200
                && response.header("docker-distribution-api-version") == Some("registry/2.0") =>
        {
            checks.push(Check::pass("oci ping", "registry/2.0"));
        }
        Ok(response) => checks.push(Check::fail(
            "oci ping",
            format!(
                "HTTP {} version {}",
                response.status,
                response.header("docker-distribution-api-version").unwrap_or("missing")
            ),
        )),
        Err(error) => checks.push(Check::fail("oci ping", error)),
    }
    let Some(first) = catalog.packages.first() else {
        return;
    };
    let tag = &first.latest;
    let label = format!("oci manifest {}:{}", first.name, tag);
    let accept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json";
    let manifest_url = format!("{base}/v2/{}/manifests/{tag}", first.name);
    let response = match crate::source::fetch_response(&manifest_url, Some(accept)) {
        Ok(response) => response,
        Err(error) => {
            checks.push(Check::fail(label, error));
            return;
        }
    };
    if response.status != 200 {
        checks.push(Check::fail(label, format!("HTTP {}", response.status)));
        return;
    }
    let actual = stuff_oci::Digest::from_bytes(&response.body);
    match response.header("docker-content-digest") {
        Some(digest) if digest == actual.as_str() => {
            checks.push(Check::pass(label.clone(), actual.to_string()));
        }
        Some(digest) => {
            checks.push(Check::fail(
                label.clone(),
                format!("Docker-Content-Digest {digest} does not match {actual}"),
            ));
            return;
        }
        None => {
            checks.push(Check::fail(label.clone(), "Docker-Content-Digest is missing"));
            return;
        }
    }
    let image = match stuff_oci::parse_image(&response.body) {
        Ok(image) => image,
        Err(error) => {
            checks.push(Check::fail(label, error));
            return;
        }
    };
    let config = match &image {
        stuff_oci::Image::Manifest(manifest) => Some(manifest.config.digest.clone()),
        stuff_oci::Image::Index(index) => index.manifests.iter().find_map(|child| {
            let url = format!("{base}/v2/{}/manifests/{}", first.name, child.digest);
            let response = crate::source::fetch_response(&url, Some(accept)).ok()?;
            if response.status != 200 {
                return None;
            }
            match stuff_oci::parse_image(&response.body).ok()? {
                stuff_oci::Image::Manifest(manifest) => Some(manifest.config.digest),
                stuff_oci::Image::Index(_) => None,
            }
        }),
    };
    let config_label = format!("oci config {}:{}", first.name, tag);
    let Some(config) = config else {
        checks.push(Check::fail(config_label, "manifest graph has no config blob"));
        return;
    };
    match crate::source::fetch_response(
        &format!("{base}/v2/{}/blobs/{config}", first.name),
        None,
    ) {
        Ok(response)
            if response.status == 200
                && stuff_oci::Digest::from_bytes(&response.body) == config =>
        {
            checks.push(Check::pass(config_label, format!("blob {config} hashes")));
        }
        Ok(response) => checks.push(Check::fail(
            config_label,
            format!("HTTP {} for {config}", response.status),
        )),
        Err(error) => checks.push(Check::fail(config_label, error)),
    }
}

fn report(checks: &[Check]) {
    for check in checks {
        let status = match check.state {
            CheckState::Pass => "ok",
            CheckState::Fail => "FAIL",
            CheckState::Skip => "skip",
        };
        println!("{status}: {} — {}", check.name, check.detail);
    }
}

const SAMPLE_CAP: u64 = 300 * 1024 * 1024;

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn sha512_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha512::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn file_sha256_hex(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut h = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn file_sha512_hex(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let mut h = sha2::Sha512::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

fn json_field<'a>(value: &'a deser_value::Value, key: &str) -> Option<&'a deser_value::Value> {
    value.as_map()?.get(key)
}

fn json_str<'a>(value: &'a deser_value::Value, key: &str) -> Option<&'a str> {
    json_field(value, key)?.as_str()
}

fn parse_json(text: &str, what: &str) -> Result<deser_value::Value, String> {
    deser_json::from_str(text).map_err(|e| format!("{what} is not valid JSON: {e}"))
}

fn first_index_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.to_string())
}

fn expand_dl(template: &str, name: &str, vers: &str, cksum: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let shard = cargo_shard(&lower)?;
    let prefix = shard.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    Some(
        template
            .replace("{crate}", name)
            .replace("{version}", vers)
            .replace("{prefix}", prefix)
            .replace("{lowerprefix}", &prefix.to_lowercase())
            .replace("{sha256-checksum}", cksum),
    )
}

fn sample_local(
    name: &str,
    catalog: &stuff_catalog::Catalog,
    site_dir: &std::path::Path,
    gen_dir: &std::path::Path,
) -> Check {
    let label = format!("{name} sample artifact");
    let first = &catalog.packages[0];
    match name {
        "cargo" => {
            let Some(shard) = cargo_shard(&first.name.to_lowercase()) else {
                return Check::fail(label, format!("{} is not an ASCII crate name", first.name));
            };
            let mut line = None;
            for view in ["registry", "crates-io"] {
                let path = gen_dir.join(view).join(&shard);
                if let Ok(text) = std::fs::read_to_string(&path)
                    && let Some(first_line) = first_index_line(&text) {
                        line = Some(first_line);
                        break;
                    }
            }
            let Some(line) = line else {
                return Check::fail(label, format!("no index entry for {}", first.name));
            };
            let entry = match parse_json(&line, "index entry") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let (Some(cksum), Some(vers)) = (json_str(&entry, "cksum"), json_str(&entry, "vers")) else {
                return Check::fail(label, "index entry has no cksum/vers".to_string());
            };
            let blob = site_dir.join("blobs").join(format!("{cksum}.crate"));
            match file_sha256_hex(&blob) {
                Ok(actual) if actual == cksum => {
                    Check::pass(label, format!("{} {vers} hash matches", first.name))
                }
                Ok(actual) => Check::fail(label, format!("{} {vers} hash mismatch: {actual}", first.name)),
                Err(e) => Check::fail(label, e),
            }
        }
        "npm" => {
            let text = match std::fs::read_to_string(gen_dir.join("manifest.json")) {
                Ok(t) => t,
                Err(e) => return Check::fail(label, format!("could not read manifest.json: {e}")),
            };
            let manifest = match parse_json(&text, "manifest.json") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let digest = manifest
                .as_map()
                .and_then(|m| m.get("artifacts"))
                .and_then(|v| v.as_seq())
                .and_then(|s| s.iter().next())
                .and_then(|a| json_field(a, "artifact"))
                .and_then(|a| json_str(a, "sha512"));
            let Some(digest) = digest else {
                return Check::fail(label, "manifest has no artifacts".to_string());
            };
            let tarball = site_dir.join("tarballs").join(format!("{digest}.tgz"));
            match file_sha512_hex(&tarball) {
                Ok(actual) if actual == digest => {
                    Check::pass(label, format!("tarballs/{digest}.tgz hash matches"))
                }
                Ok(actual) => Check::fail(label, format!("tarballs/{digest}.tgz hash mismatch: {actual}")),
                Err(e) => Check::fail(label, e),
            }
        }
        _ => {
            let text = match std::fs::read_to_string(gen_dir.join("manifest.json")) {
                Ok(t) => t,
                Err(e) => return Check::fail(label, format!("could not read manifest.json: {e}")),
            };
            let manifest = match parse_json(&text, "manifest.json") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let row = manifest
                .as_map()
                .and_then(|m| m.get("artifacts"))
                .and_then(|v| v.as_seq())
                .and_then(|s| s.iter().next());
            let (Some(filename), Some(sha)) = (
                row.and_then(|r| json_str(r, "filename")),
                row.and_then(|r| json_str(r, "sha256")),
            ) else {
                return Check::fail(label, "manifest has no artifacts".to_string());
            };
            let wheel = site_dir.join("wheels").join(filename);
            match file_sha256_hex(&wheel) {
                Ok(actual) if actual == sha => Check::pass(label, format!("wheels/{filename} hash matches")),
                Ok(actual) => Check::fail(label, format!("wheels/{filename} hash mismatch: {actual}")),
                Err(e) => Check::fail(label, e),
            }
        }
    }
}

fn sample_remote(name: &str, catalog: &stuff_catalog::Catalog, base: &str) -> Check {
    let label = format!("{name} sample artifact");
    let first = &catalog.packages[0];
    match name {
        "cargo" => {
            let config_url = format!("{base}/current/registry/config.json");
            let (status, body) = match crate::source::fetch_bytes(&config_url) {
                Ok(r) => r,
                Err(e) => return Check::fail(label, e),
            };
            if status != 200 {
                return Check::fail(label, format!("config.json HTTP {status}"));
            }
            let text = match String::from_utf8(body) {
                Ok(t) => t,
                Err(_) => return Check::fail(label, "config.json is not UTF-8".to_string()),
            };
            let config = match parse_json(&text, "config.json") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let Some(dl) = json_str(&config, "dl") else {
                return Check::fail(label, "config.json has no dl template".to_string());
            };
            let Some(shard) = cargo_shard(&first.name.to_lowercase()) else {
                return Check::fail(label, format!("{} is not an ASCII crate name", first.name));
            };
            let mut index_body = None;
            for view in ["registry", "crates-io"] {
                match crate::source::fetch_bytes(&format!("{base}/current/{view}/{shard}")) {
                    Ok((200, body)) => {
                        index_body = Some(body);
                        break;
                    }
                    Ok(_) => continue,
                    Err(e) => return Check::fail(label, e),
                }
            }
            let Some(index_body) = index_body else {
                return Check::fail(label, format!("no index file for {}", first.name));
            };
            let text = match String::from_utf8(index_body) {
                Ok(t) => t,
                Err(_) => return Check::fail(label, "index file is not UTF-8".to_string()),
            };
            let Some(line) = first_index_line(&text) else {
                return Check::fail(label, "index file is empty".to_string());
            };
            let entry = match parse_json(&line, "index entry") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let (Some(entry_name), Some(vers), Some(cksum)) =
                (json_str(&entry, "name"), json_str(&entry, "vers"), json_str(&entry, "cksum"))
            else {
                return Check::fail(label, "index entry has no name/vers/cksum".to_string());
            };
            let Some(url) = expand_dl(dl, entry_name, vers, cksum) else {
                return Check::fail(label, format!("{entry_name} is not an ASCII crate name"));
            };
            match crate::source::fetch_bytes_capped(&url, SAMPLE_CAP) {
                Ok((200, bytes)) if sha256_hex(&bytes) == cksum => {
                    Check::pass(label, format!("{entry_name} {vers} hash matches"))
                }
                Ok((200, _)) => Check::fail(label, format!("{entry_name} {vers} hash mismatch")),
                Ok((status, _)) => Check::fail(label, format!("artifact HTTP {status}")),
                Err(e) => Check::fail(label, e),
            }
        }
        "npm" => {
            let route = first.name.replace('/', "%2f");
            let url = format!("{base}/current/{route}");
            let (status, body) = match crate::source::fetch_bytes(&url) {
                Ok(r) => r,
                Err(e) => return Check::fail(label, e),
            };
            if status != 200 {
                return Check::fail(label, format!("packument HTTP {status}"));
            }
            let text = match String::from_utf8(body) {
                Ok(t) => t,
                Err(_) => return Check::fail(label, "packument is not UTF-8".to_string()),
            };
            let packument = match parse_json(&text, "packument") {
                Ok(v) => v,
                Err(e) => return Check::fail(label, e),
            };
            let version_doc = packument
                .as_map()
                .and_then(|m| m.get("versions"))
                .and_then(|v| v.as_map())
                .and_then(|m| m.get(&first.latest))
                .cloned();
            let Some(version_doc) = version_doc else {
                return Check::fail(label, format!("packument has no {}", first.latest));
            };
            let (Some(tarball), Some(integrity)) = (
                json_field(&version_doc, "dist").and_then(|d| json_str(d, "tarball")),
                json_field(&version_doc, "dist").and_then(|d| json_str(d, "integrity")),
            ) else {
                return Check::fail(label, "version has no dist tarball/integrity".to_string());
            };
            let expected = integrity.strip_prefix("sha512-").unwrap_or(integrity);
            match crate::source::fetch_bytes_capped(tarball, SAMPLE_CAP) {
                Ok((200, bytes)) => {
                    use base64::Engine;
                    let actual = base64::engine::general_purpose::STANDARD.encode(
                        hex_to_bytes(&sha512_hex(&bytes)).unwrap_or_default(),
                    );
                    if actual == expected {
                        Check::pass(label, format!("{} {} hash matches", first.name, first.latest))
                    } else {
                        Check::fail(label, format!("{} {} hash mismatch", first.name, first.latest))
                    }
                }
                Ok((status, _)) => Check::fail(label, format!("tarball HTTP {status}")),
                Err(e) => Check::fail(label, e),
            }
        }
        _ => {
            let project = first.name.to_lowercase();
            let url = format!("{base}/current/simple/{project}/");
            let (status, body) = match crate::source::fetch_bytes(&url) {
                Ok(r) => r,
                Err(e) => return Check::fail(label, e),
            };
            if status != 200 {
                return Check::fail(label, format!("project page HTTP {status}"));
            }
            let text = match String::from_utf8(body) {
                Ok(t) => t,
                Err(_) => return Check::fail(label, "project page is not UTF-8".to_string()),
            };
            let Some((filename, hash)) = first_wheel_href(&text) else {
                return Check::fail(label, "project page has no wheel links".to_string());
            };
            let wheel_url = format!("{base}/current/wheels/{filename}");
            match crate::source::fetch_bytes_capped(&wheel_url, SAMPLE_CAP) {
                Ok((200, bytes)) if sha256_hex(&bytes) == hash => {
                    Check::pass(label, format!("wheels/{filename} hash matches"))
                }
                Ok((200, _)) => Check::fail(label, format!("wheels/{filename} hash mismatch")),
                Ok((status, _)) => Check::fail(label, format!("wheel HTTP {status}")),
                Err(e) => Check::fail(label, e),
            }
        }
    }
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        out.push(u8::from_str_radix(&hex[i..i + 2], 16).ok()?);
    }
    Some(out)
}

fn first_wheel_href(page: &str) -> Option<(String, String)> {
    let marker = "../../wheels/";
    let start = page.find(marker)? + marker.len();
    let rest = &page[start..];
    let end = rest.find('"')?;
    let href = &rest[..end];
    let (filename, hash) = href.split_once("#sha256=")?;
    if filename.is_empty() || filename.contains('/') || hash.len() != 64 {
        return None;
    }
    Some((filename.to_string(), hash.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Command, Parsed};

    fn doctor_parsed() -> Parsed {
        Parsed {
            command: Command::Doctor,
            registry: None,
            versions: false,
            json: false,
            sample_artifacts: false,
            cargo_base: None,
            npm_base: None,
            pypi_base: None,
            oci_base: None,
            cargo_dir: None,
            npm_dir: None,
            pypi_dir: None,
            oci_dir: None,
            output: None,
            expected_generation: None,
            positionals: Vec::new(),
        }
    }

    #[test]
    fn usage_errors_exit_two() {
        let mut parsed = doctor_parsed();
        parsed.cargo_base = Some("http://example.com".to_string());
        let err = run_doctor(&parsed).unwrap_err();
        assert!(matches!(err, Error::Usage(_)), "{err}");
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn non_ascii_catalog_name_fails_the_index_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("catalog.json"), "{}").unwrap();
        let local = LocalSite::resolve(dir.path()).unwrap();
        let catalog = stuff_catalog::cargo_catalog(
            "gen1",
            vec![stuff_catalog::CargoInput {
                name: "café".to_string(),
                version: "1.0.0".to_string(),
                yanked: false,
                source: stuff_catalog::CargoSource::Native,
                dependencies: stuff_catalog::Dependencies::default(),
            }],
        )
        .unwrap();

        let mut checks = Vec::new();
        run_local_checks(&mut checks, "cargo", &catalog, &local, "https://example.test", false);
        let index = checks.iter().find(|c| c.name == "cargo index").unwrap();
        assert_eq!(index.state, CheckState::Fail);
        assert!(index.detail.contains("ASCII"), "{}", index.detail);
    }

    #[test]
    fn expands_dl_templates() {
        let url = expand_dl(
            "https://dl.example/{crate}/{version}/{prefix}/{lowerprefix}/{sha256-checksum}",
            "Foo-Bar",
            "1.0.0",
            "ab12",
        )
        .unwrap();
        assert!(url.contains("Foo-Bar/1.0.0/fo/o-/fo/o-/ab12"), "{url}");
    }

    #[test]
    fn extracts_wheel_hrefs() {
        let page = "<a href=\"../../wheels/pkg-1.0-py3-none-any.whl#sha256=abc123\">x</a>";
        let _ = first_wheel_href(page);
        let page = format!(
            "<a href=\"../../wheels/pkg-1.0-py3-none-any.whl#sha256={}\">x</a>",
            "0".repeat(64)
        );
        let (file, hash) = first_wheel_href(&page).unwrap();
        assert_eq!(file, "pkg-1.0-py3-none-any.whl");
        assert_eq!(hash, "0".repeat(64));
    }

    fn cargo_fixture(dir: &std::path::Path, blob_bytes: &[u8]) -> (stuff_catalog::Catalog, std::path::PathBuf, std::path::PathBuf) {
        use sha2::Digest;
        let mut h = sha2::Sha256::new();
        h.update(blob_bytes);
        let cksum = format!("{:x}", h.finalize());
        let site = dir.to_path_buf();
        let gen_dir = site.join("generations").join("gen1");
        let shard = cargo_shard("foo").unwrap();
        let index_path = gen_dir.join("registry").join(&shard);
        std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(site.join("blobs")).unwrap();
        let line = format!("{{\"name\":\"foo\",\"vers\":\"1.0.0\",\"cksum\":\"{cksum}\"}}\n");
        std::fs::write(&index_path, line).unwrap();
        std::fs::write(site.join("blobs").join(format!("{cksum}.crate")), blob_bytes).unwrap();
        std::os::unix::fs::symlink("generations/gen1", site.join("current")).unwrap();
        let catalog = stuff_catalog::cargo_catalog(
            "gen1",
            vec![stuff_catalog::CargoInput {
                name: "foo".to_string(),
                version: "1.0.0".to_string(),
                yanked: false,
                source: stuff_catalog::CargoSource::Native,
                dependencies: stuff_catalog::Dependencies::default(),
            }],
        )
        .unwrap();
        std::fs::write(gen_dir.join("catalog.json"), stuff_catalog::render_catalog_json(&catalog)).unwrap();
        std::fs::write(
            gen_dir.join("llms.txt"),
            stuff_catalog::render_llms_txt(&catalog, DEFAULT_CARGO_ORIGIN),
        )
        .unwrap();
        (catalog, site, gen_dir)
    }

    #[test]
    fn resolves_site_generation_and_catalog_to_one_pair() {
        let dir = tempfile::tempdir().unwrap();
        let (_, site, gen_dir) = cargo_fixture(dir.path(), b"crate-bytes");
        let catalog_file = gen_dir.join("catalog.json");
        let from_site = LocalSite::resolve(&site).unwrap();
        let from_generation = LocalSite::resolve(&gen_dir).unwrap();
        let from_catalog = LocalSite::resolve(&catalog_file).unwrap();
        assert_eq!(from_site, from_generation);
        assert_eq!(from_site, from_catalog);
        assert_eq!(from_site.site_root, site);
        assert_eq!(from_site.generation_dir, gen_dir);
    }

    #[test]
    fn doctor_accepts_site_generation_and_catalog_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let (_, site, gen_dir) = cargo_fixture(dir.path(), b"crate-bytes");
        let catalog_file = gen_dir.join("catalog.json");
        for input in [&site, &gen_dir, &catalog_file] {
            let mut parsed = doctor_parsed();
            parsed.cargo_dir = Some(input.to_string_lossy().to_string());
            parsed.sample_artifacts = true;
            assert!(run_doctor(&parsed).unwrap(), "doctor failed for {}", input.display());
        }
    }

    #[test]
    fn doctor_checks_oci_manifests_and_config_blobs() {
        use stuff_oci::{Descriptor, Digest, RepositoryName, Tag};
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().to_path_buf();
        let gen_dir = site.join("generations").join("gen1");
        std::fs::create_dir_all(gen_dir.join("repositories/example/app")).unwrap();
        std::fs::create_dir_all(site.join("oci/manifests/sha256")).unwrap();
        std::fs::create_dir_all(site.join("blobs/sha256")).unwrap();
        let layout = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/oci/layout");
        let entries = [
            ("04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388", "application/vnd.oci.image.index.v1+json", 491u64, true),
            ("576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4", "application/vnd.oci.image.manifest.v1+json", 401, true),
            ("d4ae9ea60e8f1f455de9959f23d711014d5f97e9d85ce82a6ecc769a61d1c65c", "application/vnd.oci.image.manifest.v1+json", 401, true),
            ("3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25", "application/vnd.oci.image.manifest.v1+json", 671, true),
        ];
        let mut view = stuff_oci::RepositoryView::new(RepositoryName::parse("example/app").unwrap());
        let mut index = None;
        for (hex, media_type, size, manifest) in entries {
            let bytes = std::fs::read(layout.join("blobs/sha256").join(hex)).unwrap();
            if manifest {
                std::fs::write(site.join("oci/manifests/sha256").join(hex), &bytes).unwrap();
            }
            let descriptor = Descriptor {
                media_type: media_type.to_string(),
                digest: Digest::parse(&format!("sha256:{hex}")).unwrap(),
                size,
                artifact_type: None,
                annotations: std::collections::BTreeMap::new(),
                platform: None,
                urls: Vec::new(),
            };
            view.insert_manifest(descriptor.clone());
            if index.is_none() && hex.starts_with("04237663") {
                index = Some(descriptor);
            }
        }
        view.tag(Tag::parse("stable").unwrap(), index.unwrap());
        for hex in [
            "120ab4e8b5d4dfbdda6b6ccd16c9e4320530c08b35463b414e411fb76d9e30c9",
            "1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55",
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
            "6615b2d5776183ba25e1bfa8e789346fe3a65066e808ac5b6beb5710bb94d10e",
            "882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f",
            "bf2585f7b79b5ba8a5d7f256f7c2ce5a537ebc1c0d9e043ca90c461f2183f718",
        ] {
            let bytes = std::fs::read(layout.join("blobs/sha256").join(hex)).unwrap();
            std::fs::write(site.join("blobs/sha256").join(hex), bytes).unwrap();
        }
        std::fs::write(
            gen_dir.join("repositories/example/app/repository.json"),
            stuff_oci::render_repository_json(&view),
        )
        .unwrap();
        let catalog = stuff_catalog::oci_catalog(
            "gen1",
            vec![stuff_catalog::OciInput {
                name: "example/app".to_string(),
                tags: vec!["stable".to_string()],
            }],
        )
        .unwrap();
        std::fs::write(gen_dir.join("catalog.json"), stuff_catalog::render_catalog_json(&catalog)).unwrap();
        std::fs::write(gen_dir.join("index.html"), stuff_catalog::render_html(&catalog)).unwrap();
        std::fs::write(
            gen_dir.join("llms.txt"),
            stuff_catalog::render_llms_txt(&catalog, DEFAULT_OCI_ORIGIN),
        )
        .unwrap();
        std::os::unix::fs::symlink("generations/gen1", site.join("current")).unwrap();

        let mut parsed = doctor_parsed();
        parsed.oci_dir = Some(site.to_string_lossy().to_string());
        let checks = doctor_checks(&parsed).unwrap();
        for name in ["oci manifest example/app:stable", "oci config example/app:stable"] {
            let check = checks.iter().find(|check| check.name == name).unwrap();
            assert_eq!(check.state, CheckState::Pass, "{}: {}", check.name, check.detail);
        }
        assert!(run_doctor(&parsed).unwrap());
    }

    #[test]
    fn local_input_skips_omitted_registries_and_refuses_expected_generation() {
        let dir = tempfile::tempdir().unwrap();
        let (_, site, _) = cargo_fixture(dir.path(), b"crate-bytes");
        let mut parsed = doctor_parsed();
        parsed.cargo_dir = Some(site.to_string_lossy().to_string());
        let checks = doctor_checks(&parsed).unwrap();
        let skipped: Vec<&str> = checks
            .iter()
            .filter(|check| check.state == CheckState::Skip)
            .map(|check| check.name.as_str())
            .collect();
        assert_eq!(skipped, vec!["npm registry", "pypi registry", "oci registry"]);
        assert!(checks.iter().all(|check| check.state != CheckState::Fail));

        parsed.expected_generation = Some("gen1".to_string());
        let err = run_doctor(&parsed).unwrap_err();
        assert!(matches!(err, Error::Usage(_)), "{err}");

        parsed.registry = Some(crate::cli::RegistryFilter::Cargo);
        assert!(run_doctor(&parsed).unwrap());
    }

    #[test]
    fn local_cargo_sample_matches() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, site, gen_dir) = cargo_fixture(dir.path(), b"crate-bytes");
        let check = sample_local("cargo", &catalog, &site, &gen_dir);
        assert_eq!(check.state, CheckState::Pass, "{}", check.detail);
    }

    #[test]
    fn local_cargo_sample_fails_on_tampered_blob() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, site, gen_dir) = cargo_fixture(dir.path(), b"crate-bytes");
        let blob = std::fs::read_dir(site.join("blobs")).unwrap().next().unwrap().unwrap().path();
        std::fs::write(&blob, b"tampered").unwrap();
        let check = sample_local("cargo", &catalog, &site, &gen_dir);
        assert_ne!(check.state, CheckState::Pass, "tampered blob must fail");
    }

    #[test]
    fn local_llms_check_uses_the_configured_origin() {
        let dir = tempfile::tempdir().unwrap();
        let (catalog, site, gen_dir) = cargo_fixture(dir.path(), b"crate-bytes");
        let origin = "https://cargo.example.test/mirror";
        std::fs::write(
            gen_dir.join("llms.txt"),
            stuff_catalog::render_llms_txt(&catalog, origin),
        )
        .unwrap();
        let local = LocalSite::resolve(&site).unwrap();

        let mut matching = Vec::new();
        run_local_checks(&mut matching, "cargo", &catalog, &local, origin, false);
        let llms = matching.iter().find(|c| c.name == "cargo llms.txt").unwrap();
        assert_eq!(llms.state, CheckState::Pass, "{}", llms.detail);

        let mut mismatched = Vec::new();
        run_local_checks(
            &mut mismatched,
            "cargo",
            &catalog,
            &local,
            "https://other.example.test",
            false,
        );
        let llms = mismatched.iter().find(|c| c.name == "cargo llms.txt").unwrap();
        assert_eq!(llms.state, CheckState::Fail, "doctor must detect an origin mismatch");
        assert!(llms.detail.contains("does not match catalog"), "{}", llms.detail);
    }

    #[test]
    fn local_npm_and_pypi_samples_match() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().to_path_buf();
        let gen_dir = site.join("generations").join("g1");
        std::fs::create_dir_all(&gen_dir).unwrap();
        std::fs::create_dir_all(site.join("tarballs")).unwrap();
        std::fs::create_dir_all(site.join("wheels")).unwrap();
        std::os::unix::fs::symlink("generations/g1", site.join("current")).unwrap();
        let tgz = b"tarball-bytes";
        let digest = sha512_hex(tgz);
        std::fs::write(site.join("tarballs").join(format!("{digest}.tgz")), tgz).unwrap();
        std::fs::write(
            gen_dir.join("manifest.json"),
            format!("{{\"artifacts\":[{{\"artifact\":{{\"sha512\":\"{digest}\"}}}}]}}"),
        )
        .unwrap();
        let wheel = b"wheel-bytes";
        let whash = sha256_hex(wheel);
        std::fs::write(site.join("wheels").join("pkg-1.0-py3-none-any.whl"), wheel).unwrap();
        let npm_catalog = stuff_catalog::npm_catalog(
            "g1",
            vec![stuff_catalog::NpmInput { name: "a".to_string(), version: "1.0.0".to_string(), dependencies: stuff_catalog::Dependencies::default() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let check = sample_local("npm", &npm_catalog, &site, &gen_dir);
        assert_eq!(check.state, CheckState::Pass, "{}", check.detail);
        let pypi_catalog = stuff_catalog::pypi_catalog(
            "g1",
            vec![stuff_catalog::PypiInput { name: "pkg".to_string(), version: "1.0".to_string(), yanked: false, dependencies: stuff_catalog::Dependencies::default() }],
        )
        .unwrap();
        std::fs::write(
            gen_dir.join("manifest.json"),
            format!("{{\"artifacts\":[{{\"filename\":\"pkg-1.0-py3-none-any.whl\",\"sha256\":\"{whash}\"}}]}}"),
        )
        .unwrap();
        let check = sample_local("pypi", &pypi_catalog, &site, &gen_dir);
        assert_eq!(check.state, CheckState::Pass, "{}", check.detail);
    }
}
