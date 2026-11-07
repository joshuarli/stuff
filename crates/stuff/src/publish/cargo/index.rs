use super::crate_file::PackagedManifest;
use super::model::{
    CRATES_IO_INDEX, DependencyKind, IndexDep, IndexEntry, PublishDependency, PublishRequest,
};
use super::requirements::canonical_requirement;
use super::semver::Version;
use super::{input, integrity, names, Rejection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Registry<'a> {
    Same,
    CratesIo,
    Foreign(&'a str),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyKey {
    alias: String,
    real: String,
    kind: String,
    target: Option<String>,
}

#[derive(Clone, Debug)]
struct NormalizedDep {
    key: DependencyKey,
    version_req: String,
    features: Vec<String>,
    optional: bool,
    default_features: bool,
    registry: Option<String>,
    package: Option<String>,
}

pub fn build_entry(
    request: &PublishRequest,
    manifest: &PackagedManifest,
    cksum: &str,
) -> Result<IndexEntry, Rejection> {
    names::validate_crate_name(&request.name)?;
    Version::parse(&request.vers)?;
    reconcile_manifest(request, manifest)?;

    let mut deps = Vec::new();
    for dep in &request.deps {
        if dep.name.as_deref().unwrap_or_default().is_empty()
            && dep.explicit_name_in_toml.is_none()
        {
            return Err(input("dependency missing name"));
        }
        let mut dep_name = dep.name.clone().unwrap_or_default();
        let mut dep_package = dep.package.clone();
        if let Some(explicit) = &dep.explicit_name_in_toml {
            dep_name = explicit.clone();
            if dep_package.is_none() {
                dep_package = dep.name.clone();
            }
        }
        if dep.version_req.is_empty() {
            return Err(input(format!(
                "dependency '{dep_name}' missing version_req"
            )));
        }
        if canonical_requirement(&dep.version_req).is_err() {
            return Err(input(format!(
                "dependency '{dep_name}' has invalid version_req '{}'",
                dep.version_req
            )));
        }
        if dep_name.is_empty() {
            return Err(input("dependency with empty name"));
        }
        if names::validate_crate_name(&dep_name).is_err() {
            return Err(input(format!("dependency has invalid name '{dep_name}'")));
        }
        if let Some(package) = &dep_package
            && names::validate_crate_name(package).is_err()
        {
            return Err(input(format!(
                "dependency has invalid package '{package}'"
            )));
        }
        deps.push(IndexDep {
            name: dep_name,
            req: dep.version_req.clone(),
            features: dep.features.clone(),
            optional: dep.optional,
            default_features: dep.default_features,
            target: dep.target.clone(),
            kind: dep.kind.as_deref().map(DependencyKind::parse),
            registry: dep.registry.clone().filter(|value| !value.is_empty()),
            package: dep_package,
        });
    }

    Ok(IndexEntry {
        name: request.name.clone(),
        vers: request.vers.clone(),
        deps,
        cksum: cksum.to_string(),
        features: request.features.sorted(),
        yanked: request.yanked,
        links: request.links.clone(),
        rust_version: request.rust_version.clone(),
        v: Some(if request.features.has_v2_syntax() { 2 } else { 1 }),
        extra: Vec::new(),
    })
}

fn reconcile_manifest(
    request: &PublishRequest,
    manifest: &PackagedManifest,
) -> Result<(), Rejection> {
    if manifest.name != request.name || manifest.version != request.vers {
        return Err(integrity("name/version mismatch"));
    }
    if manifest.links != request.links {
        return Err(integrity(
            "links mismatch between archive and publish metadata",
        ));
    }
    if manifest.rust_version != request.rust_version {
        return Err(integrity(
            "rust_version mismatch between archive and publish metadata",
        ));
    }
    if !features_equal(&manifest.features, &request.features) {
        return Err(integrity(
            "features mismatch between archive and publish metadata",
        ));
    }

    let packaged: Vec<NormalizedDep> = manifest
        .dependencies
        .iter()
        .map(|dependency| NormalizedDep {
            key: DependencyKey {
                alias: dependency.alias.clone(),
                real: dependency
                    .package
                    .clone()
                    .unwrap_or_else(|| dependency.alias.clone()),
                kind: kind_text(dependency.kind.as_ref()),
                target: normalize_target(dependency.target.as_deref()),
            },
            version_req: dependency.version_req.clone(),
            features: dependency.features.clone(),
            optional: dependency.optional,
            default_features: dependency.default_features,
            registry: dependency
                .registry
                .clone()
                .filter(|value| !value.is_empty()),
            package: dependency.package.clone(),
        })
        .collect();

    let mut published: Vec<(NormalizedDep, &PublishDependency)> = Vec::new();
    for dependency in &request.deps {
        let (alias, real) = publish_alias_real(dependency)?;
        published.push((
            NormalizedDep {
                key: DependencyKey {
                    alias,
                    real,
                    kind: publish_kind_text(dependency.kind.as_deref()),
                    target: normalize_target(dependency.target.as_deref()),
                },
                version_req: dependency.version_req.clone(),
                features: dependency.features.clone(),
                optional: dependency.optional,
                default_features: dependency.default_features,
                registry: dependency
                    .registry
                    .clone()
                    .filter(|value| !value.is_empty()),
                package: dependency.package.clone(),
            },
            dependency,
        ));
    }

    if packaged.len() != published.len() {
        return Err(integrity(format!(
            "dependency count mismatch: archive has {}, publish metadata has {}",
            packaged.len(),
            published.len()
        )));
    }

    let packaged_groups = group_packaged(&packaged);
    let published_groups = group_published(&published);
    let mut packaged_keys: Vec<DependencyKey> = packaged_groups
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    packaged_keys.sort();
    let mut published_keys: Vec<DependencyKey> = published_groups
        .iter()
        .map(|(key, _)| key.clone())
        .collect();
    published_keys.sort();
    if packaged_keys != published_keys {
        let mut problems = Vec::new();
        for key in &packaged_keys {
            if !published_keys.contains(key) {
                problems.push(format!(
                    "archive-only '{}' kind={} target={}",
                    key.alias,
                    key.kind,
                    target_repr(&key.target)
                ));
            }
        }
        for key in &published_keys {
            if !packaged_keys.contains(key) {
                problems.push(format!(
                    "publish-only '{}' kind={} target={}",
                    key.alias,
                    key.kind,
                    target_repr(&key.target)
                ));
            }
        }
        return Err(integrity(format!(
            "dependency set mismatch (alias/package/kind/target): {}",
            problems.join("; ")
        )));
    }

    for (key, packaged_indexes) in &packaged_groups {
        let published_indexes = published_groups
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, indexes)| indexes)
            .ok_or_else(|| integrity("dependency set mismatch (alias/package/kind/target): "))?;
        if packaged_indexes.len() != published_indexes.len() {
            return Err(integrity(format!(
                "dependency '{}' count mismatch for kind={} target={}",
                key.alias,
                key.kind,
                target_repr(&key.target)
            )));
        }
        for (packaged_index, published_index) in
            packaged_indexes.iter().zip(published_indexes.iter())
        {
            let expected = &packaged[*packaged_index];
            let (got, raw) = &published[*published_index];
            if !requirements_match(&got.version_req, &expected.version_req) {
                return Err(integrity(format!(
                    "dependency '{}' version_req mismatch: archive '{}' vs publish '{}'",
                    key.alias, expected.version_req, got.version_req
                )));
            }
            let mut got_features = got.features.clone();
            got_features.sort();
            let mut expected_features = expected.features.clone();
            expected_features.sort();
            if got_features != expected_features
                || got.optional != expected.optional
                || got.default_features != expected.default_features
            {
                return Err(integrity(format!(
                    "dependency '{}' flags mismatch",
                    key.alias
                )));
            }
            match_registry(got.registry.as_deref(), expected.registry.as_deref(), &key.alias)?;
            if !package_matches(raw, expected.package.as_deref(), &key.alias) {
                return Err(integrity(format!(
                    "dependency '{}' package mismatch",
                    key.alias
                )));
            }
        }
    }
    Ok(())
}

