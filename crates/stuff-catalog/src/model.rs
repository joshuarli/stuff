#![forbid(unsafe_code)]

use crate::version::{compare_npm_version, compare_semver, is_stable_semver, parse_pep440};
use std::collections::BTreeMap;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Registry {
    Cargo,
    Npm,
    Pypi,
}

impl Registry {
    pub fn as_str(&self) -> &'static str {
        match self {
            Registry::Cargo => "cargo",
            Registry::Npm => "npm",
            Registry::Pypi => "pypi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cargo" => Some(Registry::Cargo),
            "npm" => Some(Registry::Npm),
            "pypi" => Some(Registry::Pypi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CargoSource {
    Native,
    Upstream,
}

impl CargoSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            CargoSource::Native => "native",
            CargoSource::Upstream => "upstream",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "native" => Some(CargoSource::Native),
            "upstream" => Some(CargoSource::Upstream),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionEntry {
    pub version: String,
    pub yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub source: Option<String>,
    pub latest: String,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub schema_version: u32,
    pub registry: Registry,
    pub generation: String,
    pub packages: Vec<Package>,
}

impl Catalog {
    pub fn package_count(&self) -> usize {
        self.packages.len()
    }

    pub fn version_count(&self) -> usize {
        self.packages.iter().map(|p| p.versions.len()).sum()
    }

    pub fn find(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn render_catalog_json(catalog: &Catalog) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"schema_version\": {},\n", SCHEMA_VERSION));
    out.push_str(&format!("  \"registry\": {},\n", escape_json(catalog.registry.as_str())));
    out.push_str(&format!("  \"generation\": {},\n", escape_json(&catalog.generation)));
    out.push_str("  \"packages\": [");
    if catalog.packages.is_empty() {
        out.push_str("]\n}\n");
        return out;
    }
    out.push('\n');
    for (i, package) in catalog.packages.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"name\": {},\n", escape_json(&package.name)));
        match &package.source {
            Some(s) => out.push_str(&format!("      \"source\": {},\n", escape_json(s))),
            None => out.push_str("      \"source\": null,\n"),
        }
        out.push_str(&format!("      \"latest\": {},\n", escape_json(&package.latest)));
        out.push_str("      \"versions\": [");
        if package.versions.is_empty() {
            out.push_str("]\n");
        } else {
            out.push('\n');
            for (j, v) in package.versions.iter().enumerate() {
                let yanked = if v.yanked { "true" } else { "false" };
                out.push_str(&format!(
                    "        {{\"version\": {}, \"yanked\": {yanked}}}",
                    escape_json(&v.version)
                ));
                if j + 1 < package.versions.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str("      ]\n");
        }
        out.push_str("    }");
        if i + 1 < catalog.packages.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn value_as_str<'a>(v: &'a deser_value::Value) -> Option<&'a str> {
    v.as_str()
}

