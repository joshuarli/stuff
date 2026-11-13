#![forbid(unsafe_code)]

use crate::version::{compare_npm_version, compare_semver, is_stable_semver, parse_pep440};
use rustc_hash::{FxHashMap, FxHashSet};

pub const SCHEMA_VERSION: u32 = 4;
const OLDEST_READABLE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Registry {
    Cargo,
    Npm,
    Pypi,
    Oci,
    Mirror,
}

impl Registry {
    pub fn as_str(&self) -> &'static str {
        match self {
            Registry::Cargo => "cargo",
            Registry::Npm => "npm",
            Registry::Pypi => "pypi",
            Registry::Oci => "oci",
            Registry::Mirror => "mirror",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cargo" => Some(Registry::Cargo),
            "npm" => Some(Registry::Npm),
            "pypi" => Some(Registry::Pypi),
            "oci" => Some(Registry::Oci),
            "mirror" => Some(Registry::Mirror),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
#[non_exhaustive]
pub struct VersionEntry {
    pub version: String,
    pub yanked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DependencyKind {
    Runtime,
    Peer,
    Build,
    Dev,
}

impl DependencyKind {
    pub const ALL: [DependencyKind; 4] =
        [DependencyKind::Runtime, DependencyKind::Peer, DependencyKind::Build, DependencyKind::Dev];

    pub fn as_str(&self) -> &'static str {
        match self {
            DependencyKind::Runtime => "runtime",
            DependencyKind::Peer => "peer",
            DependencyKind::Build => "build",
            DependencyKind::Dev => "dev",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dependencies {
    pub runtime: Vec<String>,
    pub peer: Vec<String>,
    pub build: Vec<String>,
    pub dev: Vec<String>,
}

impl Dependencies {
    pub fn runtime_only(names: Vec<String>) -> Self {
        Self { runtime: names, ..Self::default() }
    }

    pub fn of(&self, kind: DependencyKind) -> &[String] {
        match kind {
            DependencyKind::Runtime => &self.runtime,
            DependencyKind::Peer => &self.peer,
            DependencyKind::Build => &self.build,
            DependencyKind::Dev => &self.dev,
        }
    }

    fn of_mut(&mut self, kind: DependencyKind) -> &mut Vec<String> {
        match kind {
            DependencyKind::Runtime => &mut self.runtime,
            DependencyKind::Peer => &mut self.peer,
            DependencyKind::Build => &mut self.build,
            DependencyKind::Dev => &mut self.dev,
        }
    }

    pub fn is_empty(&self) -> bool {
        DependencyKind::ALL.iter().all(|kind| self.of(*kind).is_empty())
    }

    pub fn iter(&self) -> impl Iterator<Item = (DependencyKind, &str)> {
        DependencyKind::ALL
            .into_iter()
            .flat_map(|kind| self.of(kind).iter().map(move |name| (kind, name.as_str())))
    }

    fn merge(&mut self, other: Dependencies) {
        for kind in DependencyKind::ALL {
            self.of_mut(kind).extend(other.of(kind).iter().cloned());
        }
    }

    fn normalized(mut self, owner: &str, rename: impl Fn(&str) -> String) -> Result<Self, String> {
        for kind in DependencyKind::ALL {
            let names = std::mem::take(self.of_mut(kind));
            if names.iter().any(|n| n.is_empty() || n.contains('\0')) {
                return Err(format!("package {owner} has an empty dependency name"));
            }
            let mut unique: Vec<String> = names.iter().map(|n| rename(n)).collect::<FxHashSet<_>>().into_iter().collect();
            unique.sort_unstable();
            *self.of_mut(kind) = unique;
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Package {
    pub name: String,
    pub source: Option<String>,
    pub latest: String,
    pub versions: Vec<VersionEntry>,
    pub dependencies: Dependencies,
}

pub struct PackagesByName<'a>(FxHashMap<&'a str, &'a Package>);

impl<'a> PackagesByName<'a> {
    pub fn resolve(&self, name: &str) -> Option<&'a Package> {
        self.0.get(name).copied()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
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

    pub fn packages_by_name(&self) -> PackagesByName<'_> {
        let mut first = FxHashMap::default();
        for package in &self.packages {
            first.entry(package.name.as_str()).or_insert(package);
        }
        PackagesByName(first)
    }

    pub fn find(&self, name: &str, source: Option<CargoSource>) -> Option<&Package> {
        self.packages
            .iter()
            .find(|p| p.name == name && p.source.as_deref() == source.map(|s| s.as_str()))
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
    out.push_str(&format!("  \"schema_version\": {},\n", catalog.schema_version));
    let records_dependencies = catalog.schema_version >= 2;
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
        let versions_end = if records_dependencies { "," } else { "" };
        if package.versions.is_empty() {
            out.push_str(&format!("]{versions_end}\n"));
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
            out.push_str(&format!("      ]{versions_end}\n"));
        }
        match catalog.schema_version {
            1 => {}
            2 => out.push_str(&format!("      \"dependencies\": {}\n", json_names(&package.dependencies.runtime))),
            _ => {
                let kinds: Vec<String> = DependencyKind::ALL
                    .iter()
                    .map(|kind| format!("\"{}\": {}", kind.as_str(), json_names(package.dependencies.of(*kind))))
                    .collect();
                out.push_str(&format!("      \"dependencies\": {{{}}}\n", kinds.join(", ")));
            }
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

fn json_names(names: &[String]) -> String {
    format!("[{}]", names.iter().map(|n| escape_json(n)).collect::<Vec<_>>().join(", "))
}

fn value_as_str(v: &deser_value::Value) -> Option<&str> {
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

fn parse_names(v: &deser_value::Value, package: &str, label: &str) -> Result<Vec<String>, String> {
    let seq = v
        .as_seq()
        .ok_or_else(|| format!("package {package} {label} must be an array"))?;
    let mut names = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        let name = value_as_str(item)
            .ok_or_else(|| format!("package {package} {label} must be strings"))?;
        if name.is_empty() || name.contains('\0') {
            return Err(format!("package {package} has an empty dependency name"));
        }
        names.push(name.to_string());
    }
    if names.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!("package {package} {label} must be sorted and unique"));
    }
    Ok(names)
}

fn parse_dependencies(v: &deser_value::Value, package: &str, schema: u32) -> Result<Dependencies, String> {
    if schema < 3 {
        return Ok(Dependencies::runtime_only(parse_names(v, package, "dependencies")?));
    }
    let map = v
        .as_map()
        .ok_or_else(|| format!("package {package} dependencies must be an object"))?;
    if map.len() != DependencyKind::ALL.len() {
        return Err(format!("package {package} dependencies have unknown or missing kinds"));
    }
    let mut dependencies = Dependencies::default();
    for kind in DependencyKind::ALL {
        let names = map
            .get(kind.as_str())
            .ok_or_else(|| format!("package {package} dependencies have unknown or missing kinds"))?;
        *dependencies.of_mut(kind) = parse_names(names, package, kind.as_str())?;
    }
    Ok(dependencies)
}

fn parse_package(v: &deser_value::Value, registry: Registry, schema: u32) -> Result<Package, String> {
    let map = v.as_map().ok_or_else(|| "package must be an object".to_string())?;
    let keys: &[&str] = if schema >= 2 {
        &["name", "source", "latest", "versions", "dependencies"]
    } else {
        &["name", "source", "latest", "versions"]
    };
    if map.len() != keys.len() {
        return Err("package has unknown or missing fields".to_string());
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    for key in keys {
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
    let mut seen = FxHashSet::default();
    for v in &versions {
        if !seen.insert(v.version.clone()) {
            return Err(format!("package {name} has a duplicate version {}", v.version));
        }
    }
    if !versions.iter().any(|v| v.version == latest) {
        return Err(format!("package {name} latest {latest} is not in versions"));
    }
    let dependencies = if schema >= 2 {
        parse_dependencies(map.get("dependencies").unwrap_or(&null), name, schema)?
    } else {
        Dependencies::default()
    };
    if registry == Registry::Oci && !dependencies.is_empty() {
        return Err(format!("oci package {name} must not carry dependencies"));
    }
    Ok(Package { name: name.to_string(), source, latest: latest.to_string(), versions, dependencies })
}

pub fn parse_catalog_json(text: &str) -> Result<Catalog, String> {
    let value: deser_value::Value =
        deser_json::from_str(text).map_err(|e| format!("catalog is not valid JSON: {e}"))?;
    let map = value.as_map().ok_or_else(|| "catalog must be an object".to_string())?;
    if map.get("registry").and_then(|value| value.as_str()) == Some("mirror") {
        return parse_mirror_catalog(map);
    }
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
    if schema < u64::from(OLDEST_READABLE_SCHEMA_VERSION) || schema > u64::from(SCHEMA_VERSION) {
        return Err(format!("unsupported catalog schema_version: {schema}"));
    }
    let schema = schema as u32;
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
        packages.push(parse_package(item, registry, schema)?);
    }
    let mut names: Vec<(&str, Option<&str>)> = packages
        .iter()
        .map(|p| (p.name.as_str(), p.source.as_deref()))
        .collect();
    names.sort();
    let mut dedup = names.clone();
    dedup.dedup();
    if dedup.len() != names.len() {
        return Err("catalog contains a duplicate package identity".to_string());
    }
    let mut sorted = packages;
    sorted.sort_by(|a, b| (a.name.as_str(), a.source.as_deref()).cmp(&(b.name.as_str(), b.source.as_deref())));
    Ok(Catalog { schema_version: schema, registry, generation: generation.to_string(), packages: sorted })
}

fn parse_mirror_catalog(map: &deser_value::Map) -> Result<Catalog, String> {
    let keys = ["artifacts", "baseUrl", "generation", "packages", "registry", "renderer", "schemaVersion"];
    if map.len() != keys.len() || keys.iter().any(|key| map.get(*key).is_none()) {
        return Err("mirror catalog has unknown or missing fields".to_string());
    }
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let schema = map.get("schemaVersion").unwrap_or(&null).as_u64().unwrap_or(0);
    if schema != 1 {
        return Err(format!("unsupported mirror catalog schemaVersion: {schema}"));
    }
    map.get("artifacts")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| "mirror catalog artifacts must be an array".to_string())?;
    map.get("baseUrl")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "mirror catalog baseUrl must be a string".to_string())?;
    map.get("renderer")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| "mirror catalog renderer must be a positive integer".to_string())?;
    let generation = map.get("generation").unwrap_or(&null).as_str().unwrap_or("");
    if generation.is_empty() {
        return Err("mirror catalog generation must not be empty".to_string());
    }
    let packages = map.get("packages").unwrap_or(&null).as_seq()
        .ok_or_else(|| "mirror catalog packages must be an array".to_string())?;
    let mut parsed = Vec::with_capacity(packages.len());
    for item in packages {
        let entry = item.as_map().ok_or_else(|| "mirror package entry must be an object".to_string())?;
        let entry_keys = ["name", "source", "versions"];
        if entry.len() != entry_keys.len() || entry_keys.iter().any(|key| entry.get(*key).is_none()) {
            return Err("mirror package has unknown or missing fields".to_string());
        }
        let name = entry.get("name").unwrap_or(&null).as_str().unwrap_or("");
        if name.is_empty() || name.contains('\0') {
            return Err("mirror package name must not be empty".to_string());
        }
        let source = entry.get("source").unwrap_or(&null).as_str().unwrap_or("");
        if source != "upstream" {
            return Err(format!("mirror package {name} source must be upstream"));
        }
        let versions = entry.get("versions").unwrap_or(&null).as_seq()
            .ok_or_else(|| format!("mirror package {name} versions must be an array"))?;
        if versions.is_empty() {
            return Err(format!("mirror package {name} must carry at least one version"));
        }
        let mut parsed_versions = Vec::with_capacity(versions.len());
        for version in versions {
            let version_entry = version.as_map().ok_or_else(|| format!("mirror package {name} version entry must be an object"))?;
            if version_entry.len() != 1 || version_entry.get("version").is_none() {
                return Err(format!("mirror package {name} version entry has unknown or missing fields"));
            }
            let version = version_entry.get("version").unwrap_or(&null).as_str().unwrap_or("");
            if version.is_empty() {
                return Err(format!("mirror package {name} version must not be empty"));
            }
            parsed_versions.push(VersionEntry { version: version.to_string(), yanked: false });
        }
        parsed.push(Package {
            name: name.to_string(),
            source: Some(source.to_string()),
            latest: parsed_versions[0].version.clone(),
            versions: parsed_versions,
            dependencies: Dependencies::default(),
        });
    }
    let mut names: Vec<(&str, Option<&str>)> = parsed
        .iter()
        .map(|package| (package.name.as_str(), package.source.as_deref()))
        .collect();
    names.sort();
    let mut dedup = names.clone();
    dedup.dedup();
    if dedup.len() != names.len() {
        return Err("mirror catalog contains a duplicate package identity".to_string());
    }
    let mut sorted = parsed;
    sorted.sort_by(|a, b| (a.name.as_str(), a.source.as_deref()).cmp(&(b.name.as_str(), b.source.as_deref())));
    Ok(Catalog { schema_version: 1, registry: Registry::Mirror, generation: generation.to_string(), packages: sorted })
}

#[derive(Default)]
struct VersionGroup {
    versions: Vec<VersionEntry>,
    dependencies_by_version: FxHashMap<String, Dependencies>,
}

impl VersionGroup {
    fn push(&mut self, version: String, yanked: bool, dependencies: Dependencies) {
        self.dependencies_by_version.entry(version.clone()).or_default().merge(dependencies);
        self.versions.push(VersionEntry { version, yanked });
    }
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

fn sort_semver_versions(versions: &mut [VersionEntry]) {
    versions.sort_by(|a, b| compare_semver(&a.version, &b.version));
}

#[derive(Debug, Clone)]
pub struct CargoInput {
    pub name: String,
    pub version: String,
    pub yanked: bool,
    pub source: CargoSource,
    pub dependencies: Dependencies,
}

pub fn cargo_catalog(generation: &str, inputs: Vec<CargoInput>) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: FxHashMap<(String, CargoSource), VersionGroup> = FxHashMap::default();
    for input in inputs {
        if input.name.is_empty() || input.version.is_empty() {
            return Err("cargo package name and version must not be empty".to_string());
        }
        if crate::version::Semver::parse(&input.version).is_none() {
            return Err(format!("invalid cargo version {} for {}", input.version, input.name));
        }
        grouped.entry((input.name, input.source)).or_default().push(input.version, input.yanked, input.dependencies);
    }
    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut packages = Vec::with_capacity(grouped.len());
    for ((name, source), group) in grouped {
        let VersionGroup { mut versions, dependencies_by_version: mut dependencies } = group;
        let mut seen = FxHashSet::default();
        for v in &versions {
            if !seen.insert(v.version.clone()) {
                return Err(format!(
                    "cargo package {name} ({}) has a duplicate version {}",
                    source.as_str(),
                    v.version
                ));
            }
        }
        sort_semver_versions(&mut versions);
        let latest = latest_semver(&versions);
        let dependencies = dependencies.remove(&latest).unwrap_or_default().normalized(&name, str::to_string)?;
        packages.push(Package { name, source: Some(source.as_str().to_string()), latest, versions, dependencies });
    }
    packages.sort_by(|a, b| (a.name.as_str(), a.source.as_deref()).cmp(&(b.name.as_str(), b.source.as_deref())));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Cargo, generation: generation.to_string(), packages })
}

#[derive(Debug, Clone)]
pub struct NpmInput {
    pub name: String,
    pub version: String,
    pub dependencies: Dependencies,
}

pub fn npm_catalog(
    generation: &str,
    inputs: Vec<NpmInput>,
    dist_latest: &FxHashMap<String, String>,
) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: FxHashMap<String, VersionGroup> = FxHashMap::default();
    for input in inputs {
        if input.name.is_empty() || input.version.is_empty() {
            return Err("npm package name and version must not be empty".to_string());
        }
        if crate::version::Semver::parse(&input.version).is_none() {
            return Err(format!("invalid npm version {} for {}", input.version, input.name));
        }
        grouped.entry(input.name).or_default().push(input.version, false, input.dependencies);
    }
    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, group) in grouped {
        let VersionGroup { mut versions, dependencies_by_version: mut dependencies } = group;
        let mut seen = FxHashSet::default();
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
        let dependencies = dependencies.remove(&latest).unwrap_or_default().normalized(&name, str::to_string)?;
        packages.push(Package { name, source: None, latest, versions, dependencies });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Npm, generation: generation.to_string(), packages })
}

#[derive(Debug, Clone)]
pub struct OciInput {
    pub name: String,
    pub tags: Vec<String>,
}

pub fn oci_catalog(generation: &str, inputs: Vec<OciInput>) -> Result<Catalog, String> {
    if generation.is_empty() {
        return Err("generation must not be empty".to_string());
    }
    let mut grouped: FxHashMap<String, Vec<String>> = FxHashMap::default();
    for input in inputs {
        if input.name.is_empty() {
            return Err("oci repository name must not be empty".to_string());
        }
        let slot = grouped.entry(input.name).or_default();
        slot.extend(input.tags);
    }
    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, mut tags) in grouped {
        if tags.iter().any(|tag| tag.is_empty()) {
            return Err(format!("oci repository {name} has an empty tag"));
        }
        tags.sort();
        tags.dedup();
        if tags.is_empty() {
            continue;
        }
        let latest = if tags.iter().any(|tag| tag == "latest") {
            "latest".to_string()
        } else {
            tags.last().cloned().expect("tags is not empty")
        };
        let versions = tags.into_iter().map(|version| VersionEntry { version, yanked: false }).collect();
        packages.push(Package {
            name,
            source: None,
            latest,
            versions,
            dependencies: Dependencies::default(),
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Catalog { schema_version: SCHEMA_VERSION, registry: Registry::Oci, generation: generation.to_string(), packages })
}

#[derive(Debug, Clone)]
pub struct PypiInput {
    pub name: String,
    pub version: String,
    pub yanked: bool,
    pub dependencies: Dependencies,
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
    let mut grouped: FxHashMap<String, FxHashMap<String, Vec<bool>>> = FxHashMap::default();
    let mut dependencies_by_project: FxHashMap<String, FxHashMap<String, Dependencies>> = FxHashMap::default();
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
        dependencies_by_project
            .entry(project.clone())
            .or_default()
            .entry(file.version.clone())
            .or_default()
            .merge(file.dependencies);
        grouped.entry(project).or_default().entry(file.version).or_default().push(file.yanked);
    }
    let mut grouped: Vec<_> = grouped.into_iter().collect();
    grouped.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut packages = Vec::with_capacity(grouped.len());
    for (name, versions_map) in grouped {
        let mut version_rows: Vec<_> = versions_map.into_iter().collect();
        version_rows.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut entries: Vec<(String, bool)> = Vec::with_capacity(version_rows.len());
        for (version, flags) in version_rows {
            // Version-level yank is conservative-by-construction: the index
            // stays per-file, so a version reads yanked only when every file
            // is. Partial yank information lives in the simple pages, not here.
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
        let dependencies = dependencies_by_project
            .get_mut(&name)
            .and_then(|by_version| by_version.remove(&latest))
            .unwrap_or_default()
            .normalized(&name, canonical_project_name)?;
        packages.push(Package { name, source: None, latest, versions, dependencies });
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
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "foo".to_string(), version: "2.0.0-alpha".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages[0].latest, "1.0.0");
        let text = render_catalog_json(&catalog);
        let parsed = parse_catalog_json(&text).unwrap();
        assert_eq!(parsed, catalog);
    }

    #[test]
    fn cargo_keeps_native_and_upstream_identities_separate() {
        let catalog = cargo_catalog(
            "g",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "foo".to_string(), version: "2.0.0".to_string(), yanked: false, source: CargoSource::Upstream, dependencies: Dependencies::default() },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages.len(), 2);
        let native = catalog.find("foo", Some(CargoSource::Native)).unwrap();
        assert_eq!(native.latest, "1.0.0");
        let upstream = catalog.find("foo", Some(CargoSource::Upstream)).unwrap();
        assert_eq!(upstream.latest, "2.0.0");
        let parsed = parse_catalog_json(&render_catalog_json(&catalog)).unwrap();
        assert_eq!(parsed, catalog);
    }