fn features_equal(left: &super::model::Features, right: &super::model::Features) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter().all(|(key, values)| {
        let mut left_values = values.to_vec();
        left_values.sort();
        right.get(key).is_some_and(|right_values| {
            let mut right_values = right_values.to_vec();
            right_values.sort();
            left_values == right_values
        })
    })
}

fn publish_alias_real(dependency: &PublishDependency) -> Result<(String, String), Rejection> {
    if let Some(explicit) = &dependency.explicit_name_in_toml {
        let alias = explicit.clone();
        let real = first_nonempty(&[dependency.package.as_deref(), dependency.name.as_deref()])
            .unwrap_or(&alias)
            .to_string();
        return Ok((alias, real));
    }
    let name = dependency
        .name
        .clone()
        .ok_or_else(|| integrity("publish dependency missing name"))?;
    let real = first_nonempty(&[dependency.package.as_deref(), Some(name.as_str())])
        .unwrap_or(&name)
        .to_string();
    Ok((name, real))
}

fn first_nonempty<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values.iter().copied().flatten().find(|text| !text.is_empty())
}

fn group_packaged(dependencies: &[NormalizedDep]) -> Vec<(DependencyKey, Vec<usize>)> {
    let mut groups: Vec<(DependencyKey, Vec<usize>)> = Vec::new();
    for (index, dependency) in dependencies.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|(key, _)| *key == dependency.key)
        {
            Some((_, indexes)) => indexes.push(index),
            None => groups.push((dependency.key.clone(), vec![index])),
        }
    }
    groups
}