fn parse_version_entry(v: &deser_value::Value, context: &str) -> Result<VersionEntry, String> {
    let map = v.as_map().ok_or_else(|| format!("{context} must be an object"))?;
    if map.len() != 2 {
        return Err(format!("{context} has unknown or missing fields"));
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let version = map.get("version").unwrap_or(&null);
    let yanked = map.get("yanked").unwrap_or(&null);
    let version = value_as_str(version).ok_or_else(|| format!("{context}.version must be a string"))?;
    if version.is_empty() {
        return Err(format!("{context}.version must not be empty"));
    }
    let yanked = yanked.as_bool().ok_or_else(|| format!("{context}.yanked must be a boolean"))?;
    Ok(VersionEntry { version: version.to_string(), yanked })
}

fn parse_package(v: &deser_value::Value, registry: Registry) -> Result<Package, String> {
    let map = v.as_map().ok_or_else(|| "package must be an object".to_string())?;
    if map.len() != 4 {
        return Err("package has unknown or missing fields".to_string());
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    for key in ["name", "source", "latest", "versions"] {
        if map.get(key).is_none() {
            return Err("package has unknown or missing fields".to_string());
        }
    }
    let name = value_as_str(map.get("name").unwrap_or(&null))
        .ok_or_else(|| "package.name must be a string".to_string())?;
    if name.is_empty() || name.contains('\0') {
        return Err("package.name must not be empty".to_string());
    }
    let source = match map.get("source").unwrap_or(&null) {
        s if matches!(&**s, deser_value::Kind::Null) => None,
        s => Some(
            value_as_str(s)
                .ok_or_else(|| "package.source must be a string or null".to_string())?
                .to_string(),
        ),
    };
    if registry != Registry::Cargo && source.is_some() {
        return Err("only cargo packages carry a source".to_string());
    }
    if let Some(ref s) = source
        && CargoSource::parse(s).is_none() {
            return Err("cargo package source must be native or upstream".to_string());
        }
    let latest = value_as_str(map.get("latest").unwrap_or(&null))
        .ok_or_else(|| "package.latest must be a string".to_string())?;
    if latest.is_empty() {
        return Err("package.latest must not be empty".to_string());
    }
    let versions_val = map.get("versions").unwrap_or(&null);
    let seq = versions_val.as_seq().ok_or_else(|| "package.versions must be an array".to_string())?;
    if seq.is_empty() {
        return Err("package.versions must not be empty".to_string());
    }
    let mut versions = Vec::with_capacity(seq.len());
    for (i, item) in seq.iter().enumerate() {
        versions.push(parse_version_entry(item, &format!("package {name} versions[{i}]"))?);
    }
    let mut seen = std::collections::BTreeSet::new();
    for v in &versions {
        if !seen.insert(v.version.clone()) {
            return Err(format!("package {name} has a duplicate version {}", v.version));
        }
    }
    if !versions.iter().any(|v| v.version == latest) {
        return Err(format!("package {name} latest {latest} is not in versions"));
    }
    Ok(Package { name: name.to_string(), source, latest: latest.to_string(), versions })
}

pub fn parse_catalog_json(text: &str) -> Result<Catalog, String> {
    let value: deser_value::Value =
        deser_json::from_str(text).map_err(|e| format!("catalog is not valid JSON: {e}"))?;
    let map = value.as_map().ok_or_else(|| "catalog must be an object".to_string())?;
    if map.len() != 4 {
        return Err("catalog has unknown or missing fields".to_string());
    }
    for key in ["schema_version", "registry", "generation", "packages"] {
        if map.get(key).is_none() {
            return Err("catalog has unknown or missing fields".to_string());
        }
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let schema = map.get("schema_version").unwrap_or(&null).as_u64().unwrap_or(0);
    if schema != u64::from(SCHEMA_VERSION) {
        return Err(format!("unsupported catalog schema_version: {schema}"));
    }
    let registry_text = map
        .get("registry")
        .unwrap_or(&null)
        .as_str()
        .ok_or_else(|| "catalog.registry must be a string".to_string())?;
    let registry = Registry::parse(registry_text)
        .ok_or_else(|| format!("unsupported catalog registry: {registry_text}"))?;
    let generation = map
        .get("generation")
        .unwrap_or(&null)
        .as_str()
        .ok_or_else(|| "catalog.generation must be a string".to_string())?;
    if generation.is_empty() {
        return Err("catalog.generation must not be empty".to_string());
    }
    let packages_val = map.get("packages").unwrap_or(&null);
    let seq = packages_val.as_seq().ok_or_else(|| "catalog.packages must be an array".to_string())?;
    let mut packages = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        packages.push(parse_package(item, registry)?);
    }
    let mut names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    names.sort();
    let mut dedup = names.clone();
    dedup.dedup();
    if dedup.len() != names.len() {
        return Err("catalog contains a duplicate package name".to_string());
    }
    let mut sorted = packages;
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry, generation: generation.to_string(), packages: sorted })
}

fn latest_semver(versions: &[VersionEntry]) -> String {
    let pool: Vec<&VersionEntry> = if versions.iter().any(|v| !v.yanked) {
        versions.iter().filter(|v| !v.yanked).collect()
    } else {
        versions.iter().collect()
    };
    let stable: Vec<&&VersionEntry> = pool.iter().filter(|v| is_stable_semver(&v.version)).collect();
    let ranked: Vec<&&VersionEntry> = if stable.is_empty() { pool.iter().collect() } else { stable };
    ranked
        .into_iter()
        .max_by(|a, b| compare_semver(&a.version, &b.version))
        .map(|v| v.version.clone())
        .unwrap_or_else(|| versions.last().map(|v| v.version.clone()).unwrap_or_default())
}

