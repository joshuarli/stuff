#![forbid(unsafe_code)]

use crate::llms::{CatalogOrigins, host_of, setup};
use crate::model::{Catalog, DependencyKind, Package, PackagesByName, Registry};
use crate::ui::{
    self, NavLink, Tile, breadcrumbs, count, escape, group, inline_code, page_header, panel, plural, row, short_list,
    snippet, status_strip,
};
use rustc_hash::FxHashMap;

const SHORT_LIST: usize = 12;

fn registry_label(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => "Cargo",
        Registry::Npm => "npm",
        Registry::Pypi => "PyPI",
        Registry::Oci => "OCI",
        Registry::Mirror => "Mirror",
    }
}

fn section_id(registry: Registry) -> String {
    format!("r-{}", registry.as_str())
}

fn anchor(registry: Registry, package: &Package) -> String {
    match &package.source {
        Some(source) => format!("{}/{source}/{}", registry.as_str(), package.name),
        None => format!("{}/{}", registry.as_str(), package.name),
    }
}

fn fragment_href(anchor: &str) -> String {
    let mut out = String::from("#");
    for byte in anchor.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn package_link(registry: Registry, package: &Package) -> String {
    format!(
        "<a href=\"{}\">{}</a>",
        escape(&fragment_href(&anchor(registry, package))),
        escape(&package.name)
    )
}

fn is_published_here(registry: Registry, package: &Package) -> bool {
    registry == Registry::Cargo && package.source.as_deref() == Some("native")
}

/// The command that installs one package once the registry is set up, when
/// the registry has one that names a package.
fn install_command(registry: Registry, package: &Package, origins: Option<&CatalogOrigins>) -> Option<String> {
    match registry {
        Registry::Cargo => Some(format!("cargo add {}@{}", package.name, package.latest)),
        Registry::Npm => Some(format!("npm install {}@{}", package.name, package.latest)),
        Registry::Pypi => Some(match origins {
            Some(origins) => format!(
                "pip install --index-url {}/current/simple/ {}=={}",
                origins.pypi, package.name, package.latest
            ),
            None => format!("pip install {}=={}", package.name, package.latest),
        }),
        Registry::Oci => {
            origins.map(|origins| format!("docker pull {}/{}:{}", host_of(&origins.oci), package.name, package.latest))
        }
        Registry::Mirror => None,
    }
}

struct Dependent<'a> {
    package: &'a Package,
    kind: DependencyKind,
}

fn dependents_of<'a>(catalog: &'a Catalog, by_name: &PackagesByName<'a>) -> FxHashMap<String, Vec<Dependent<'a>>> {
    let mut dependents: FxHashMap<String, Vec<Dependent<'a>>> = FxHashMap::default();
    for package in &catalog.packages {
        let mut strongest: FxHashMap<String, DependencyKind> = FxHashMap::default();
        for (kind, name) in package.dependencies.iter() {
            if let Some(target) = by_name.resolve(name) {
                let slot = strongest.entry(anchor(catalog.registry, target)).or_insert(kind);
                *slot = (*slot).min(kind);
            }
        }
        for (target, kind) in strongest {
            dependents.entry(target).or_default().push(Dependent { package, kind });
        }
    }
    dependents
}

