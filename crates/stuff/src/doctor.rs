#![forbid(unsafe_code)]

use crate::source::{fetch_bytes, load_local_catalog, DEFAULT_CARGO_BASE, DEFAULT_NPM_BASE, DEFAULT_PYPI_BASE};
use std::path::PathBuf;

pub struct RegistrySources {
    pub cargo_base: String,
    pub npm_base: String,
    pub pypi_base: String,
    pub cargo_dir: Option<String>,
    pub npm_dir: Option<String>,
    pub pypi_dir: Option<String>,
}

impl RegistrySources {
    pub fn from_parsed(parsed: &crate::cli::Parsed) -> Self {
        Self {
            cargo_base: parsed
                .cargo_base
                .clone()
                .or_else(|| std::env::var("STUFF_CARGO_BASE").ok())
                .unwrap_or_else(|| DEFAULT_CARGO_BASE.to_string()),
            npm_base: parsed
                .npm_base
                .clone()
                .or_else(|| std::env::var("STUFF_NPM_BASE").ok())
                .unwrap_or_else(|| DEFAULT_NPM_BASE.to_string()),
            pypi_base: parsed
                .pypi_base
                .clone()
                .or_else(|| std::env::var("STUFF_PYPI_BASE").ok())
                .unwrap_or_else(|| DEFAULT_PYPI_BASE.to_string()),
            cargo_dir: parsed.cargo_dir.clone(),
            npm_dir: parsed.npm_dir.clone(),
            pypi_dir: parsed.pypi_dir.clone(),
        }
    }

    pub fn selected(&self, filter: Option<&str>) -> Vec<(&'static str, bool)> {
        let all = [("cargo", self.cargo_dir.is_some()), ("npm", self.npm_dir.is_some()), ("pypi", self.pypi_dir.is_some())];
        match filter {
            Some(f) => all.into_iter().filter(|(name, _)| *name == f).collect(),
            None => all.to_vec(),
        }
    }
}

pub fn load_catalogs(
    sources: &RegistrySources,
    filter: Option<&str>,
) -> Result<Vec<stuff_catalog::Catalog>, String> {
    let any_local =
        sources.cargo_dir.is_some() || sources.npm_dir.is_some() || sources.pypi_dir.is_some();
    let mut out = Vec::new();
    for (name, is_local) in sources.selected(filter) {
        if filter.is_none() && any_local && !is_local {
            continue;
        }
        let catalog = match name {
            "cargo" => {
                if is_local {
                    load_local_catalog(sources.cargo_dir.as_deref().unwrap_or(""))?
                } else {
                    crate::source::fetch_catalog(&sources.cargo_base)?
                }
            }
            "npm" => {
                if is_local {
                    load_local_catalog(sources.npm_dir.as_deref().unwrap_or(""))?
                } else {
                    crate::source::fetch_catalog(&sources.npm_base)?
                }
            }
            "pypi" => {
                if is_local {
                    load_local_catalog(sources.pypi_dir.as_deref().unwrap_or(""))?
                } else {
                    crate::source::fetch_catalog(&sources.pypi_base)?
                }
            }
            _ => unreachable!(),
        };
        let expected = match name {
            "cargo" => stuff_catalog::Registry::Cargo,
            "npm" => stuff_catalog::Registry::Npm,
            _ => stuff_catalog::Registry::Pypi,
        };
        if catalog.registry != expected {
            return Err(format!("{name} catalog has unexpected registry {}", catalog.registry.as_str()));
        }
        out.push(catalog);
    }
    out.sort_by(|a: &stuff_catalog::Catalog, b: &stuff_catalog::Catalog| {
        a.registry.as_str().cmp(b.registry.as_str())
    });
    Ok(out)
}

fn cargo_shard(lower_name: &str) -> String {
    let chars: Vec<char> = lower_name.chars().collect();
    match chars.len() {
        1 => format!("1/{}", lower_name),
        2 => format!("2/{}", lower_name),
        3 => format!("3/{}/{}", chars[0], lower_name),
        _ => format!("{}/{}/{}", &lower_name[0..2], &lower_name[2..4], lower_name),
    }
}

struct Check {
    name: String,
    ok: bool,
    detail: String,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), ok: true, detail: detail.into() }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), ok: false, detail: detail.into() }
    }
}

