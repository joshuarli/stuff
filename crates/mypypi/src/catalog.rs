#![forbid(unsafe_code)]

use crate::error::{Error, Result, Stage};
use crate::models::PackageVersion;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSettings {
    pub likely_binary_ignore: Vec<(String, String)>,
    pub custom_prebuild: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationSettings {
    pub validate_extras: Vec<(String, String)>,
    pub validate_incorrect_missing_deps: Vec<(String, String)>,
    pub validate_skip_imports: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageEntry {
    pub package: PackageVersion,
    pub build: BuildSettings,
    pub validation: ValidationSettings,
    pub excluded_platforms: Vec<(String, String)>,
    pub yanked_reason: Option<String>,
    pub platforms: Option<Vec<String>>,
    pub clear_yanked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub entries: Vec<PackageEntry>,
}

impl Catalog {
    pub fn by_package(&self) -> BTreeMap<PackageVersion, &PackageEntry> {
        self.entries.iter().map(|e| (e.package.clone(), e)).collect()
    }
}

fn fail(detail: impl Into<String>) -> Error {
    Error::new(crate::error::Kind::Configuration, detail, Stage::Configuration)
}

fn map_keys(map: &deser_value::Map) -> Vec<String> {
    let mut out = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let Some((k, _)) = map.get_index(i)
            && let Some(s) = k.as_str() {
                out.push(s.to_string());
            }
    }
    out
}

fn require_table<'a>(v: &'a deser_value::Value, label: &str) -> Result<&'a deser_value::Map> {
    v.as_map()
        .ok_or_else(|| fail(format!("{label} must be a table")))
}

fn check_mapping(map: &deser_value::Map, label: &str, allowed: &[&str]) -> Result<()> {
    for key in map_keys(map) {
        if !allowed.contains(&key.as_str()) {
            return Err(fail(format!("unknown {label} key: {key}")));
        }
    }
    Ok(())
}

fn reason_map_sorted(v: &deser_value::Value, label: &str) -> Result<Vec<(String, String)>> {
    let map = require_table(v, label)?;
    let mut pairs = Vec::with_capacity(map.len());
    for key in map_keys(map) {
        if key.trim().is_empty() || key.trim() != key {
            return Err(fail(format!("{label} keys must be non-empty strings")));
        }
        let reason = map
            .get(key.as_str())
            .and_then(|r| r.as_str())
            .unwrap_or("");
        if reason.trim().is_empty() || reason.trim() != reason {
            return Err(fail(format!("{label} requires a non-empty reason for {key}")));
        }
        pairs.push((key, reason.to_string()));
    }
    pairs.sort();
    Ok(pairs)
}

fn reason_map_ordered(v: &deser_value::Value, label: &str) -> Result<Vec<(String, String)>> {
    let map = require_table(v, label)?;
    let mut pairs = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        let Some((k, reason_val)) = map.get_index(i) else {
            continue;
        };
        let key = k.as_str().unwrap_or("").to_string();
        if key.trim().is_empty() || key.trim() != key {
            return Err(fail(format!("{label} keys must be non-empty strings")));
        }
        let reason = reason_val.as_str().unwrap_or("");
        if reason.trim().is_empty() || reason.trim() != reason {
            return Err(fail(format!("{label} requires a non-empty reason for {key}")));
        }
        pairs.push((key, reason.to_string()));
    }
    Ok(pairs)
}

fn platforms_of(v: &deser_value::Value, label: &str) -> Result<Option<Vec<String>>> {
    if matches!(&**v, deser_value::Kind::Null) {
        return Ok(None);
    }
    let seq = v
        .as_seq()
        .ok_or_else(|| fail(format!("{label} must be a non-empty array of platform names")))?;
    if seq.is_empty() {
        return Err(fail(format!("{label} must be a non-empty array of platform names")));
    }
    let mut out = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        match item.as_str() {
            Some(s) if !s.is_empty() && s.trim() == s => out.push(s.to_string()),
            _ => {
                return Err(fail(format!(
                    "{label} must be a non-empty array of platform names"
                )));
            }
        }
    }
    let mut sorted = out.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != out.len() {
        return Err(fail(format!("{label} platform names must be unique")));
    }
    Ok(Some(out))
}

