#![forbid(unsafe_code)]

use crate::model::Catalog;

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

const CSS: &str = r#"
:root { color-scheme: light dark; }
body { font-family: system-ui, -apple-system, sans-serif; margin: 0 auto; max-width: 72rem; padding: 2rem 1rem; line-height: 1.5; }
header p { color: #666; }
.stats { display: flex; gap: 1rem; margin: 1.5rem 0; }
.stat { border: 1px solid #ddd; border-radius: 8px; padding: 0.75rem 1.25rem; }
.stat b { display: block; font-size: 1.5rem; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #e5e5e5; }
th { font-size: 0.8rem; text-transform: uppercase; letter-spacing: 0.04em; color: #666; }
code { font-family: ui-monospace, monospace; }
.yanked { color: #999; }
footer { margin-top: 2rem; font-size: 0.85rem; color: #666; }
"#;

fn registry_label(catalog: &Catalog) -> &'static str {
    match catalog.registry {
        crate::model::Registry::Cargo => "Cargo",
        crate::model::Registry::Npm => "npm",
        crate::model::Registry::Pypi => "PyPI",
    }
}

pub fn render_html(catalog: &Catalog) -> String {
    let label = registry_label(catalog);
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("<title>{} registry browser</title>\n<style>{CSS}</style>\n</head>\n<body>\n", escape_html(label)));
    out.push_str(&format!("<header><h1>{} registry</h1>\n", escape_html(label)));
    out.push_str(&format!(
        "<p>Generation <code>{}</code> &middot; {} packages &middot; {} versions</p></header>\n",
        escape_html(&catalog.generation),
        catalog.package_count(),
        catalog.version_count()
    ));
    out.push_str("<table>\n<thead><tr><th>Package</th><th>Source</th><th>Latest</th><th>Versions</th></tr></thead>\n<tbody>\n");
    for package in &catalog.packages {
        let versions: Vec<String> = package
            .versions
            .iter()
            .map(|v| {
                if v.yanked {
                    format!("{} (yanked)", v.version)
                } else {
                    v.version.clone()
                }
            })
            .collect();
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td><code>{}</code></td><td>{}</td></tr>\n",
            escape_html(&package.name),
            escape_html(package.source.as_deref().unwrap_or("-")),
            escape_html(&package.latest),
            escape_html(&versions.join(", "))
        ));
    }
    out.push_str("</tbody>\n</table>\n");
    out.push_str(&format!(
        "<footer><p>Machine-readable inventory: <a href=\"/generations/{}/catalog.json\"><code>catalog.json</code></a> &middot; schema version 1 &middot; generation <code>{}</code></p></footer>\n",
        escape_html(&catalog.generation),
        escape_html(&catalog.generation)
    ));
    out.push_str("</body>\n</html>\n");
    out
}

pub fn render_unified(catalogs: &[Catalog]) -> String {
    let total_packages: usize = catalogs.iter().map(|c| c.package_count()).sum();
    let total_versions: usize = catalogs.iter().map(|c| c.version_count()).sum();
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("<title>stuff registry browser</title>\n<style>{CSS}</style>\n</head>\n<body>\n"));
    out.push_str("<header><h1>stuff</h1><p>Registry browser</p></header>\n");
    out.push_str("<div class=\"stats\">\n");
    for catalog in catalogs {
        out.push_str(&format!(
            "<div class=\"stat\"><b>{}</b><span>{} packages</span></div>\n",
            catalog.package_count(),
            escape_html(registry_label(catalog))
        ));
    }
    out.push_str("</div>\n");
    out.push_str(&format!("<p>{total_packages} packages &middot; {total_versions} versions</p>\n"));
    out.push_str("<table>\n<thead><tr><th>Registry</th><th>Package</th><th>Source</th><th>Latest</th></tr></thead>\n<tbody>\n");
    for catalog in catalogs {
        for package in &catalog.packages {
            out.push_str(&format!(
                "<tr><td>{}</td><td><code>{}</code></td><td>{}</td><td><code>{}</code></td></tr>\n",
                escape_html(registry_label(catalog)),
                escape_html(&package.name),
                escape_html(package.source.as_deref().unwrap_or("-")),
                escape_html(&package.latest)
            ));
        }
    }
    out.push_str("</tbody>\n</table>\n</body>\n</html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CargoInput, CargoSource, cargo_catalog};

    #[test]
    fn registry_page_lists_packages_and_links_catalog() {
        let catalog = cargo_catalog(
            "gen1",
            vec![CargoInput {
                name: "h12tiny".to_string(),
                version: "0.6.0".to_string(),
                yanked: false,
                source: CargoSource::Native,
            }],
        )
        .unwrap();
        let html = render_html(&catalog);
        assert!(html.contains("h12tiny"));
        assert!(html.contains("catalog.json"));
        assert!(html.contains("gen1"));
        let hrefs: Vec<&str> = html
            .split("href=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert!(!hrefs.is_empty());
        for href in hrefs {
            assert!(
                href.starts_with("/generations/gen1/"),
                "public page link is not generation-pinned: {href}"
            );
        }
    }

    #[test]
    fn unified_page_aggregates_registries() {
        let catalog = cargo_catalog(
            "gen1",
            vec![CargoInput {
                name: "h12tiny".to_string(),
                version: "0.6.0".to_string(),
                yanked: false,
                source: CargoSource::Native,
            }],
        )
        .unwrap();
        let html = render_unified(std::slice::from_ref(&catalog));
        assert!(html.contains("stuff"));
        assert!(html.contains("h12tiny"));
    }
}