fn find_panel(catalogs: &[&Catalog]) -> String {
    let total: usize = catalogs.iter().map(|c| c.package_count()).sum();
    let mut rows = String::new();
    for catalog in catalogs {
        let label = registry_label(catalog.registry);
        for package in &catalog.packages {
            let note = match package.source.as_deref() {
                Some(source) => format!("{label} · {source}"),
                None => label.to_string(),
            };
            rows.push_str(&format!(
                "<li data-name=\"{}\" data-registry=\"{}\" data-source=\"{}\"><span class=\"row-name\">{}<span class=\"row-note\">{}</span></span><span class=\"row-value\">{}</span></li>",
                escape(&package.name),
                catalog.registry.as_str(),
                escape(package.source.as_deref().unwrap_or("")),
                package_link(catalog.registry, package),
                escape(&note),
                escape(&package.latest)
            ));
        }
    }
    let body = format!(
        "<div class=\"find\">\
<label class=\"ui-search\" data-package-search-box hidden>{icon}<input type=\"search\" data-package-search placeholder=\"Find a package by name\" aria-label=\"Find a package\" autocomplete=\"off\"></label>\
<details class=\"browse\" data-browse><summary>Browse all {total}</summary><ol class=\"rows\" data-packages>{rows}</ol></details>\
<p class=\"note\" data-no-match hidden>No package matches.</p>\
<button class=\"ui-btn\" type=\"button\" data-more hidden></button>\
</div>",
        icon = ui::SEARCH_ICON,
        total = plural(total, "package", "packages"),
    );
    panel(Some("find"), "Find a package", &plural(total, "package", "packages"), &body)
}

fn published_here_panel(catalog: &Catalog) -> String {
    let rows: Vec<String> = catalog
        .packages
        .iter()
        .filter(|p| is_published_here(catalog.registry, p))
        .map(|p| row(&package_link(catalog.registry, p), "", &escape(&p.latest)))
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let mirrored = catalog.package_count() - rows.len();
    panel(
        Some("published"),
        &format!("Published here · {}", count(rows.len())),
        &format!("{} mirrored", count(mirrored)),
        &format!("<div class=\"columns\">{}</div>", short_list(&rows, 24)),
    )
}

fn version_note(catalog: &Catalog) -> String {
    plural(catalog.version_count(), "version", "versions")
}

fn registry_panel(catalog: &Catalog, origins: Option<&CatalogOrigins>) -> String {
    let label = registry_label(catalog.registry);
    let generation = escape(&catalog.generation);
    let mut body = String::new();
    let base = origins.map(|o| o.for_registry(catalog.registry)).unwrap_or("");
    // A sealed page may name its generation only in full: verification maps the full id back to the fingerprint.
    match origins {
        Some(origins) => {
            let setup = setup(catalog.registry, origins.for_registry(catalog.registry));
            body.push_str(&format!("<p class=\"lead\">{}</p>{}", inline_code(setup.lead), snippet(&setup.code)));
        }
        None => body.push_str(&format!(
            "<p class=\"lead\">Setup for this registry, with the client configuration, is in <a href=\"/generations/{generation}/llms.txt\">llms.txt</a>.</p>"
        )),
    }
    match origins {
        Some(_) => body.push_str(&format!(
            "<p class=\"panel-foot\">Generation <code>{short}</code> · <a href=\"{base}/generations/{generation}/catalog.json\">catalog.json</a></p>",
            short = escape(catalog.generation.get(..12).unwrap_or(&catalog.generation)),
            base = escape(base),
        )),
        None => body.push_str(&format!(
            "<p class=\"panel-foot\"><a href=\"/generations/{generation}/catalog.json\">catalog.json</a></p>"
        )),
    }
    match origins {
        Some(_) => panel(
            Some(&section_id(catalog.registry)),
            label,
            &plural(catalog.package_count(), "package", "packages"),
            &body,
        ),
        None => panel(Some(&section_id(catalog.registry)), "Setup", "", &body),
    }
}

fn name_rows(catalog: &Catalog, by_name: &PackagesByName<'_>, names: &[String]) -> String {
    let mut hosted = Vec::new();
    let mut elsewhere = Vec::new();
    for name in names {
        match by_name.resolve(name) {
            Some(target) => hosted.push(format!("<li>{}</li>", package_link(catalog.registry, target))),
            None => elsewhere.push(format!("<code>{}</code>", escape(name))),
        }
    }
    let mut out = String::new();
    if !hosted.is_empty() {
        out.push_str(&short_list(&hosted, SHORT_LIST));
    }
    if !elsewhere.is_empty() {
        out.push_str(&format!("<p class=\"note\">Not hosted here: {}</p>", elsewhere.join(", ")));
    }
    out
}

