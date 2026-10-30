#![forbid(unsafe_code)]

use crate::model::{Catalog, Registry};

fn registry_title(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => "Cargo registry",
        Registry::Npm => "npm registry",
        Registry::Pypi => "PyPI registry",
    }
}

fn registry_origin(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => "https://crates.17166969.xyz",
        Registry::Npm => "https://npm.17166969.xyz",
        Registry::Pypi => "https://pypi.17166969.xyz",
    }
}

fn use_section(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => concat!(
            "Configure Cargo against the sparse index under `current/registry/`:\n",
            "\n",
            "```toml\n",
            "[source.crates-io]\n",
            "replace-with = \"stuff\"\n",
            "[source.stuff]\n",
            "registry = \"sparse+https://crates.17166969.xyz/current/registry/\"\n",
            "```\n",
            "\n",
            "Index files live at `current/registry/<shard>` (sharded like the crates.io sparse index; mirrored upstream entries under `current/crates-io/`). ",
            "Config: [config.json](registry/config.json). ",
            "Blobs: `blobs/<sha256>.crate` via the `dl` template. ",
            "Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
        ),
        Registry::Npm => concat!(
            "Point npm at the registry root under `current/`:\n",
            "\n",
            "```\n",
            "registry=https://npm.17166969.xyz/current/\n",
            "```\n",
            "\n",
            "Packuments live at `current/<name>` (scoped names URL-encode `/` as `%2f`, e.g. `current/@scope%2fpkg`). ",
            "Tarballs live at `current/tarballs/<sha512>.tgz` (also `current/-/tarballs/<sha512>.tgz`). ",
            "Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
        ),
        Registry::Pypi => concat!(
            "Point pip at the PEP 503 simple index under `current/simple/`:\n",
            "\n",
            "```\n",
            "pip install --index-url https://pypi.17166969.xyz/current/simple/ <package>\n",
            "```\n",
            "\n",
            "Indexes: [simple](simple/index.html) with per-project pages at `current/simple/<project>/`. ",
            "Wheels live at `current/wheels/<filename>` (content-addressed by sha256). ",
            "Pin a generation with `current` (`current -> generations/<id>`); immutable snapshots live under `generations/<id>/`."
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

pub fn render_llms_txt(catalog: &Catalog) -> String {
    let title = registry_title(catalog.registry);
    let origin = registry_origin(catalog.registry);
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
    out.push_str("Machine-readable inventory: [catalog.json](catalog.json). Human browser: [index.html](index.html).\n");
    out.push_str("\n## Registry\n\n");
    out.push_str("- Origin: ");
    out.push_str(origin);
    out.push_str("\n- Pinned generation: `generations/");
    out.push_str(&catalog.generation);
    out.push_str("/`\n- Inventory: [catalog.json](catalog.json)\n- Browser: [index.html](index.html)\n");
    out.push_str("\n## Use\n\n");
    out.push_str(use_section(catalog.registry));
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

pub fn render_unified_llms_txt(catalogs: &[Catalog]) -> String {
    let total_packages: usize = catalogs.iter().map(|c| c.package_count()).sum();
    let total_versions: usize = catalogs.iter().map(|c| c.version_count()).sum();
    let mut sorted: Vec<&Catalog> = catalogs.iter().collect();
    sorted.sort_by(|a, b| a.registry.as_str().cmp(b.registry.as_str()));
    let mut out = String::new();
    out.push_str("# stuff registries\n");
    out.push_str("> Aggregate Cargo + npm + PyPI snapshot for LLMs. ");
    out.push_str(&total_packages.to_string());
    out.push_str(if total_packages == 1 { " package, " } else { " packages, " });
    out.push_str(&total_versions.to_string());
    out.push_str(if total_versions == 1 { " version " } else { " versions " });
    out.push_str("across ");
    out.push_str(&sorted.len().to_string());
    out.push_str(" registries. Aggregate browser: https://stuff.17166969.xyz/ .\n");
    out.push_str("\n## Registries\n\n");
    for catalog in &sorted {
        out.push_str("- ");
        out.push_str(registry_title(catalog.registry));
        out.push_str(" (");
        out.push_str(registry_origin(catalog.registry));
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
    out.push_str("- Cargo: https://crates.17166969.xyz/llms.txt\n");
    out.push_str("- npm: https://npm.17166969.xyz/llms.txt\n");
    out.push_str("- PyPI: https://pypi.17166969.xyz/llms.txt\n");
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
    use crate::model::{cargo_catalog, npm_catalog, pypi_catalog, CargoInput, CargoSource, NpmInput, PypiInput};

    fn cargo_fixture() -> Catalog {
        cargo_catalog(
            "gen1",
            vec![
                CargoInput { name: "h12tiny".to_string(), version: "0.6.0".to_string(), yanked: false, source: CargoSource::Native },
                CargoInput { name: "h12tiny".to_string(), version: "0.5.0".to_string(), yanked: true, source: CargoSource::Native },
            ],
        )
        .unwrap()
    }

    #[test]
    fn per_registry_llms_txt_shape() {
        let text = render_llms_txt(&cargo_fixture());
        assert!(text.starts_with("# Cargo registry\n"));
        assert!(text.contains("> Static snapshot for LLMs. Generation `gen1`"));
        assert!(text.contains("[catalog.json](catalog.json)"));
        assert!(text.contains("## Use"));
        assert!(text.contains("## Packages"));
        assert!(text.contains("`h12tiny` — latest `0.6.0`"));
        assert!(text.contains("`0.5.0` (yanked)"));
        assert!(text.contains("sparse+https://crates.17166969.xyz/current/registry/"));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn npm_and_pypi_sections_differ() {
        let npm = npm_catalog(
            "g",
            vec![NpmInput { name: "@mine/utils".to_string(), version: "1.3.0".to_string() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let text = render_llms_txt(&npm);
        assert!(text.starts_with("# npm registry\n"));
        assert!(text.contains("registry=https://npm.17166969.xyz/current/"));
        assert!(text.contains("`@mine/utils`"));

        let pypi = pypi_catalog(
            "g",
            vec![PypiInput { name: "foo-bar".to_string(), version: "1.0".to_string(), yanked: false }],
        )
        .unwrap();
        let text = render_llms_txt(&pypi);
        assert!(text.starts_with("# PyPI registry\n"));
        assert!(text.contains("--index-url https://pypi.17166969.xyz/current/simple/"));
    }

    #[test]
    fn empty_catalog_reports_no_packages() {
        let catalog = cargo_catalog("g", vec![]).unwrap();
        let text = render_llms_txt(&catalog);
        assert!(text.contains("No packages published"));
    }

    #[test]
    fn unified_llms_txt_aggregates_registries() {
        let cargo = cargo_fixture();
        let npm = npm_catalog(
            "g2",
            vec![NpmInput { name: "pkg".to_string(), version: "1.0.0".to_string() }],
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let text = render_unified_llms_txt(&[npm, cargo]);
        assert!(text.starts_with("# stuff registries\n"));
        assert!(text.contains("## Registries"));
        assert!(text.contains("https://crates.17166969.xyz/llms.txt"));
        assert!(text.contains("## Cargo registry"));
        assert!(text.contains("## npm registry"));
        assert!(text.contains("`h12tiny`"));
        assert!(text.contains("`pkg`"));
    }
}
