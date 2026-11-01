#![forbid(unsafe_code)]

use crate::model::{Catalog, Registry};

pub const DEFAULT_CARGO_ORIGIN: &str = "https://crates.17166969.xyz";
pub const DEFAULT_NPM_ORIGIN: &str = "https://npm.17166969.xyz";
pub const DEFAULT_PYPI_ORIGIN: &str = "https://pypi.17166969.xyz";
pub const DEFAULT_OCI_ORIGIN: &str = "https://oci.17166969.xyz";
pub const DEFAULT_UNIFIED_ORIGIN: &str = "https://stuff.17166969.xyz";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogOrigins {
    pub cargo: String,
    pub npm: String,
    pub pypi: String,
    pub oci: String,
    pub unified: String,
}

impl CatalogOrigins {
    pub fn new(
        cargo: impl Into<String>,
        npm: impl Into<String>,
        pypi: impl Into<String>,
        oci: impl Into<String>,
        unified: impl Into<String>,
    ) -> Self {
        Self {
            cargo: trim_origin(cargo.into()),
            npm: trim_origin(npm.into()),
            pypi: trim_origin(pypi.into()),
            oci: trim_origin(oci.into()),
            unified: trim_origin(unified.into()),
        }
    }

    pub fn for_registry(&self, registry: Registry) -> &str {
        match registry {
            Registry::Cargo => &self.cargo,
            Registry::Npm => &self.npm,
            Registry::Pypi => &self.pypi,
            Registry::Oci => &self.oci,
        }
    }
}

impl Default for CatalogOrigins {
    fn default() -> Self {
        Self::new(
            DEFAULT_CARGO_ORIGIN,
            DEFAULT_NPM_ORIGIN,
            DEFAULT_PYPI_ORIGIN,
            DEFAULT_OCI_ORIGIN,
            DEFAULT_UNIFIED_ORIGIN,
        )
    }
}

fn trim_origin(origin: String) -> String {
    origin.trim_end_matches('/').to_string()
}

fn registry_title(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => "Cargo registry",
        Registry::Npm => "npm registry",
        Registry::Pypi => "PyPI registry",
        Registry::Oci => "OCI registry",
    }
}

fn use_section(registry: Registry, generation: &str, origin: &str) -> String {
    let origin = origin.trim_end_matches('/');
    let catalog = format!("/generations/{generation}/catalog.json");
    let index = format!("/generations/{generation}/index.html");
    match registry {
        Registry::Cargo => format!(
            "Configure Cargo against the sparse index under `current/registry/`:\n\
            \n\
            ```toml\n\
            [source.crates-io]\n\
            replace-with = \"stuff\"\n\
            [source.stuff]\n\
            registry = \"sparse+{origin}/current/registry/\"\n\
            ```\n\
            \n\
            Index files live at `current/registry/<shard>` (sharded like the crates.io sparse index; mirrored upstream entries under `current/crates-io/`). \
            Config: [config.json](/generations/{generation}/registry/config.json). \
            Blobs: `blobs/<sha256>.crate` via the `dl` template. \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). \
            Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
        ),
        Registry::Npm => format!(
            "Point npm at the registry root under `current/`:\n\
            \n\
            ```\n\
            registry={origin}/current/\n\
            ```\n\
            \n\
            Packuments live at `current/<name>` (scoped names URL-encode `/` as `%2f`, e.g. `current/@scope%2fpkg`). \
            Tarballs live at `current/tarballs/<sha512>.tgz` (also `current/-/tarballs/<sha512>.tgz`). \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). \
            Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
        ),
        Registry::Pypi => format!(
            "Point pip at the PEP 503 simple index under `current/simple/`:\n\
            \n\
            ```\n\
            pip install --index-url {origin}/current/simple/ <package>\n\
            ```\n\
            \n\
            Indexes: [simple](/generations/{generation}/simple/index.html) with per-project pages at `current/simple/<project>/`. \
            Wheels live at `current/wheels/<filename>` (content-addressed by sha256). \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). \
            Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
        ),
        Registry::Oci => format!(
            "Point an OCI client at the registry root:\n\
            \n\
            ```\n\
            docker pull {host}/<repository>:<tag>\n\
            ```\n\
            \n\
            Manifests resolve at `current/repositories/<repository>/repository.json` through `/v2/<repository>/manifests/<tag|digest>`; \
            blobs are content-addressed by `sha256/<hex>`. \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). \
            Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`.",
            host = origin.trim_end_matches('/').trim_start_matches("https://").trim_start_matches("http://"),
        ),
    }
}

