#![forbid(unsafe_code)]

pub mod diff;
pub mod html;
pub mod llms;
pub mod model;
pub mod version;

pub use diff::{CatalogDiff, diff_catalogs};
pub use html::{render_html, render_unified};
pub use llms::{render_llms_txt, render_unified_llms_txt};
pub use model::{
    canonical_project_name, cargo_catalog, npm_catalog, parse_catalog_json, pypi_catalog,
    render_catalog_json, CargoInput, CargoSource, Catalog, NpmInput, Package, PypiInput, Registry,
    VersionEntry,
};
pub use version::{compare_npm_version, compare_semver, is_stable_semver, parse_pep440};