fn kind_title(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Runtime => "Runtime",
        DependencyKind::Peer => "Peer",
        DependencyKind::Build => "Build",
        DependencyKind::Dev => "Dev",
    }
}

fn depends_on_panel(catalog: &Catalog, by_name: &PackagesByName<'_>, package: &Package) -> String {
    let mut body = String::new();
    if package.dependencies.runtime.is_empty() {
        body.push_str(&format!("<p class=\"note\">No runtime dependencies in {}.</p>", escape(&package.latest)));
    } else {
        body.push_str(&name_rows(catalog, by_name, &package.dependencies.runtime));
    }
    if catalog.schema_version < 3 {
        body.push_str("<p class=\"note\">Peer, build and dev dependencies were not recorded for this generation.</p>");
    } else {
        for kind in [DependencyKind::Peer, DependencyKind::Build, DependencyKind::Dev] {
            let names = package.dependencies.of(kind);
            if !names.is_empty() {
                body.push_str(&group(
                    &format!("{} · {}", kind_title(kind), names.len()),
                    &name_rows(catalog, by_name, names),
                ));
            }
        }
    }
    let total: usize = DependencyKind::ALL.iter().map(|k| package.dependencies.of(*k).len()).sum();
    panel(None, "Depends on", &plural(total, "package", "packages"), &body)
}

fn used_by_panel(catalog: &Catalog, users: Option<&Vec<Dependent<'_>>>) -> String {
    let users = users.map(Vec::as_slice).unwrap_or(&[]);
    let body = if users.is_empty() {
        "<p class=\"note\">No package here depends on it.</p>".to_string()
    } else {
        let rows: Vec<String> = users
            .iter()
            .map(|user| {
                let kind = match user.kind {
                    DependencyKind::Runtime => String::new(),
                    kind => format!("<span class=\"row-note\">{}</span>", kind.as_str()),
                };
                format!("<li>{}{kind}</li>", package_link(catalog.registry, user.package))
            })
            .collect();
        short_list(&rows, SHORT_LIST)
    };
    panel(None, "Used by", &plural(users.len(), "package", "packages"), &body)
}

fn versions_panel(package: &Package) -> String {
    let rows: Vec<String> = package
        .versions
        .iter()
        .rev()
        .map(|v| {
            let state = if v.yanked {
                "<span class=\"state state-bad\">Yanked</span>"
            } else if v.version == package.latest {
                "<span class=\"row-note\">latest</span>"
            } else {
                ""
            };
            format!("<li><span class=\"mono\">{}</span>{state}</li>", escape(&v.version))
        })
        .collect();
    let yanked = package.versions.iter().filter(|v| v.yanked).count();
    let mut meta = plural(package.versions.len(), "version", "versions");
    if yanked > 0 {
        meta.push_str(&format!(", {yanked} yanked"));
    }
    panel(None, "Versions", &meta, &short_list(&rows, 8))
}