fn sort_semver_versions(versions: &mut Vec<VersionEntry>) {
    versions.sort_by(|a, b| compare_semver(&a.version, &b.version));
}

#[derive(Debug, Clone)]
pub struct CargoInput {
    pub name: String,
    pub version: String,
    pub yanked: bool,
    pub source: CargoSource,
}

pub fn cargo_catalog(generation: &str, inputs: Vec<CargoInput>) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: BTreeMap<String, Vec<VersionEntry>> = BTreeMap::new();
    let mut sources: BTreeMap<String, CargoSource> = BTreeMap::new();
    for input in inputs {
        if input.name.is_empty() || input.version.is_empty() {
            return Err("cargo package name and version must not be empty".to_string());
        }
        if crate::version::Semver::parse(&input.version).is_none() {
            return Err(format!("invalid cargo version {} for {}", input.version, input.name));
        }
        match sources.get(&input.name) {
            None => {
                sources.insert(input.name.clone(), input.source);
            }
            Some(existing) if *existing != input.source => {
                if input.source == CargoSource::Native {
                    sources.insert(input.name.clone(), CargoSource::Native);
                }
            }
            _ => {}
        }
        grouped.entry(input.name).or_default().push(VersionEntry { version: input.version, yanked: input.yanked });
    }
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, mut versions) in grouped {
        let mut seen = std::collections::BTreeSet::new();
        for v in &versions {
            if !seen.insert(v.version.clone()) {
                return Err(format!("cargo package {name} has a duplicate version {}", v.version));
            }
        }
        sort_semver_versions(&mut versions);
        let latest = latest_semver(&versions);
        let source = sources.get(&name).copied().unwrap_or(CargoSource::Native);
        packages.push(Package { name, source: Some(source.as_str().to_string()), latest, versions });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Cargo, generation: generation.to_string(), packages })
}

#[derive(Debug, Clone)]
pub struct NpmInput {
    pub name: String,
    pub version: String,
}

pub fn npm_catalog(
    generation: &str,
    inputs: Vec<NpmInput>,
    dist_latest: &BTreeMap<String, String>,
) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: BTreeMap<String, Vec<VersionEntry>> = BTreeMap::new();
    for input in inputs {
        if input.name.is_empty() || input.version.is_empty() {
            return Err("npm package name and version must not be empty".to_string());
        }
        if crate::version::Semver::parse(&input.version).is_none() {
            return Err(format!("invalid npm version {} for {}", input.version, input.name));
        }
        grouped.entry(input.name).or_default().push(VersionEntry { version: input.version, yanked: false });
    }
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, mut versions) in grouped {
        let mut seen = std::collections::BTreeSet::new();
        for v in &versions {
            if !seen.insert(v.version.clone()) {
                return Err(format!("npm package {name} has a duplicate version {}", v.version));
            }
        }
        versions.sort_by(|a, b| compare_npm_version(&a.version, &b.version));
        let latest = match dist_latest.get(&name) {
            Some(tag) if versions.iter().any(|v| &v.version == tag) => tag.clone(),
            _ => latest_semver(&versions),
        };
        packages.push(Package { name, source: None, latest, versions });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Npm, generation: generation.to_string(), packages })
}

#[derive(Debug, Clone)]
pub struct PypiInput {
    pub name: String,
    pub version: String,
    pub yanked: bool,
}

pub fn canonical_project_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_sep = false;
    for c in lower.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !in_sep {
                out.push('-');
                in_sep = true;
            }
        } else {
            out.push(c);
            in_sep = false;
        }
    }
    out
}

