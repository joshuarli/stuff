#![forbid(unsafe_code)]

use crate::model::Catalog;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackageRef {
    pub name: String,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageDiff {
    pub name: String,
    pub source: Option<String>,
    pub added_versions: Vec<String>,
    pub removed_versions: Vec<String>,
    pub yanked_now: Vec<String>,
    pub unyanked_now: Vec<String>,
    pub latest_before: Option<String>,
    pub latest_after: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogDiff {
    pub added_packages: Vec<PackageRef>,
    pub removed_packages: Vec<PackageRef>,
    pub changed_packages: Vec<PackageDiff>,
}

impl CatalogDiff {
    pub fn is_empty(&self) -> bool {
        self.added_packages.is_empty() && self.removed_packages.is_empty() && self.changed_packages.is_empty()
    }
}

fn package_ref(package: &crate::model::Package) -> PackageRef {
    PackageRef {
        name: package.name.clone(),
        source: package.source.clone(),
    }
}

pub fn diff_catalogs(before: &Catalog, after: &Catalog) -> Result<CatalogDiff, String> {
    if before.registry != after.registry {
        return Err(format!(
            "cannot diff {} catalog against {} catalog",
            before.registry.as_str(),
            after.registry.as_str()
        ));
    }
    let mut before_map: BTreeMap<PackageRef, &crate::model::Package> = BTreeMap::new();
    for p in &before.packages {
        before_map.insert(package_ref(p), p);
    }
    let mut after_map: BTreeMap<PackageRef, &crate::model::Package> = BTreeMap::new();
    for p in &after.packages {
        after_map.insert(package_ref(p), p);
    }
    let before_keys: BTreeSet<&PackageRef> = before_map.keys().collect();
    let after_keys: BTreeSet<&PackageRef> = after_map.keys().collect();
    let mut diff = CatalogDiff::default();
    for key in after_keys.difference(&before_keys) {
        diff.added_packages.push((*key).clone());
    }
    for key in before_keys.difference(&after_keys) {
        diff.removed_packages.push((*key).clone());
    }
    for key in before_keys.intersection(&after_keys) {
        let b = before_map[*key];
        let a = after_map[*key];
        let b_versions: BTreeMap<&str, bool> = b.versions.iter().map(|v| (v.version.as_str(), v.yanked)).collect();
        let a_versions: BTreeMap<&str, bool> = a.versions.iter().map(|v| (v.version.as_str(), v.yanked)).collect();
        let mut pkg = PackageDiff {
            name: key.name.clone(),
            source: key.source.clone(),
            added_versions: Vec::new(),
            removed_versions: Vec::new(),
            yanked_now: Vec::new(),
            unyanked_now: Vec::new(),
            latest_before: Some(b.latest.clone()),
            latest_after: Some(a.latest.clone()),
        };
        for v in a_versions.keys() {
            if !b_versions.contains_key(*v) {
                pkg.added_versions.push((*v).to_string());
            }
        }
        for v in b_versions.keys() {
            if !a_versions.contains_key(*v) {
                pkg.removed_versions.push((*v).to_string());
            }
        }
        for v in b_versions.keys().filter(|v| a_versions.contains_key(**v)) {
            match (b_versions[*v], a_versions[*v]) {
                (false, true) => pkg.yanked_now.push((*v).to_string()),
                (true, false) => pkg.unyanked_now.push((*v).to_string()),
                _ => {}
            }
        }
        if pkg.latest_before == pkg.latest_after {
            pkg.latest_before = None;
            pkg.latest_after = None;
        }
        if !pkg.added_versions.is_empty()
            || !pkg.removed_versions.is_empty()
            || !pkg.yanked_now.is_empty()
            || !pkg.unyanked_now.is_empty()
            || pkg.latest_before.is_some()
        {
            diff.changed_packages.push(pkg);
        }
    }
    diff.added_packages.sort();
    diff.removed_packages.sort();
    diff.changed_packages.sort_by(|a, b| (a.name.as_str(), a.source.as_deref()).cmp(&(b.name.as_str(), b.source.as_deref())));
    Ok(diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CargoInput, CargoSource, cargo_catalog};

    fn catalog_a() -> Catalog {
        cargo_catalog(
            "a",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() },
                CargoInput { name: "gone".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() },
            ],
        )
        .unwrap()
    }

    fn catalog_b() -> Catalog {
        cargo_catalog(
            "b",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: true, source: CargoSource::Native, dependencies: Vec::new() },
                CargoInput { name: "foo".to_string(), version: "1.1.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() },
                CargoInput { name: "new".to_string(), version: "0.1.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() },
            ],
        )
        .unwrap()
    }

    #[test]
    fn reports_added_removed_and_yank_changes() {
        let diff = diff_catalogs(&catalog_a(), &catalog_b()).unwrap();
        assert_eq!(diff.added_packages, vec![PackageRef { name: "new".to_string(), source: Some("native".to_string()) }]);
        assert_eq!(diff.removed_packages, vec![PackageRef { name: "gone".to_string(), source: Some("native".to_string()) }]);
        let foo = diff.changed_packages.iter().find(|p| p.name == "foo").unwrap();
        assert_eq!(foo.added_versions, vec!["1.1.0".to_string()]);
        assert_eq!(foo.yanked_now, vec!["1.0.0".to_string()]);
    }

    #[test]
    fn treats_same_name_from_different_sources_as_distinct() {
        let before = cargo_catalog(
            "a",
            vec![CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() }],
        )
        .unwrap();
        let after = cargo_catalog(
            "b",
            vec![
                CargoInput { name: "foo".to_string(), version: "1.0.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Vec::new() },
                CargoInput { name: "foo".to_string(), version: "2.0.0".to_string(), yanked: false, source: CargoSource::Upstream, dependencies: Vec::new() },
            ],
        )
        .unwrap();
        let diff = diff_catalogs(&before, &after).unwrap();
        assert_eq!(diff.added_packages, vec![PackageRef { name: "foo".to_string(), source: Some("upstream".to_string()) }]);
        assert!(diff.changed_packages.is_empty());
    }

    #[test]
    fn empty_diff_for_identical() {
        let a = catalog_a();
        let diff = diff_catalogs(&a, &catalog_a()).unwrap();
        assert!(diff.is_empty());
    }
}