/// Package views; the unified page passes `origins` and places each registry under "stuff".
fn detail_sections(catalogs: &[&Catalog], origins: Option<&CatalogOrigins>) -> String {
    let mut out = String::new();
    for catalog in catalogs {
        let by_name = catalog.packages_by_name();
        let dependents = dependents_of(catalog, &by_name);
        let label = registry_label(catalog.registry);
        let registry_href = format!("#{}", section_id(catalog.registry));
        for package in &catalog.packages {
            let id = anchor(catalog.registry, package);
            out.push_str(&format!("<section class=\"detail page\" id=\"{}\">\n", escape(&id)));
            match origins {
                Some(_) => out.push_str(&breadcrumbs(&[("#", "stuff"), (&registry_href, label)], &package.name)),
                None => out.push_str(&breadcrumbs(&[("#", label)], &package.name)),
            }
            let source = package.source.as_deref().map(|s| format!(" · {}", escape(s))).unwrap_or_default();
            out.push_str(&page_header(
                &package.name,
                &format!("{label}{source} · latest <b>{}</b>", escape(&package.latest)),
            ));
            if let Some(command) = install_command(catalog.registry, package, origins) {
                out.push_str(&format!("<div class=\"install\">{}</div>\n", snippet(&command)));
            }
            out.push_str("<div class=\"ui-dashboard-grid\">\n");
            out.push_str(&versions_panel(package));
            if catalog.registry == Registry::Oci {
                out.push_str(&panel(
                    None,
                    "Tags",
                    "",
                    "<p class=\"note\">Tags resolve to OCI manifests through the registry API.</p>",
                ));
            } else if catalog.schema_version < 2 {
                out.push_str(&panel(
                    None,
                    "Dependencies",
                    "not recorded",
                    "<p class=\"note\">This generation was published before dependencies were recorded.</p>",
                ));
            } else {
                out.push_str(&depends_on_panel(catalog, &by_name, package));
                out.push_str(&used_by_panel(catalog, dependents.get(&id)));
            }
            out.push_str("</div>\n</section>\n");
        }
    }
    out
}

fn registry_tile(catalog: &Catalog) -> Tile {
    Tile {
        label: registry_label(catalog.registry).to_string(),
        value: count(catalog.package_count()),
        note: version_note(catalog),
        href: format!("#{}", section_id(catalog.registry)),
    }
}

pub fn render_html(catalog: &Catalog) -> String {
    let label = registry_label(catalog.registry);
    let mut page = String::from("<div class=\"index\">\n");
    page.push_str(&page_header(
        &format!("{label} registry"),
        &format!(
            "{} packages served as static files from generation <code>{}</code>.",
            count(catalog.package_count()),
            escape(&catalog.generation)
        ),
    ));
    let mut tiles = vec![Tile {
        label: "Packages".to_string(),
        value: count(catalog.package_count()),
        note: version_note(catalog),
        href: "#find".to_string(),
    }];
    let native = catalog.packages.iter().filter(|p| is_published_here(catalog.registry, p)).count();
    if native > 0 {
        tiles.push(Tile {
            label: "Published here".to_string(),
            value: count(native),
            note: format!("{} mirrored", count(catalog.package_count() - native)),
            href: "#published".to_string(),
        });
    }
    page.push_str(&status_strip(&tiles));
    page.push_str(&find_panel(&[catalog]));
    page.push_str(&published_here_panel(catalog));
    page.push_str(&registry_panel(catalog, None));
    page.push_str("</div>\n");
    page.push_str(&detail_sections(&[catalog], None));
    let section = format!("#{}", section_id(catalog.registry));
    let mut nav = vec![NavLink { href: "#find", label: "Find" }];
    if native > 0 {
        nav.push(NavLink { href: "#published", label: "Published here" });
    }
    nav.push(NavLink { href: &section, label: "Setup" });
    ui::document(&format!("{label} registry browser"), label, &nav, &page)
}

pub fn render_unified(catalogs: &[Catalog], origins: &CatalogOrigins) -> String {
    render_unified_with_notices(catalogs, origins, &[])
}