pub fn run_doctor(parsed: &crate::cli::Parsed) -> Result<bool, String> {
    let sources = RegistrySources::from_parsed(parsed);
    let filter = parsed.registry_filter();
    let mut checks: Vec<Check> = Vec::new();
    let catalogs = match load_catalogs(&sources, filter) {
        Ok(c) => c,
        Err(e) => {
            checks.push(Check::fail("catalog load", e));
            report(&checks);
            return Ok(false);
        }
    };
    if catalogs.is_empty() {
        return Err("no registries selected".to_string());
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
        let is_local = match name {
            "cargo" => sources.cargo_dir.is_some(),
            "npm" => sources.npm_dir.is_some(),
            _ => sources.pypi_dir.is_some(),
        };
        if is_local {
            run_local_checks(&mut checks, name, catalog, &sources);
        } else {
            run_remote_checks(&mut checks, name, catalog, &sources);
        }
    }
    let ok = checks.iter().all(|c| c.ok);
    report(&checks);
    Ok(ok)
}

fn site_dir_for(name: &str, sources: &RegistrySources) -> PathBuf {
    let raw = match name {
        "cargo" => sources.cargo_dir.as_deref().unwrap_or(""),
        "npm" => sources.npm_dir.as_deref().unwrap_or(""),
        _ => sources.pypi_dir.as_deref().unwrap_or(""),
    };
    PathBuf::from(raw)
}

fn generation_dir_for(site_dir: &std::path::Path, catalog: &stuff_catalog::Catalog) -> Option<PathBuf> {
    let direct = site_dir.join("catalog.json");
    if site_dir.join("current").symlink_metadata().is_err() && direct.is_file() {
        return site_dir.parent().map(|p| p.to_path_buf()).or(Some(site_dir.to_path_buf()));
    }
    Some(site_dir.join("generations").join(&catalog.generation))
}

fn run_local_checks(
    checks: &mut Vec<Check>,
    name: &str,
    catalog: &stuff_catalog::Catalog,
    sources: &RegistrySources,
) {
    let site_dir = site_dir_for(name, sources);
    let gen_dir = match generation_dir_for(&site_dir, catalog) {
        Some(d) => d,
        None => {
            checks.push(Check::fail(format!("{name} generation dir"), "could not resolve generation directory"));
            return;
        }
    };
    if catalog.packages.is_empty() {
        checks.push(Check::pass(format!("{name} endpoints"), "empty catalog: nothing further to check"));
        return;
    }
    let first = &catalog.packages[0];
    match name {
        "cargo" => {
            let shard = cargo_shard(&first.name.to_lowercase());
            let candidates = [gen_dir.join("registry").join(&shard), gen_dir.join("crates-io").join(&shard)];
            if candidates.iter().any(|p| p.is_file()) {
                checks.push(Check::pass(format!("{name} index"), format!("{} present", first.name)));
            } else {
                checks.push(Check::fail(format!("{name} index"), format!("{} missing ({}:{})", first.name, catalog.generation, shard)));
            }
        }
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
}

fn run_remote_checks(
    checks: &mut Vec<Check>,
    name: &str,
    catalog: &stuff_catalog::Catalog,
    sources: &RegistrySources,
) {
    let base = match name {
        "cargo" => sources.cargo_base.trim_end_matches('/').to_string(),
        "npm" => sources.npm_base.trim_end_matches('/').to_string(),
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
    let first = &catalog.packages[0];
    match name {
        "cargo" => {
            let shard = cargo_shard(&first.name.to_lowercase());
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
        "npm" => {
            let route = first.name.replace('/', "%2f");
            let url = format!("{base}/current/{route}");
            match fetch_bytes(&url) {
                Ok((200, _)) => checks.push(Check::pass(format!("{name} packument"), format!("{} reachable", first.name))),
                Ok((status, _)) => checks.push(Check::fail(format!("{name} packument"), format!("HTTP {status}"))),
                Err(e) => checks.push(Check::fail(format!("{name} packument"), e)),
            }
        }
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
}

fn report(checks: &[Check]) {
    for check in checks {
        let status = if check.ok { "ok" } else { "FAIL" };
        println!("{status}: {} — {}", check.name, check.detail);
    }
}
