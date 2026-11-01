#![forbid(unsafe_code)]

use crate::models::{canonicalize_name, PackageVersion};

/// METADATA `(name, version)` identity with PyPA name canonicalization.
/// Upstream wheels keep the project's original capitalization in `Name`
/// (e.g. Pygments) while filenames are normalized. Names are
/// case-insensitive, so parsing canonicalizes first; versions must already
/// be canonical. Callers enforce singleton presence with their own errors.
pub fn metadata_identity(name: &str, version: &str) -> Result<PackageVersion, String> {
    PackageVersion::parse(&canonicalize_name(name), version)
        .map_err(|_| "wheel has invalid package metadata".to_string())
}

/// Runtime `Requires-Dist` project names, skipping requirements gated on an extra.
pub fn requires_dist_names(metadata: &[u8]) -> Vec<String> {
    crate::validate::parse_headers(metadata)
        .remove("requires-dist")
        .unwrap_or_default()
        .iter()
        .filter(|requirement| !is_extra_requirement(requirement))
        .filter_map(|requirement| crate::validate::parse_requirement(requirement))
        .map(|requirement| requirement.name)
        .collect()
}

fn is_extra_requirement(requirement: &str) -> bool {
    requirement.split_once(';').is_some_and(|(_, marker)| {
        let marker = marker.replace(' ', "");
        marker.contains("extra==") || marker.contains("extra!=")
    })
}

pub fn matches_dist_info(package_name: &str, version: &pep440_rs::Version, directory: &str) -> bool {
    let suffix = ".dist-info";
    if !directory.ends_with(suffix) {
        return false;
    }
    let stem = &directory[..directory.len() - suffix.len()];
    let version_part = format!("-{}", version.to_string().replace('-', "_"));
    if stem.len() < version_part.len() {
        return false;
    }
    let (name_part, ver_part) = stem.split_at(stem.len() - version_part.len());
    if !ver_part.eq_ignore_ascii_case(&version_part) {
        return false;
    }
    canonicalize_name(name_part) == package_name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn requires_dist_names_skip_extras_and_keep_markers() {
        let metadata = b"Metadata-Version: 2.1\nName: Pkg\nVersion: 1.0\nRequires-Dist: Typing_Extensions (>=4)\nRequires-Dist: colorama ; sys_platform == \"win32\"\nRequires-Dist: pytest ; extra == \"test\"\nRequires-Dist: rich[jupyter]>=13 ; python_version >= \"3.9\" and extra != \"none\"\n\nbody Requires-Dist: ignored\n";
        assert_eq!(requires_dist_names(metadata), ["Typing_Extensions", "colorama"]);
    }

    #[test]
    fn matches_expected_dist_info() {
        let v = pep440_rs::Version::from_str("1.0").unwrap();
        assert!(matches_dist_info("validation-fixture", &v, "validation_fixture-1.0.dist-info"));
        assert!(!matches_dist_info("other", &v, "validation_fixture-1.0.dist-info"));
        assert!(!matches_dist_info("validation-fixture", &v, "validation_fixture-1.1.dist-info"));
        assert!(!matches_dist_info("validation-fixture", &v, "validation_fixture-1.0.data"));
    }
}