pub fn render_unified_with_notices(catalogs: &[Catalog], origins: &CatalogOrigins, notices: &[String]) -> String {
    let mut page = String::from("<div class=\"index\">\n");
    page.push_str(&page_header(
        "stuff",
        "Cargo, npm, PyPI and OCI packages, served as static files. Each publish creates a new generation; the current one is live.",
    ));
    for notice in notices {
        page.push_str(&format!("<p class=\"ui-notice ui-notice-bad\" role=\"status\">{}</p>\n", escape(notice)));
    }
    let tiles: Vec<Tile> = catalogs.iter().map(registry_tile).collect();
    page.push_str(&status_strip(&tiles));
    let all: Vec<&Catalog> = catalogs.iter().collect();
    page.push_str(&find_panel(&all));
    for catalog in catalogs {
        page.push_str(&published_here_panel(catalog));
    }
    page.push_str("<div class=\"ui-dashboard-grid setup\">\n");
    for catalog in catalogs {
        page.push_str(&registry_panel(catalog, Some(origins)));
    }
    page.push_str("</div>\n</div>\n");
    page.push_str(&detail_sections(&all, Some(origins)));
    let sections: Vec<(String, &str)> =
        catalogs.iter().map(|c| (format!("#{}", section_id(c.registry)), registry_label(c.registry))).collect();
    let mut nav = vec![NavLink { href: "#find", label: "Find" }];
    if catalogs.iter().any(|c| c.packages.iter().any(|p| is_published_here(c.registry, p))) {
        nav.push(NavLink { href: "#published", label: "Published here" });
    }
    nav.extend(sections.iter().map(|(href, label)| NavLink { href, label }));
    ui::document("stuff registry browser", "stuff", &nav, &page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CargoInput, CargoSource, Dependencies, NpmInput, cargo_catalog, npm_catalog};
    use rustc_hash::FxHashMap;

    fn cargo_input(name: &str, version: &str, dependencies: &[&str]) -> CargoInput {
        CargoInput {
            name: name.to_string(),
            version: version.to_string(),
            yanked: false,
            source: CargoSource::Native,
            dependencies: Dependencies::runtime_only(dependencies.iter().map(|d| d.to_string()).collect()),
        }
    }

    fn upstream(name: &str, version: &str) -> CargoInput {
        CargoInput { source: CargoSource::Upstream, ..cargo_input(name, version, &[]) }
    }

    fn npm_input(name: &str, dependencies: &[&str]) -> NpmInput {
        NpmInput {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            dependencies: Dependencies::runtime_only(dependencies.iter().map(|d| d.to_string()).collect()),
        }
    }

    fn section<'a>(html: &'a str, id: &str) -> &'a str {
        let start = html.find(&format!("<section class=\"detail page\" id=\"{id}\">")).expect("detail section");
        let rest = &html[start..];
        &rest[..rest.find("</div>\n</section>\n").expect("section end")]
    }

    fn panel_body<'a>(section: &'a str, title: &str) -> &'a str {
        let start = section.find(&format!("<h2>{title}</h2>")).expect("panel");
        let rest = &section[start..];
        &rest[..rest.find("</section>").unwrap_or(rest.len())]
    }

    fn hrefs(html: &str) -> Vec<&str> {
        html.split("href=\"").skip(1).filter_map(|rest| rest.split('"').next()).collect()
    }

    #[test]
    fn registry_page_links_stay_in_its_generation() {
        let catalog = cargo_catalog("gen1", vec![cargo_input("h12tiny", "0.6.0", &[])]).unwrap();
        let html = render_html(&catalog);
        assert!(html.contains("h12tiny"));
        assert!(html.contains("href=\"/generations/gen1/catalog.json\""));
        assert!(html.contains("href=\"/generations/gen1/llms.txt\""));
        let hrefs = hrefs(&html);
        assert!(!hrefs.is_empty());
        for href in hrefs {
            assert!(
                href.starts_with("/generations/gen1/") || href.starts_with('#'),
                "public page link is neither generation-pinned nor in-page: {href}"
            );
        }
    }

    #[test]
    fn pages_carry_a_search_box_that_only_scripts_reveal() {
        let catalog = npm_catalog("gen1", vec![npm_input("@scope/a", &[])], &FxHashMap::default()).unwrap();
        for html in [render_html(&catalog), render_unified(std::slice::from_ref(&catalog), &CatalogOrigins::default())] {
            assert!(html.contains("<label class=\"ui-search\" data-package-search-box hidden>"));
            assert!(html.contains("<input type=\"search\" data-package-search"));
            assert!(html.contains("<ol class=\"rows\" data-packages>"));
            assert!(html.contains("<li data-name=\"@scope/a\" data-registry=\"npm\" data-source=\"\">"));
            assert!(html.contains("data-no-match hidden"));
            assert!(html.contains("data-more hidden"));
            assert_eq!(html.matches("<script>").count(), 1);
            assert!(html.contains("<div class=\"ui-shell\">"));
        }
    }

    #[test]
    fn unified_page_shows_notices_for_registries_it_could_not_read() {
        let catalog = cargo_catalog("gen1", vec![cargo_input("h12tiny", "0.6.0", &[])]).unwrap();
        let origins = CatalogOrigins::default();
        let html = render_unified_with_notices(
            std::slice::from_ref(&catalog),
            &origins,
            &["PyPI is not listed <because> its catalog could not be read.".to_string()],
        );
        assert!(html.contains(
            "<p class=\"ui-notice ui-notice-bad\" role=\"status\">PyPI is not listed &lt;because&gt; its catalog could not be read.</p>"
        ));
        assert!(html.contains("h12tiny"));
        assert!(!render_unified(std::slice::from_ref(&catalog), &origins).contains("<p class=\"ui-notice"));
    }

    #[test]
    fn unified_page_has_one_tile_and_one_setup_panel_per_registry() {
        let cargo = cargo_catalog("cargo-gen", vec![cargo_input("mine", "1.0.0", &[]), upstream("serde", "1.0.0")]).unwrap();
        let npm = npm_catalog("npm-gen", vec![npm_input("a", &[]), npm_input("b", &[])], &FxHashMap::default()).unwrap();
        let origins = CatalogOrigins::new("https://c.test", "https://n.test", "https://p.test", "https://o.test", "https://m.test", "https://u.test");
        let html = render_unified(&[cargo, npm], &origins);
        assert_eq!(html.matches("<a class=\"tile\"").count(), 2);
        assert!(html.contains("<a class=\"tile\" href=\"#r-cargo\"><span class=\"tile-label\">Cargo</span><span class=\"tile-value\">2</span><span class=\"tile-note\">2 versions</span></a>"));
        assert!(html.contains("<a class=\"tile\" href=\"#r-npm\"><span class=\"tile-label\">npm</span><span class=\"tile-value\">2</span>"));
        let cargo_panel = &html[html.find("id=\"r-cargo\"").unwrap()..];
        assert!(cargo_panel.contains("registry = &quot;sparse+https://c.test/current/registry/&quot;"));
        assert!(cargo_panel.contains("href=\"https://c.test/generations/cargo-gen/catalog.json\""));
        let npm_panel = &html[html.find("id=\"r-npm\"").unwrap()..];
        assert!(npm_panel.contains("registry=https://n.test/current/"));
        assert!(html.contains("<a href=\"#r-npm\">npm</a>"), "the sidebar links each registry");
    }

    #[test]
    fn only_crates_published_here_are_listed_and_mirrors_are_counted() {
        let catalog = cargo_catalog(
            "g",
            vec![cargo_input("mine", "1.0.0", &[]), upstream("serde", "1.0.0"), upstream("rand", "0.9.0")],
        )
        .unwrap();
        let html = render_html(&catalog);
        let panel = &html[html.find("id=\"published\"").unwrap()..];
        let panel = &panel[..panel.find("</section>").unwrap()];
        assert!(panel.contains("<h2>Published here · 1</h2></div><span class=\"ui-panel-meta\">2 mirrored</span>"));
        assert!(panel.contains("href=\"#cargo/native/mine\""));
        assert!(!panel.contains("serde"));
        assert!(html.contains("<span class=\"tile-label\">Published here</span><span class=\"tile-value\">1</span><span class=\"tile-note\">2 mirrored</span>"));

        let npm = npm_catalog("g", vec![npm_input("a", &[])], &FxHashMap::default()).unwrap();
        assert!(!render_html(&npm).contains("Published here"));
    }

    #[test]
    fn long_lists_show_a_short_head_and_reveal_the_tail() {
        let names: Vec<String> = (0..20).map(|i| format!("lib{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut inputs = vec![cargo_input("app", "1.0.0", &refs)];
        inputs.extend(names.iter().map(|name| cargo_input(name, "1.0.0", &[])));
        let html = render_html(&cargo_catalog("g", inputs).unwrap());
        let depends = panel_body(section(&html, "cargo/native/app"), "Depends on");
        assert_eq!(depends.matches("<li>").count(), 20);
        assert!(depends.contains("<summary>Show 8 more</summary>"));
    }

    #[test]
    fn package_detail_links_dependencies_and_dependents() {
        let catalog = cargo_catalog(
            "gen1",
            vec![
                cargo_input("app", "1.0.0", &["lib", "outside"]),
                cargo_input("lib", "2.0.0", &[]),
                cargo_input("tool", "0.1.0", &["lib"]),
            ],
        )
        .unwrap();
        let html = render_html(&catalog);

        let app = section(&html, "cargo/native/app");
        assert!(app.contains("href=\"#cargo/native/lib\""));
        assert!(app.contains("Not hosted here: <code>outside</code>"));
        assert!(!app.contains("href=\"#cargo/native/outside\""));
        assert!(app.contains("No package here depends on it."));
        assert!(app.contains("<ol><li><a href=\"#\">Cargo</a>"), "breadcrumbs lead back to the registry");
        let unified = render_unified(std::slice::from_ref(&catalog), &CatalogOrigins::default());
        assert!(section(&unified, "cargo/native/app").contains("<ol><li><a href=\"#\">stuff</a><span class=\"ui-breadcrumb-separator\" aria-hidden=\"true\">/</span></li><li><a href=\"#r-cargo\">Cargo</a>"));

        let lib = section(&html, "cargo/native/lib");
        assert!(panel_body(lib, "Used by").contains("href=\"#cargo/native/app\""));
        assert!(panel_body(lib, "Used by").contains("href=\"#cargo/native/tool\""));
        assert!(lib.contains("No runtime dependencies in 2.0.0."));
    }

    #[test]
    fn install_commands_name_the_latest_version() {
        let cargo = cargo_catalog("g", vec![cargo_input("app", "1.2.0", &[])]).unwrap();
        let npm = npm_catalog("g", vec![npm_input("@scope/a", &[])], &FxHashMap::default()).unwrap();
        let origins = CatalogOrigins::default();
        let html = render_unified(&[cargo.clone(), npm], &origins);
        assert!(section(&html, "cargo/native/app").contains("<code>cargo add app@1.2.0</code>"));
        assert!(section(&html, "npm/@scope/a").contains("<code>npm install @scope/a@1.0.0</code>"));
        assert!(section(&render_html(&cargo), "cargo/native/app").contains("<code>cargo add app@1.2.0</code>"));
    }

    #[test]
    fn non_runtime_kinds_get_their_own_groups_and_labels() {
        let mut app = cargo_input("app", "1.0.0", &["lib"]);
        app.dependencies.dev = vec!["helper".to_string(), "outside-dev".to_string()];
        app.dependencies.build = vec!["lib".to_string()];
        app.dependencies.peer = vec!["outside-peer".to_string()];
        let catalog = cargo_catalog(
            "gen1",
            vec![app, cargo_input("lib", "2.0.0", &[]), cargo_input("helper", "0.1.0", &[])],
        )
        .unwrap();
        let html = render_html(&catalog);

        let app = section(&html, "cargo/native/app");
        assert!(app.contains("<h3>Peer · 1</h3>"));
        assert!(app.contains("<h3>Build · 1</h3>"));
        assert!(app.contains("<h3>Dev · 2</h3>"));
        assert!(app.contains("Not hosted here: <code>outside-dev</code>"));
        assert!(app.contains("href=\"#cargo/native/helper\""));

        let lib = section(&html, "cargo/native/lib");
        let used_by = panel_body(lib, "Used by");
        assert!(used_by.contains("href=\"#cargo/native/app\""));
        assert!(!used_by.contains(">build<"), "a runtime use outranks a build use");

        let helper = section(&html, "cargo/native/helper");
        assert!(panel_body(helper, "Used by").contains("<li><a href=\"#cargo/native/app\">app</a><span class=\"row-note\">dev</span></li>"));
        assert!(!lib.contains("<h3>"), "kinds with no names get no group");
    }

    #[test]
    fn schema_two_catalogs_do_not_claim_other_kinds_are_empty() {
        let text = r#"{"schema_version":2,"registry":"cargo","generation":"g","packages":[{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}],"dependencies":["b"]}]}"#;
        let html = render_html(&crate::model::parse_catalog_json(text).unwrap());
        let detail = section(&html, "cargo/native/a");
        assert!(detail.contains("Peer, build and dev dependencies were not recorded"));
        assert!(detail.contains("Not hosted here: <code>b</code>"));
    }

    #[test]
    fn scoped_npm_names_keep_a_working_fragment() {
        let catalog = npm_catalog(
            "gen1",
            vec![npm_input("@scope/a", &["@scope/b"]), npm_input("@scope/b", &[])],
            &FxHashMap::default(),
        )
        .unwrap();
        let html = render_html(&catalog);
        assert!(section(&html, "npm/@scope/a").contains("href=\"#npm/@scope/b\""));
        assert!(section(&html, "npm/@scope/b").contains("href=\"#npm/@scope/a\""));
    }

    #[test]
    fn dependencies_never_cross_registries() {
        let cargo = cargo_catalog("g", vec![cargo_input("shared", "1.0.0", &[])]).unwrap();
        let npm = npm_catalog("g", vec![npm_input("shared", &[]), npm_input("user", &["shared"])], &FxHashMap::default()).unwrap();
        let html = render_unified(&[cargo, npm], &CatalogOrigins::default());
        assert!(section(&html, "cargo/native/shared").contains("No package here depends on it."));
        assert!(section(&html, "npm/shared").contains("href=\"#npm/user\""));
    }

    #[test]
    fn schema_one_catalogs_do_not_claim_empty_dependencies() {
        let text = r#"{"schema_version":1,"registry":"cargo","generation":"g","packages":[{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}]}]}"#;
        let html = render_html(&crate::model::parse_catalog_json(text).unwrap());
        let detail = section(&html, "cargo/native/a");
        assert!(detail.contains("published before dependencies were recorded"));
        assert!(!detail.contains("No runtime dependencies"));
        assert!(!detail.contains("Used by"));
    }

    #[test]
    fn versions_list_newest_first_and_name_yanked_ones() {
        let mut old = cargo_input("a", "1.0.0", &[]);
        old.yanked = true;
        let catalog = cargo_catalog("g", vec![old, cargo_input("a", "1.1.0", &[])]).unwrap();
        let html = render_html(&catalog);
        let versions = panel_body(section(&html, "cargo/native/a"), "Versions");
        assert!(versions.contains("2 versions, 1 yanked"));
        let newest = versions.find("1.1.0").unwrap();
        let oldest = versions.find("1.0.0").unwrap();
        assert!(newest < oldest);
        assert!(versions.contains("<li><span class=\"mono\">1.0.0</span><span class=\"state state-bad\">Yanked</span></li>"));
        assert!(versions.contains("<li><span class=\"mono\">1.1.0</span><span class=\"row-note\">latest</span></li>"));
    }
}
