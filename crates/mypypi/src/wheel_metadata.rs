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
    fn matches_expected_dist_info() {
        let v = pep440_rs::Version::from_str("1.0").unwrap();
        assert!(matches_dist_info("validation-fixture", &v, "validation_fixture-1.0.dist-info"));
        assert!(!matches_dist_info("other", &v, "validation_fixture-1.0.dist-info"));
        assert!(!matches_dist_info("validation-fixture", &v, "validation_fixture-1.1.dist-info"));
        assert!(!matches_dist_info("validation-fixture", &v, "validation_fixture-1.0.data"));
    }

    #[test]
    fn identity_canonicalizes_name() {
        let id = metadata_identity("Pkg", "1.0").unwrap();
        assert_eq!(id.name, "pkg");
        assert!(metadata_identity("Pkg", "not-a-version").is_err());
    }
}