fn format_package_line(catalog: &Catalog, name: &str, latest: &str, versions: &[crate::model::VersionEntry], source: Option<&str>) -> String {
    let mut out = String::from("- `");
    out.push_str(name);
    out.push_str("` — latest `");
    out.push_str(latest);
    out.push_str("` (");
    out.push_str(&versions.len().to_string());
    out.push_str(if versions.len() == 1 { " version" } else { " versions" });
    out.push_str("): ");
    for (i, v) in versions.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('`');
        out.push_str(&v.version);
        out.push('`');
        if v.yanked {
            out.push_str(" (yanked)");
        }
    }
    if catalog.registry == Registry::Cargo
        && let Some(s) = source {
        out.push(' ');
        out.push('[');
        out.push_str(s);
        out.push(']');
    }
    let _ = catalog;
    out
}

pub fn render_llms_txt(catalog: &Catalog, origin: &str) -> String {
    let title = registry_title(catalog.registry);
    let origin = origin.trim_end_matches('/');
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(title);
    out.push('\n');
    out.push_str("> Static snapshot for LLMs. Generation `");
    out.push_str(&catalog.generation);
    out.push_str("`: ");
    out.push_str(&catalog.package_count().to_string());
    out.push_str(if catalog.package_count() == 1 { " package, " } else { " packages, " });
    out.push_str(&catalog.version_count().to_string());
    out.push_str(if catalog.version_count() == 1 { " version. " } else { " versions. " });
    out.push_str("Machine-readable inventory: [catalog.json](/generations/");
    out.push_str(&catalog.generation);
    out.push_str("/catalog.json). Human browser: [index.html](/generations/");
    out.push_str(&catalog.generation);
    out.push_str("/index.html).\n");
    out.push_str("\n## Registry\n\n");
    out.push_str("- Origin: ");
    out.push_str(origin);
    out.push_str("\n- Pinned generation: `generations/");
    out.push_str(&catalog.generation);
    out.push_str("/`\n- Inventory: [catalog.json](/generations/");
    out.push_str(&catalog.generation);
    out.push_str("/catalog.json)\n- Browser: [index.html](/generations/");
    out.push_str(&catalog.generation);
    out.push_str("/index.html)\n");
    out.push_str("\n## Use\n\n");
    out.push_str(&use_section(catalog.registry, &catalog.generation, origin));
    out.push_str("\n\n## Packages\n\n");
    if catalog.packages.is_empty() {
        out.push_str("No packages published in this generation.\n");
    } else {
        for package in &catalog.packages {
            out.push_str(&format_package_line(catalog, &package.name, &package.latest, &package.versions, package.source.as_deref()));
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

pub fn render_unified_llms_txt(catalogs: &[Catalog], origins: &CatalogOrigins) -> String {
    render_unified_llms_txt_with_notices(catalogs, origins, &[])
}

pub fn render_unified_llms_txt_with_notices(catalogs: &[Catalog], origins: &CatalogOrigins, notices: &[String]) -> String {
    let total_packages: usize = catalogs.iter().map(|c| c.package_count()).sum();
    let total_versions: usize = catalogs.iter().map(|c| c.version_count()).sum();
    let mut sorted: Vec<&Catalog> = catalogs.iter().collect();
    sorted.sort_by(|a, b| a.registry.as_str().cmp(b.registry.as_str()));
    let mut out = String::new();
    out.push_str("# stuff registries\n");
    out.push_str("> Aggregate Cargo + npm + PyPI + OCI snapshot for LLMs. ");
    out.push_str(&total_packages.to_string());
    out.push_str(if total_packages == 1 { " package, " } else { " packages, " });
    out.push_str(&total_versions.to_string());
    out.push_str(if total_versions == 1 { " version " } else { " versions " });
    out.push_str("across ");
    out.push_str(&sorted.len().to_string());
    out.push_str(" registries. Aggregate browser: ");
    out.push_str(origins.unified.trim_end_matches('/'));
    out.push_str("/ .\n");
    for notice in notices {
        out.push_str("> Note: ");
        out.push_str(notice);
        out.push('\n');
    }
    out.push_str("\n## Registries\n\n");
    for catalog in &sorted {
        out.push_str("- ");
        out.push_str(registry_title(catalog.registry));
        out.push_str(" (");
        out.push_str(origins.for_registry(catalog.registry).trim_end_matches('/'));
        out.push_str("): generation `");
        out.push_str(&catalog.generation);
        out.push_str("`, ");
        out.push_str(&catalog.package_count().to_string());
        out.push_str(if catalog.package_count() == 1 { " package, " } else { " packages, " });
        out.push_str(&catalog.version_count().to_string());
        out.push_str(if catalog.version_count() == 1 { " version" } else { " versions" });
        out.push('\n');
    }
    out.push_str("\n## Use\n\n");
    out.push_str("Per-registry `llms.txt` files live at the registry roots (`/llms.txt`, pinned under `current/llms.txt` and `generations/<id>/llms.txt`):\n\n");
    for catalog in &sorted {
        out.push_str("- ");
        out.push_str(match catalog.registry {
            Registry::Cargo => "Cargo",
            Registry::Npm => "npm",
            Registry::Pypi => "PyPI",
            Registry::Oci => "OCI",
        });
        out.push_str(": ");
        out.push_str(origins.for_registry(catalog.registry).trim_end_matches('/'));
        out.push_str("/llms.txt\n");
    }
    for catalog in &sorted {
        out.push_str("\n## ");
        out.push_str(registry_title(catalog.registry));
        out.push_str("\n\n");
        if catalog.packages.is_empty() {
            out.push_str("No packages published in this generation.\n");
        } else {
            for package in &catalog.packages {
                out.push_str(&format_package_line(catalog, &package.name, &package.latest, &package.versions, package.source.as_deref()));
                out.push('\n');
            }
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{cargo_catalog, npm_catalog, pypi_catalog, CargoInput, CargoSource, Dependencies, NpmInput, PypiInput};

    fn cargo_fixture() -> Catalog {
        cargo_catalog(
            "gen1",
            vec![
                CargoInput { name: "h12tiny".to_string(), version: "0.6.0".to_string(), yanked: false, source: CargoSource::Native, dependencies: Dependencies::default() },
                CargoInput { name: "h12tiny".to_string(), version: "0.5.0".to_string(), yanked: true, source: CargoSource::Native, dependencies: Dependencies::default() },
            ],
        )
        .unwrap()
    }

    #[test]
    fn per_registry_llms_txt_shape() {
        let text = render_llms_txt(&cargo_fixture(), DEFAULT_CARGO_ORIGIN);
        assert!(text.starts_with("# Cargo registry\n"));
        assert!(text.contains("> Static snapshot for LLMs. Generation `gen1`"));
        assert!(text.contains("[catalog.json](/generations/gen1/catalog.json)"));
        assert!(text.contains("## Use"));
        assert!(text.contains("## Packages"));
        assert!(text.contains("`h12tiny` — latest `0.6.0`"));
        assert!(text.contains("`0.5.0` (yanked)"));
        assert!(text.contains("sparse+https://crates.17166969.xyz/current/registry/"));
        let links: Vec<&str> = text
            .split("](")
            .skip(1)
            .filter_map(|rest| rest.split(')').next())
            .collect();
        assert!(!links.is_empty());
        for link in links {
            assert!(
                link.starts_with("/generations/gen1/"),
                "llms.txt link is not generation-pinned: {link}"
            );
        }
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn render_uses_each_registry_origin() {
        let cargo = render_llms_txt(&cargo_fixture(), "https://cargo.example.test/mirror/");
        assert!(cargo.contains("- Origin: https://cargo.example.test/mirror\n"));
        assert!(cargo.contains("sparse+https://cargo.example.test/mirror/current/registry/"));
        assert!(!cargo.contains("crates.17166969.xyz"));

        let npm = render_llms_txt(
            &npm_catalog(
                "g",
                vec![NpmInput { name: "@mine/utils".to_string(), version: "1.3.0".to_string(), dependencies: Dependencies::default() }],
                &std::collections::BTreeMap::new(),
            )
            .unwrap(),
            "https://npm.example.test",
        );
        assert!(npm.contains("registry=https://npm.example.test/current/"));
        assert!(!npm.contains("npm.17166969.xyz"));

        let pypi = render_llms_txt(
            &pypi_catalog(
                "g",
                vec![PypiInput { name: "foo-bar".to_string(), version: "1.0".to_string(), yanked: false, dependencies: Dependencies::default() }],
            )
            .unwrap(),
            "https://pypi.example.test",
        );
        assert!(pypi.contains("--index-url https://pypi.example.test/current/simple/"));
        assert!(!pypi.contains("pypi.17166969.xyz"));
    }

    #[test]
    fn npm_and_pypi_sections_differ() {
        let npm = npm_catalog(
            "g",
            vec![NpmInput { name: "@mine/utils".to_string(), version: "1.3.0".to_string(), dependencies: Dependencies::default() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let text = render_llms_txt(&npm, DEFAULT_NPM_ORIGIN);
        assert!(text.starts_with("# npm registry\n"));
        assert!(text.contains("registry=https://npm.17166969.xyz/current/"));
        assert!(text.contains("`@mine/utils`"));

        let pypi = pypi_catalog(
            "g",
            vec![PypiInput { name: "foo-bar".to_string(), version: "1.0".to_string(), yanked: false, dependencies: Dependencies::default() }],
        )
        .unwrap();
        let text = render_llms_txt(&pypi, DEFAULT_PYPI_ORIGIN);
        assert!(text.starts_with("# PyPI registry\n"));
        assert!(text.contains("--index-url https://pypi.17166969.xyz/current/simple/"));
    }

    #[test]
    fn empty_catalog_reports_no_packages() {
        let catalog = cargo_catalog("g", vec![]).unwrap();
        let text = render_llms_txt(&catalog, DEFAULT_CARGO_ORIGIN);
        assert!(text.contains("No packages published"));
    }

    #[test]
    fn unified_llms_txt_aggregates_registries() {
        let cargo = cargo_fixture();
        let npm = npm_catalog(
            "g2",
            vec![NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let text = render_unified_llms_txt(&[npm, cargo], &CatalogOrigins::default());
        assert!(text.starts_with("# stuff registries\n"));
        assert!(text.contains("## Registries"));
        assert!(text.contains("https://crates.17166969.xyz/llms.txt"));
        assert!(text.contains("## Cargo registry"));
        assert!(text.contains("## npm registry"));
        assert!(text.contains("`h12tiny`"));
        assert!(text.contains("`pkg`"));
    }

    #[test]
    fn unified_llms_txt_uses_configured_origins() {
        let cargo = cargo_fixture();
        let npm = npm_catalog(
            "g2",
            vec![NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let origins = CatalogOrigins::new(
            "https://cargo.example.test",
            "https://npm.example.test",
            "https://pypi.example.test",
            "https://oci.example.test",
            "https://stuff.example.test",
        );
        let text = render_unified_llms_txt(&[npm, cargo], &origins);
        assert!(text.contains("Aggregate browser: https://stuff.example.test/ ."));
        assert!(text.contains("Cargo registry (https://cargo.example.test)"));
        assert!(text.contains("- Cargo: https://cargo.example.test/llms.txt"));
        assert!(text.contains("- npm: https://npm.example.test/llms.txt"));
        assert!(!text.contains("- PyPI:"));
        assert!(!text.contains("17166969.xyz"));
    }

    #[test]
    fn unified_llms_txt_lists_notices_for_unreadable_registries() {
        let cargo = cargo_catalog("g", vec![]).unwrap();
        let text = render_unified_llms_txt_with_notices(
            &[cargo],
            &CatalogOrigins::default(),
            &["PyPI is not listed because its catalog could not be read.".to_string()],
        );
        assert!(text.contains("> Note: PyPI is not listed because its catalog could not be read.\n"));
        assert!(!render_unified_llms_txt(&[cargo_catalog("g", vec![]).unwrap()], &CatalogOrigins::default()).contains("> Note:"));
    }
}