pub fn pypi_catalog(generation: &str, files: Vec<PypiInput>) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<bool>>> = BTreeMap::new();
    for file in files {
        if file.name.is_empty() || file.version.is_empty() {
            return Err("pypi package name and version must not be empty".to_string());
        }
        let parsed = parse_pep440(&file.version)
            .ok_or_else(|| format!("invalid pypi version {} for {}", file.version, file.name))?;
        let project = canonical_project_name(&file.name);
        if project.is_empty() {
            return Err(format!("invalid pypi package name {}", file.name));
        }
        let _ = parsed;
        grouped.entry(project).or_default().entry(file.version).or_default().push(file.yanked);
    }
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, versions_map) in grouped {
        let mut entries: Vec<(String, bool)> = Vec::with_capacity(versions_map.len());
        for (version, flags) in versions_map {
            let yanked = !flags.is_empty() && flags.iter().all(|y| *y);
            entries.push((version, yanked));
        }
        entries.sort_by(|(a, _), (b, _)| {
            match (parse_pep440(a), parse_pep440(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => a.cmp(b),
            }
        });
        let parsed: Vec<(pep440_rs::Version, &str, bool)> = entries
            .iter()
            .filter_map(|(v, y)| parse_pep440(v).map(|p| (p, v.as_str(), *y)))
            .collect();
        let pool: Vec<(&pep440_rs::Version, &str)> = {
            let non_yanked: Vec<(&pep440_rs::Version, &str)> =
                parsed.iter().filter(|(_, _, y)| !y).map(|(p, v, _)| (p, *v)).collect();
            if non_yanked.is_empty() {
                parsed.iter().map(|(p, v, _)| (p, *v)).collect()
            } else {
                non_yanked
            }
        };
        let stable: Vec<(&pep440_rs::Version, &str)> =
            pool.iter().filter(|(p, _)| p.is_stable()).map(|(p, v)| (*p, *v)).collect();
        let ranked = if stable.is_empty() { pool } else { stable };
        let latest = ranked
            .into_iter()
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
        let versions = entries.into_iter().map(|(version, yanked)| VersionEntry { version, yanked }).collect();
        packages.push(Package { name, source: None, latest, versions });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Pypi, generation: generation.to_string(), packages })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_json() {
        let catalog = cargo_catalog(
            "abc123",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native },
                CargoInput { name: "foo".to_string(), version: "2.0.0-alpha".to_string(), yanked: false, source: CargoSource::Native },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages[0].latest, "1.0.0");
        let text = render_catalog_json(&catalog);
        let parsed = parse_catalog_json(&text).unwrap();
        assert_eq!(parsed, catalog);
    }

    #[test]
    fn npm_prefers_dist_tag() {
        let mut tags = BTreeMap::new();
        tags.insert("pkg".to_string(), "1.0.0".to_string());
        let catalog = npm_catalog(
            "g",
            vec![
                NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string() },
                NpmInput { name: "pkg".to_string(), version: "2.0.0".to_string() },
            ],
            &tags,
        )
        .unwrap();
        assert_eq!(catalog.packages[0].latest, "1.0.0");
    }

    #[test]
    fn pypi_groups_wheels_and_picks_stable_latest() {
        let catalog = pypi_catalog(
            "g",
            vec![
                PypiInput { name: "Foo_Bar".to_string(), version: "1.0".to_string(), yanked: false },
                PypiInput { name: "foo-bar".to_string(), version: "1.0".to_string(), yanked: false },
                PypiInput { name: "foo-bar".to_string(), version: "2.0a1".to_string(), yanked: false },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages.len(), 1);
        assert_eq!(catalog.packages[0].name, "foo-bar");
        assert_eq!(catalog.packages[0].latest, "1.0");
        assert_eq!(catalog.packages[0].versions.len(), 2);
    }

    #[test]
    fn pypi_version_yanked_only_when_all_files_yanked() {
        let catalog = pypi_catalog(
            "g",
            vec![
                PypiInput { name: "pkg".to_string(), version: "1.0".to_string(), yanked: true },
                PypiInput { name: "pkg".to_string(), version: "1.0".to_string(), yanked: false },
            ],
        )
        .unwrap();
        assert!(!catalog.packages[0].versions[0].yanked);
    }

    #[test]
    fn cargo_prefers_stable_and_ignores_yanked() {
        let catalog = cargo_catalog(
            "g",
            vec![
                CargoInput { name: "c".to_string(), version: "2.0.0".to_string(), yanked: true, source: CargoSource::Native },
                CargoInput { name: "c".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native },
                CargoInput { name: "c".to_string(), version: "1.5.0-alpha".to_string(), yanked: false, source: CargoSource::Native },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages[0].latest, "1.0.0");
    }
}