fn group_published(
    dependencies: &[(NormalizedDep, &PublishDependency)],
) -> Vec<(DependencyKey, Vec<usize>)> {
    let mut groups: Vec<(DependencyKey, Vec<usize>)> = Vec::new();
    for (index, (dependency, _)) in dependencies.iter().enumerate() {
        match groups
            .iter_mut()
            .find(|(key, _)| *key == dependency.key)
        {
            Some((_, indexes)) => indexes.push(index),
            None => groups.push((dependency.key.clone(), vec![index])),
        }
    }
    groups
}

fn kind_text(kind: Option<&DependencyKind>) -> String {
    match kind {
        None => "normal".to_string(),
        Some(kind) => publish_kind_text(Some(kind.as_str())),
    }
}

fn publish_kind_text(kind: Option<&str>) -> String {
    match kind {
        None | Some("") => "normal".to_string(),
        Some(text) => text.to_string(),
    }
}

fn normalize_target(target: Option<&str>) -> Option<String> {
    target.map(|text| {
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect()
    })
}

fn requirements_match(published: &str, packaged: &str) -> bool {
    if published == packaged {
        return true;
    }
    match (
        canonical_requirement(published),
        canonical_requirement(packaged),
    ) {
        (Ok(published), Ok(packaged)) => published == packaged,
        _ => false,
    }
}

fn match_registry(
    got: Option<&str>,
    expected: Option<&str>,
    alias: &str,
) -> Result<(), Rejection> {
    match (registry_of(got), registry_of(expected)) {
        (Registry::Same, Registry::Same) => Ok(()),
        (Registry::CratesIo, Registry::Same) => Ok(()),
        (Registry::Same, Registry::Foreign(_)) => Ok(()),
        (Registry::CratesIo, Registry::CratesIo) => Ok(()),
        (Registry::Foreign(got), Registry::Foreign(expected)) if got == expected => Err(integrity(
            format!("dependency '{alias}' uses unsupported registry '{got}'"),
        )),
        _ => Err(integrity(format!("dependency '{alias}' registry mismatch"))),
    }
}

fn registry_of(raw: Option<&str>) -> Registry<'_> {
    match raw {
        None | Some("") => Registry::Same,
        Some(CRATES_IO_INDEX) => Registry::CratesIo,
        Some(text) => Registry::Foreign(text),
    }
}

fn package_matches(
    raw: &PublishDependency,
    expected_package: Option<&str>,
    alias: &str,
) -> bool {
    if raw.package.as_deref() == expected_package {
        return true;
    }
    if raw.package.is_none() && expected_package == Some(alias) {
        return true;
    }
    if expected_package.is_none()
        && raw.package.as_deref() == Some(alias)
        && raw.explicit_name_in_toml.as_deref() == Some(alias)
    {
        return true;
    }
    let real = first_nonempty(&[
        raw.package.as_deref(),
        raw.name.as_deref(),
        raw.explicit_name_in_toml.as_deref(),
    ]);
    raw.package.is_none() && expected_package.is_some() && real == expected_package
}

fn target_repr(target: &Option<String>) -> String {
    match target {
        None => "None".to_string(),
        Some(text) => python_str_repr(text),
    }
}

fn python_str_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character == quote => {
                out.push('\\');
                out.push(character);
            }
            character if (character as u32) < 0x20 || (character as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", character as u32));
            }
            character => out.push(character),
        }
    }
    out.push(quote);
    out
}