    #[test]
    fn cargo_rejects_duplicate_versions_within_one_source() {
        let error = cargo_catalog(
            "g",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: true, source: CargoSource::Native, dependencies: Dependencies::default() },
            ],
        )
        .unwrap_err();
        assert!(error.contains("duplicate version"));
    }

    #[test]
    fn npm_prefers_dist_tag() {
        let mut tags = FxHashMap::default();
        tags.insert("pkg".to_string(), "1.0.0".to_string());
        let catalog = npm_catalog(
            "g",
            vec![
                NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() },
                NpmInput { name: "pkg".to_string(), version: "2.0.0".to_string(), dependencies: Dependencies::default() },
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
                PypiInput { name: "Foo_Bar".to_string(), version: "1.0".to_string(), yanked: false, dependencies: Dependencies::default() },
                PypiInput { name: "foo-bar".to_string(), version: "1.0".to_string(), yanked: false, dependencies: Dependencies::default() },
                PypiInput { name: "foo-bar".to_string(), version: "2.0a1".to_string(), yanked: false, dependencies: Dependencies::default() },
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
                PypiInput { name: "pkg".to_string(), version: "1.0".to_string(), yanked: true, dependencies: Dependencies::default() },
                PypiInput { name: "pkg".to_string(), version: "1.0".to_string(), yanked: false, dependencies: Dependencies::default() },
            ],
        )
        .unwrap();
        assert!(!catalog.packages[0].versions[0].yanked);
    }

    #[test]
    fn oci_catalogs_use_tags_as_versions() {
        let catalog = oci_catalog(
            "g",
            vec![
                OciInput {
                    name: "example/app".to_string(),
                    tags: vec!["stable".to_string(), "beta".to_string(), "beta".to_string()],
                },
                OciInput { name: "example/empty".to_string(), tags: vec![] },
                OciInput {
                    name: "example/tagged".to_string(),
                    tags: vec!["latest".to_string(), "latest".to_string()],
                },
            ],
        )
        .unwrap();
        assert_eq!(catalog.schema_version, 4);
        assert_eq!(catalog.registry, Registry::Oci);
        assert_eq!(catalog.packages.len(), 2);
        assert_eq!(catalog.packages[0].name, "example/app");
        assert_eq!(catalog.packages[0].latest, "stable");
        assert_eq!(
            catalog.packages[0].versions.iter().map(|v| v.version.as_str()).collect::<Vec<_>>(),
            ["beta", "stable"]
        );
        assert!(catalog.packages[0].dependencies.is_empty());
        assert_eq!(catalog.packages[1].name, "example/tagged");
        assert_eq!(catalog.packages[1].latest, "latest");
        let rendered = render_catalog_json(&catalog);
        assert!(rendered.contains("\"schema_version\": 4"));
        assert!(rendered.contains("\"registry\": \"oci\""));
        assert_eq!(parse_catalog_json(&rendered).unwrap(), catalog);
    }

    #[test]
    fn oci_catalogs_reject_dependencies_and_empty_tags() {
        let text = r#"{"schema_version":4,"registry":"oci","generation":"g","packages":[{"name":"a","source":null,"latest":"1","versions":[{"version":"1","yanked":false}],"dependencies":{"runtime":["b"],"peer":[],"build":[],"dev":[]}}]}"#;
        assert!(parse_catalog_json(text).unwrap_err().contains("must not carry dependencies"));
        assert!(oci_catalog("g", vec![OciInput { name: "a".to_string(), tags: vec![String::new()] }]).is_err());
    }

    #[test]
    fn cargo_prefers_stable_and_ignores_yanked() {
        let catalog = cargo_catalog(
            "g",
            vec![
                CargoInput { name: "c".to_string(), version: "2.0.0".to_string(), yanked: true, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "c".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "c".to_string(), version: "1.5.0-alpha".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages[0].latest, "1.0.0");
    }

    #[test]
    fn dependencies_follow_the_latest_version_and_round_trip() {
        let catalog = cargo_catalog(
            "g",
            vec![
                CargoInput {
                    name: "app".to_string(),
                    version: "1.0.0".to_string(),
                    yanked: false,
                    source: CargoSource::Native,
                    dependencies: Dependencies::runtime_only(vec!["old".to_string()]),
                },
                CargoInput {
                    name: "app".to_string(),
                    version: "1.1.0".to_string(),
                    yanked: false,
                    source: CargoSource::Native,
                    dependencies: Dependencies {
                        runtime: vec!["zeta".to_string(), "alpha".to_string(), "zeta".to_string()],
                        peer: vec!["peer".to_string()],
                        build: vec!["cc".to_string()],
                        dev: vec!["tempfile".to_string(), "criterion".to_string()],
                    },
                },
            ],
        )
        .unwrap();
        let dependencies = &catalog.packages[0].dependencies;
        assert_eq!(dependencies.runtime, ["alpha", "zeta"]);
        assert_eq!(dependencies.peer, ["peer"]);
        assert_eq!(dependencies.build, ["cc"]);
        assert_eq!(dependencies.dev, ["criterion", "tempfile"]);
        let parsed = parse_catalog_json(&render_catalog_json(&catalog)).unwrap();
        assert_eq!(parsed, catalog);
    }

    #[test]
    fn pypi_dependencies_are_canonical_and_merged_across_wheels() {
        let catalog = pypi_catalog(
            "g",
            vec![
                PypiInput {
                    name: "pkg".to_string(),
                    version: "1.0".to_string(),
                    yanked: false,
                    dependencies: Dependencies::runtime_only(vec!["Typing_Extensions".to_string()]),
                },
                PypiInput {
                    name: "pkg".to_string(),
                    version: "1.0".to_string(),
                    yanked: false,
                    dependencies: Dependencies::runtime_only(vec!["colorama".to_string(), "typing-extensions".to_string()]),
                },
            ],
        )
        .unwrap();
        assert_eq!(catalog.packages[0].dependencies.runtime, ["colorama", "typing-extensions"]);
    }

    #[test]
    fn schema_one_catalogs_stay_readable_without_dependencies() {
        let text = r#"{"schema_version":1,"registry":"npm","generation":"g","packages":[{"name":"a","source":null,"latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}]}]}"#;
        let catalog = parse_catalog_json(text).unwrap();
        assert_eq!(catalog.schema_version, 1);
        assert!(catalog.packages[0].dependencies.is_empty());
        let rendered = render_catalog_json(&catalog);
        assert!(rendered.contains("\"schema_version\": 1"));
        assert!(!rendered.contains("dependencies"));
        assert_eq!(parse_catalog_json(&rendered).unwrap(), catalog);
    }

    #[test]
    fn schema_two_catalogs_keep_their_runtime_names_and_round_trip_as_schema_two() {
        let text = r#"{"schema_version":2,"registry":"npm","generation":"g","packages":[{"name":"a","source":null,"latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}],"dependencies":["b","c"]}]}"#;
        let catalog = parse_catalog_json(text).unwrap();
        assert_eq!(catalog.schema_version, 2);
        assert_eq!(catalog.packages[0].dependencies.runtime, ["b", "c"]);
        assert!(catalog.packages[0].dependencies.dev.is_empty());
        let rendered = render_catalog_json(&catalog);
        assert!(rendered.contains("\"dependencies\": [\"b\", \"c\"]"));
        assert_eq!(parse_catalog_json(&rendered).unwrap(), catalog);
    }

    #[test]
    fn schema_three_requires_every_kind_sorted_and_unique() {
        let text = |deps: &str| {
            format!(
                r#"{{"schema_version":3,"registry":"npm","generation":"g","packages":[{{"name":"a","source":null,"latest":"1.0.0","versions":[{{"version":"1.0.0","yanked":false}}],"dependencies":{deps}}}]}}"#
            )
        };
        let ok = r#"{"runtime":["b"],"peer":[],"build":[],"dev":["c","d"]}"#;
        assert_eq!(parse_catalog_json(&text(ok)).unwrap().packages[0].dependencies.dev, ["c", "d"]);
        let unsorted = r#"{"runtime":["c","b"],"peer":[],"build":[],"dev":[]}"#;
        assert!(parse_catalog_json(&text(unsorted)).unwrap_err().contains("sorted"));
        let missing_kind = r#"{"runtime":[],"peer":[],"build":[]}"#;
        assert!(parse_catalog_json(&text(missing_kind)).unwrap_err().contains("kinds"));
        let unknown_kind = r#"{"runtime":[],"peer":[],"build":[],"optional":[]}"#;
        assert!(parse_catalog_json(&text(unknown_kind)).unwrap_err().contains("kinds"));
        assert!(parse_catalog_json(&text(r#"["b"]"#)).unwrap_err().contains("object"));
    }

    fn mirror_catalog_text() -> &'static str {
        r#"{
  "artifacts": [
    {"bytes": 3, "file": "node-v26.13.1-darwin-arm64.zip", "files": ["bin/node"], "id": "node-v26.13.1-darwin-arm64", "sha256": "af6202f537", "stripComponents": 1, "upstream": {"sha256": "af6202f537", "url": "https://nodejs.org/dist/v26.13.1/node-v26.13.1-darwin-arm64.tar.xz"}, "version": "26.13.1"}
  ],
  "baseUrl": "https://mirror.17166969.xyz/",
  "generation": "0f455520",
  "packages": [
    {"name": "node-v26.13.1-darwin-arm64", "source": "upstream", "versions": [{"version": "26.13.1"}]},
    {"name": "node-v26.13.1-linux-x64", "source": "upstream", "versions": [{"version": "26.13.1"}]}
  ],
  "registry": "mirror",
  "renderer": 1,
  "schemaVersion": 1
}"#
    }

    #[test]
    fn parses_the_mirror_catalog_dialect() {
        let catalog = parse_catalog_json(mirror_catalog_text()).unwrap();
        assert_eq!(catalog.registry, Registry::Mirror);
        assert_eq!(catalog.generation, "0f455520");
        assert_eq!(catalog.schema_version, 1);
        assert_eq!(catalog.packages.len(), 2);
        assert_eq!(catalog.packages[0].name, "node-v26.13.1-darwin-arm64");
        assert_eq!(catalog.packages[0].source.as_deref(), Some("upstream"));
        assert_eq!(catalog.packages[0].latest, "26.13.1");
        assert_eq!(catalog.packages[0].versions.len(), 1);
        assert!(catalog.packages[0].dependencies.is_empty());
        assert_eq!(catalog.package_count(), 2);
    }

    #[test]
    fn mirror_dialect_rejects_unknown_top_level_fields() {
        let text = mirror_catalog_text().replace("\"renderer\": 1,\n", "\"renderer\": 1,\n  \"packageCount\": 2,\n");
        assert!(parse_catalog_json(&text).unwrap_err().contains("unknown or missing fields"));
    }

    #[test]
    fn mirror_dialect_requires_a_version_per_package() {
        let text = mirror_catalog_text().replace(
            "\"versions\": [{\"version\": \"26.13.1\"}]",
            "\"versions\": []",
        );
        assert!(parse_catalog_json(&text).unwrap_err().contains("at least one version"));
    }

    #[test]
    fn mirror_dialect_requires_upstream_source() {
        let text = mirror_catalog_text().replace(
            "\"source\": \"upstream\"",
            "\"source\": \"first-party\"",
        );
        assert!(parse_catalog_json(&text).unwrap_err().contains("must be upstream"));
    }

    #[test]
    fn mirror_dialect_rejects_mismatched_schema_version() {
        let text = mirror_catalog_text().replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert!(parse_catalog_json(&text).unwrap_err().contains("schemaVersion"));
    }
}