fn entry_of(v: &deser_value::Value) -> Result<PackageEntry> {
    let row = require_table(v, "package entry")?;
    check_mapping(
        row,
        "package entry",
        &[
            "name",
            "version",
            "build",
            "validation",
            "exclude_platforms",
            "platforms",
            "yanked_reason",
            "yanked",
        ],
    )?;
    let null = deser_value::Value::from(deser_value::Kind::Null);
    let name = row.get("name").unwrap_or(&null);
    let version_text = row.get("version").unwrap_or(&null);
    let name_str = name.as_str().unwrap_or("");
    if crate::models::canonicalize_name(name_str) != name_str || name_str.is_empty() {
        return Err(fail("package name must be canonical"));
    }
    let version_str = version_text.as_str().unwrap_or("");
    if version_str.is_empty() {
        return Err(fail(format!("{name_str}: version must be an exact version pin")));
    }
    let package = PackageVersion::parse(name_str, version_str)
        .map_err(|_| fail(format!("{name_str}: version must be an exact canonical version")))?;
    if package.version.to_string() != version_str {
        return Err(fail(format!("{name_str}: version pin must be canonical")));
    }

    let build_row = row.get("build").unwrap_or(&null);
    let build_map = if matches!(&**build_row, deser_value::Kind::Null) {
        None
    } else {
        Some(require_table(
            build_row,
            &format!("{package} build settings"),
        )?)
    };
    if let Some(m) = build_map {
        check_mapping(
            m,
            &format!("{package} build settings"),
            &["likely_binary_ignore", "custom_prebuild"],
        )?;
    }
    let empty_table = deser_value::Value::from(deser_value::Map::new());
    let build_ignore_src = build_map
        .and_then(|m| m.get("likely_binary_ignore"))
        .unwrap_or(&empty_table);
    let build_ignore = if matches!(&**build_ignore_src, deser_value::Kind::Null) {
        Vec::new()
    } else {
        reason_map_sorted(build_ignore_src, &format!("{package} likely_binary_ignore"))?
    };
    let prebuild_src = build_map
        .and_then(|m| m.get("custom_prebuild"))
        .unwrap_or(&empty_table);
    let prebuild = if matches!(&**prebuild_src, deser_value::Kind::Null) {
        Vec::new()
    } else {
        reason_map_ordered(prebuild_src, &format!("{package} custom_prebuild"))?
    };
    if !prebuild.is_empty() && !prebuild.iter().all(|(cmd, _)| cmd.contains("{prefix}")) {
        return Err(fail(format!(
            "{package}: every custom_prebuild command must use {{prefix}}"
        )));
    }

    let validation_row = row.get("validation").unwrap_or(&null);
    let validation_map = if matches!(&**validation_row, deser_value::Kind::Null) {
        None
    } else {
        Some(require_table(
            validation_row,
            &format!("{package} validation settings"),
        )?)
    };
    if let Some(m) = validation_map {
        check_mapping(
            m,
            &format!("{package} validation settings"),
            &[
                "validate_extras",
                "validate_incorrect_missing_deps",
                "validate_skip_imports",
            ],
        )?;
    }
    let get_validation = |key: &str| -> Result<Vec<(String, String)>> {
        let src = validation_map
            .and_then(|m| m.get(key))
            .unwrap_or(&empty_table);
        if matches!(&**src, deser_value::Kind::Null) {
            return Ok(Vec::new());
        }
        reason_map_sorted(src, &format!("{package} {key}"))
    };
    let validation = ValidationSettings {
        validate_extras: get_validation("validate_extras")?,
        validate_incorrect_missing_deps: get_validation("validate_incorrect_missing_deps")?,
        validate_skip_imports: get_validation("validate_skip_imports")?,
    };

    let exclusions_src = row.get("exclude_platforms").unwrap_or(&empty_table);
    let exclusions = if matches!(&**exclusions_src, deser_value::Kind::Null) {
        Vec::new()
    } else {
        reason_map_sorted(exclusions_src, &format!("{package} exclusions"))?
    };

    let yanked_reason = match row.get("yanked_reason") {
        Some(v) if matches!(&**v, deser_value::Kind::Null) => None,
        Some(v) => match v.as_str() {
            Some(s) if !s.trim().is_empty() => Some(s.to_string()),
            _ => return Err(fail(format!("{package}: yanked_reason must be non-empty"))),
        },
        None => None,
    };
    let yanked = match row.get("yanked") {
        Some(v) if matches!(&**v, deser_value::Kind::Null) => None,
        Some(v) => match v.as_bool() {
            Some(b) => Some(b),
            None => return Err(fail(format!("{package}: yanked must be true or false"))),
        },
        None => None,
    };
    if yanked == Some(true) && yanked_reason.is_none() {
        return Err(fail(format!("{package}: yanked = true requires yanked_reason")));
    }
    if yanked == Some(false) && yanked_reason.is_some() {
        return Err(fail(format!("{package}: yanked = false cannot include yanked_reason")));
    }
    let platforms = match row.get("platforms") {
        Some(v) if matches!(&**v, deser_value::Kind::Null) => None,
        Some(v) => platforms_of(v, &format!("{package} platforms"))?,
        None => None,
    };
    Ok(PackageEntry {
        package,
        build: BuildSettings {
            likely_binary_ignore: build_ignore,
            custom_prebuild: prebuild,
        },
        validation,
        excluded_platforms: exclusions,
        yanked_reason,
        platforms,
        clear_yanked: yanked == Some(false),
    })
}

