#![forbid(unsafe_code)]

use crate::model::{Catalog, Registry};

pub const DEFAULT_CARGO_ORIGIN: &str = "https://crates.17166969.xyz";
pub const DEFAULT_NPM_ORIGIN: &str = "https://npm.17166969.xyz";
pub const DEFAULT_PYPI_ORIGIN: &str = "https://pypi.17166969.xyz";
pub const DEFAULT_OCI_ORIGIN: &str = "https://oci.17166969.xyz";
pub const DEFAULT_MIRROR_ORIGIN: &str = "https://mirror.17166969.xyz";
pub const DEFAULT_UNIFIED_ORIGIN: &str = "https://stuff.17166969.xyz";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogOrigins {
    pub cargo: String,
    pub npm: String,
    pub pypi: String,
    pub oci: String,
    pub mirror: String,
    pub unified: String,
}

impl CatalogOrigins {
    pub fn new(
        cargo: impl Into<String>,
        npm: impl Into<String>,
        pypi: impl Into<String>,
        oci: impl Into<String>,
        mirror: impl Into<String>,
        unified: impl Into<String>,
    ) -> Self {
        Self {
            cargo: trim_origin(cargo.into()),
            npm: trim_origin(npm.into()),
            pypi: trim_origin(pypi.into()),
            oci: trim_origin(oci.into()),
            mirror: trim_origin(mirror.into()),
            unified: trim_origin(unified.into()),
        }
    }

    pub fn for_registry(&self, registry: Registry) -> &str {
        match registry {
            Registry::Cargo => &self.cargo,
            Registry::Npm => &self.npm,
            Registry::Pypi => &self.pypi,
            Registry::Oci => &self.oci,
            Registry::Mirror => &self.mirror,
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
            DEFAULT_MIRROR_ORIGIN,
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
        Registry::Mirror => "Static artifact mirror",
    }
}

/// How a client is pointed at one registry: a sentence with Markdown code
/// spans, then a snippet. Both the llms.txt and HTML pages print it.
pub(crate) struct Setup {
    pub lead: &'static str,
    pub fence: &'static str,
    pub code: String,
}

pub(crate) fn setup(registry: Registry, origin: &str) -> Setup {
    let origin = origin.trim_end_matches('/');
    match registry {
        Registry::Cargo => Setup {
            lead: "Configure Cargo against the sparse index under `current/registry/`:",
            fence: "toml",
            code: format!(
                "[source.crates-io]\nreplace-with = \"stuff\"\n[source.stuff]\nregistry = \"sparse+{origin}/current/registry/\""
            ),
        },
        Registry::Npm => Setup {
            lead: "Point npm at the registry root under `current/`:",
            fence: "",
            code: format!("registry={origin}/current/"),
        },
        Registry::Pypi => Setup {
            lead: "Point pip at the PEP 503 simple index under `current/simple/`:",
            fence: "",
            code: format!("pip install --index-url {origin}/current/simple/ <package>"),
        },
        Registry::Oci => Setup {
            lead: "Point an OCI client at the registry root:",
            fence: "",
            code: format!("docker pull {}/<repository>:<tag>", host_of(origin)),
        },
        Registry::Mirror => Setup {
            lead: "Download `<id>.zip` artifacts from `current/` and verify them against `current/SHA256SUMS`:",
            fence: "sh",
            code: format!(
                "curl -fLO {origin}/current/SHA256SUMS\ncurl -fLO {origin}/current/<id>.zip\nsha256sum --ignore-missing -c SHA256SUMS"
            ),
        },
    }
}

pub(crate) fn host_of(origin: &str) -> &str {
    origin.trim_end_matches('/').trim_start_matches("https://").trim_start_matches("http://")
}

fn use_section(registry: Registry, generation: &str, origin: &str) -> String {
    let catalog = format!("/generations/{generation}/catalog.json");
    let index = format!("/generations/{generation}/index.html");
    let pin = "Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`.";
    let details = match registry {
        Registry::Cargo => format!(
            "Index files live at `current/registry/<shard>` (sharded like the crates.io sparse index; mirrored upstream entries under `current/crates-io/`). \
            Config: [config.json](/generations/{generation}/registry/config.json). \
            Blobs: `blobs/<sha256>.crate` via the `dl` template. \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). {pin}"
        ),
        Registry::Npm => format!(
            "Packuments live at `current/<name>` (scoped names URL-encode `/` as `%2f`, e.g. `current/@scope%2fpkg`). \
            Tarballs live at `current/tarballs/<sha512>.tgz` (also `current/-/tarballs/<sha512>.tgz`). \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). {pin}"
        ),
        Registry::Pypi => format!(
            "Indexes: [simple](/generations/{generation}/simple/index.html) with per-project pages at `current/simple/<project>/`. \
            Wheels live at `current/wheels/<filename>` (content-addressed by sha256). \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). {pin}"
        ),
        Registry::Oci => format!(
            "Manifests resolve at `current/repositories/<repository>/repository.json` through `/v2/<repository>/manifests/<tag|digest>`; \
            blobs are content-addressed by `sha256/<hex>`. \
            Inventory: [catalog.json]({catalog}). Browser: [index.html]({index}). {pin}"
        ),
        Registry::Mirror => format!(
            "The pins (upstream URL + upstream SHA-256) live in `mirror/manifest.toml` in the stuff repo; the published zip SHA-256 values live in [SHA256SUMS](/generations/{generation}/SHA256SUMS). \n\
            Inventory: [catalog.json]({catalog}). \n\
            {pin}"
        ),
    };
    let Setup { lead, fence, code } = setup(registry, origin);
    format!("{lead}\n\n```{fence}\n{code}\n```\n\n{details}")
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
    out.push_str("> Aggregate registry snapshot for LLMs. ");
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
            Registry::Mirror => "Mirror",
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
                &rustc_hash::FxHashMap::default(),
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
            &rustc_hash::FxHashMap::default(),
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
    fn mirror_usage_explains_sums_verification() {
        let catalog = Catalog {
            schema_version: 1,
            registry: Registry::Mirror,
            generation: "gen1".to_string(),
            packages: vec![crate::model::Package {
                name: "node-v26.13.1-darwin-arm64".to_string(),
                source: Some("upstream".to_string()),
                latest: "26.13.1".to_string(),
                versions: vec![crate::model::VersionEntry { version: "26.13.1".to_string(), yanked: false }],
                dependencies: crate::model::Dependencies::default(),
            }],
        };
        let text = render_llms_txt(&catalog, DEFAULT_MIRROR_ORIGIN);
        assert!(text.starts_with("# Static artifact mirror\n"));
        assert!(text.contains("curl -fLO https://mirror.17166969.xyz/current/SHA256SUMS"));
        assert!(text.contains("sha256sum --ignore-missing -c SHA256SUMS"));
        assert!(text.contains("`node-v26.13.1-darwin-arm64`"));
        assert!(text.contains("mirror/manifest.toml"));
    }

    #[test]
    fn unified_llms_txt_aggregates_registries() {
        let cargo = cargo_fixture();
        let npm = npm_catalog(
            "g2",
            vec![NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() }],
            &rustc_hash::FxHashMap::default(),
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
            &rustc_hash::FxHashMap::default(),
        )
        .unwrap();
        let origins = CatalogOrigins::new(
            "https://cargo.example.test",
            "https://npm.example.test",
            "https://pypi.example.test",
            "https://oci.example.test",
            "https://mirror.example.test",
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
