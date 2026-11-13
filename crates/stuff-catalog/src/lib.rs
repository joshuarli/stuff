#![forbid(unsafe_code)]

pub mod diff;
pub mod html;
pub mod identity;
pub mod llms;
pub mod model;
pub mod version;

pub use diff::{CatalogDiff, PackageDiff, PackageRef, diff_catalogs};
pub use html::{render_html, render_unified, render_unified_with_notices};
pub use identity::{FINGERPRINT_GENERATION, catalog_fingerprint, stored_catalog_fingerprint};
pub use llms::{
    render_llms_txt, render_unified_llms_txt, render_unified_llms_txt_with_notices, CatalogOrigins, DEFAULT_CARGO_ORIGIN,
    DEFAULT_MIRROR_ORIGIN, DEFAULT_NPM_ORIGIN, DEFAULT_OCI_ORIGIN, DEFAULT_PYPI_ORIGIN, DEFAULT_UNIFIED_ORIGIN,
};
pub use model::{
    canonical_project_name, cargo_catalog, npm_catalog, oci_catalog, parse_catalog_json, pypi_catalog,
    render_catalog_json, CargoInput, CargoSource, Catalog, Dependencies, DependencyKind, NpmInput, OciInput, Package, PackagesByName, PypiInput,
    Registry, VersionEntry,
};
pub use version::{compare_npm_version, compare_semver, is_stable_semver, parse_pep440};