fn merge_entries(first: PackageEntry, second: PackageEntry) -> Result<PackageEntry> {
    if first.build != second.build
        || first.validation != second.validation
        || first.excluded_platforms != second.excluded_platforms
        || first.yanked_reason != second.yanked_reason
        || first.clear_yanked != second.clear_yanked
    {
        return Err(fail(format!(
            "catalog has conflicting settings for {}",
            first.package
        )));
    }
    match (first.platforms, second.platforms) {
        (None, None) => Err(fail("catalog contains a duplicate package version")),
        (None, _) | (_, None) => Ok(PackageEntry {
            package: first.package,
            build: first.build,
            validation: first.validation,
            excluded_platforms: first.excluded_platforms,
            yanked_reason: first.yanked_reason,
            platforms: None,
            clear_yanked: first.clear_yanked,
        }),
        (Some(mut a), Some(b)) => {
            for p in &b {
                if a.contains(p) {
                    return Err(fail(format!(
                        "catalog contains a duplicate package version for {p}"
                    )));
                }
            }
            a.extend(b);
            Ok(PackageEntry {
                package: first.package,
                build: first.build,
                validation: first.validation,
                excluded_platforms: first.excluded_platforms,
                yanked_reason: first.yanked_reason,
                platforms: Some(a),
                clear_yanked: first.clear_yanked,
            })
        }
    }
}

pub fn parse_catalog(source: &str) -> Result<Catalog> {
    let value: deser_value::Value =
        deser_toml::from_str(source).map_err(|e| fail(format!("invalid packages.toml: {e}")))?;
    let doc = require_table(&value, "catalog")?;
    for key in map_keys(doc) {
        if key != "packages" {
            return Err(fail(format!("unknown catalog key: {key}")));
        }
    }
    let rows_val = doc
        .get("packages")
        .ok_or_else(|| fail("packages must be an array of tables"))?;
    let rows = rows_val
        .as_seq()
        .ok_or_else(|| fail("packages must be an array of tables"))?;
    let mut entries: Vec<PackageEntry> = Vec::new();
    let mut positions: BTreeMap<PackageVersion, usize> = BTreeMap::new();
    for row in rows.iter() {
        let e = entry_of(row)?;
        match positions.get(&e.package) {
            None => {
                positions.insert(e.package.clone(), entries.len());
                entries.push(e);
            }
            Some(&pos) => {
                let merged = merge_entries(entries[pos].clone(), e)?;
                entries[pos] = merged;
            }
        }
    }
    Ok(Catalog { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_keys_and_duplicates() {
        assert!(parse_catalog("other = 1\n").is_err());
        let dup = "[[packages]]\nname = \"foo\"\nversion = \"1.0\"\n[[packages]]\nname = \"foo\"\nversion = \"1.0\"\n";
        assert!(parse_catalog(dup).is_err());
    }

    #[test]
    fn merges_split_platforms() {
        let src = "[[packages]]\nname = \"foo\"\nversion = \"1.0\"\nplatforms = [\"linux-x86_64\"]\n[[packages]]\nname = \"foo\"\nversion = \"1.0\"\nplatforms = [\"macos-arm64\"]\n";
        let catalog = parse_catalog(src).unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].platforms.as_ref().unwrap().len(), 2);
    }
}
