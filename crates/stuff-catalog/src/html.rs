#![forbid(unsafe_code)]

use crate::model::{Catalog, DependencyKind, Package, Registry};
use std::collections::BTreeMap;

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
:root { color-scheme: light dark; --bg: #fff; --fg: #171717; --mute: #6b6b6b; --line: #ebebeb; --void: #b42318; }
@media (prefers-color-scheme: dark) { :root { --bg: #0d0d0d; --fg: #ededed; --mute: #9b9b9b; --line: #242424; --void: #f97066; } }
* { box-sizing: border-box; }
html { background: var(--bg); color: var(--fg); }
body { margin: 0 auto; max-width: 46rem; padding: 0 1.5rem 4rem; font: 400 0.9375rem/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; -webkit-font-smoothing: antialiased; overflow-wrap: anywhere; }
a { color: inherit; text-decoration: underline; text-decoration-color: var(--line); text-underline-offset: 0.2em; text-decoration-thickness: 1px; }
a:hover { text-decoration-color: currentColor; }
:focus-visible { outline: 2px solid var(--fg); outline-offset: 3px; border-radius: 2px; }
code, .mono { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.875em; }
.mute { color: var(--mute); }
header { padding: 2rem 0 4rem; font-weight: 600; letter-spacing: -0.01em; }
h1 { font-size: 1.5rem; line-height: 1.3; font-weight: 600; letter-spacing: -0.02em; margin: 0 0 0.5rem; max-width: 30rem; }
h2 { font-size: 0.9375rem; font-weight: 600; margin: 2.5rem 0 0.5rem; }
.lede { margin: 0 0 3rem; color: var(--mute); }
.registries { display: grid; grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr)); gap: 1.5rem; margin: 0 0 3.5rem; padding: 0; list-style: none; }
.registries b { display: block; font-weight: 600; }
.registries span { color: var(--mute); }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th { text-align: left; font-weight: 400; color: var(--mute); padding: 0 1rem 0.6rem 0; border-bottom: 1px solid var(--line); }
td { padding: 0.65rem 1rem 0.65rem 0; border-bottom: 1px solid var(--line); vertical-align: baseline; }
th:last-child, td:last-child { padding-right: 0; }
.r { text-align: right; }
.pkg { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.875rem; }
.vers { color: var(--mute); font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.8125rem; line-height: 1.7; }
.vers b { color: var(--fg); }
.yanked { color: var(--void); text-decoration: line-through; }
.detail { display: none; }
.detail:target { display: block; }
main:has(.detail:target) .index { display: none; }
.back { margin: 0 0 2rem; }
.refs { margin: 0; padding: 0; list-style: none; }
.refs li { padding: 0.35rem 0; border-bottom: 1px solid var(--line); }
.refs .mute { margin-left: 0.75rem; }\ndetails { margin: 1rem 0 0; }\nsummary { cursor: pointer; font-weight: 600; }\ndetails[open] summary { margin-bottom: 0.5rem; }
footer { margin-top: 3rem; display: flex; flex-wrap: wrap; gap: 0.25rem 1.5rem; color: var(--mute); font-size: 0.875rem; }
@media (max-width: 640px) { header { padding-bottom: 2.5rem; } .hide { display: none; } }
"#;

fn registry_label(registry: Registry) -> &'static str {
    match registry {
        Registry::Cargo => "Cargo",
        Registry::Npm => "npm",
        Registry::Pypi => "PyPI",
    }
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
        escape_html(&fragment_href(&anchor(registry, package))),
        escape_html(&package.name)
    )
}

fn versions_markup(package: &Package) -> String {
    package
        .versions
        .iter()
        .map(|v| {
            let version = escape_html(&v.version);
            if v.yanked {
                format!("<span class=\"yanked\" title=\"Yanked\">{version}</span>")
            } else if v.version == package.latest {
                format!("<b>{version}</b>")
            } else {
                version
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{CSS}</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        escape_html(title)
    )
}

struct Dependent<'a> {
    package: &'a Package,
    kind: DependencyKind,
}

fn dependents_of<'a>(catalog: &'a Catalog) -> BTreeMap<String, Vec<Dependent<'a>>> {
    let mut dependents: BTreeMap<String, Vec<Dependent<'a>>> = BTreeMap::new();
    for package in &catalog.packages {
        let mut strongest: BTreeMap<String, DependencyKind> = BTreeMap::new();
        for (kind, name) in package.dependencies.iter() {
            if let Some(target) = catalog.resolve_dependency(name) {
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

fn detail_sections(catalogs: &[&Catalog]) -> String {
    let mut out = String::new();
    for catalog in catalogs {
        let dependents = dependents_of(catalog);
        for package in &catalog.packages {
            let id = anchor(catalog.registry, package);
            out.push_str(&format!("<section class=\"detail\" id=\"{}\">\n", escape_html(&id)));
            out.push_str("<p class=\"back\"><a href=\"#\">All packages</a></p>\n");
            let source = package.source.as_deref().map(|s| format!(", {s}")).unwrap_or_default();
            out.push_str(&format!(
                "<h1 class=\"pkg\">{}</h1>\n<p class=\"lede\">{}{}. Latest {}.</p>\n",
                escape_html(&package.name),
                registry_label(catalog.registry),
                escape_html(&source),
                escape_html(&package.latest)
            ));
            if catalog.schema_version < 2 {
                out.push_str("<p class=\"mute\">This generation was published before dependencies were recorded.</p>\n");
            } else {
                out.push_str(&dependency_sections(catalog, package, dependents.get(&id)));
            }
            out.push_str(&format!(
                "<h2>Versions</h2>\n<p class=\"vers\">{}</p>\n</section>\n",
                versions_markup(package)
            ));
        }
    }
    out
}

fn name_list(catalog: &Catalog, names: &[String]) -> String {
    let mut hosted = String::new();
    let mut elsewhere = Vec::new();
    for name in names {
        match catalog.resolve_dependency(name) {
            Some(target) => hosted.push_str(&format!("<li class=\"pkg\">{}</li>\n", package_link(catalog.registry, target))),
            None => elsewhere.push(format!("<span class=\"pkg\">{}</span>", escape_html(name))),
        }
    }
    let mut out = String::new();
    if !hosted.is_empty() {
        out.push_str(&format!("<ul class=\"refs\">\n{hosted}</ul>\n"));
    }
    if !elsewhere.is_empty() {
        out.push_str(&format!("<p class=\"mute\">Not hosted here: {}</p>\n", elsewhere.join(", ")));
    }
    out
}

fn kind_title(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Runtime => "Dependencies",
        DependencyKind::Peer => "Peer dependencies",
        DependencyKind::Build => "Build dependencies",
        DependencyKind::Dev => "Dev dependencies",
    }
}

fn dependency_sections(catalog: &Catalog, package: &Package, users: Option<&Vec<Dependent<'_>>>) -> String {
    let mut out = String::from("<h2>Depends on</h2>\n");
    if package.dependencies.runtime.is_empty() {
        out.push_str(&format!("<p class=\"mute\">No dependencies in {}.</p>\n", escape_html(&package.latest)));
    } else {
        out.push_str(&name_list(catalog, &package.dependencies.runtime));
    }
    if catalog.schema_version < 3 {
        out.push_str("<p class=\"mute\">Peer, build and dev dependencies were not recorded for this generation.</p>\n");
    } else {
        for kind in [DependencyKind::Peer, DependencyKind::Build, DependencyKind::Dev] {
            let names = package.dependencies.of(kind);
            if !names.is_empty() {
                out.push_str(&format!(
                    "<details>\n<summary>{} ({})</summary>\n{}</details>\n",
                    kind_title(kind),
                    names.len(),
                    name_list(catalog, names)
                ));
            }
        }
    }

    out.push_str("<h2>Used by</h2>\n");
    match users {
        None => out.push_str("<p class=\"mute\">No package here depends on it.</p>\n"),
        Some(users) => {
            out.push_str("<ul class=\"refs\">\n");
            for user in users {
                let note = match user.kind {
                    DependencyKind::Runtime => String::new(),
                    kind => format!("<span class=\"mute\">{}</span>", kind.as_str()),
                };
                out.push_str(&format!("<li class=\"pkg\">{}{note}</li>\n", package_link(catalog.registry, user.package)));
            }
            out.push_str("</ul>\n");
        }
    }
    out
}

pub fn render_html(catalog: &Catalog) -> String {
    let label = registry_label(catalog.registry);
    let mut out = String::new();
    out.push_str(&format!("<header>{}</header>\n<main>\n", escape_html(label)));
    out.push_str(&format!(
        "<div class=\"index\">\n<h1>{} registry</h1>\n<p class=\"lede\">{} packages, {} versions</p>\n",
        escape_html(label),
        catalog.package_count(),
        catalog.version_count()
    ));
    let has_source = catalog.registry == Registry::Cargo;
    out.push_str("<table>\n<thead><tr><th>Package</th>");
    if has_source {
        out.push_str("<th class=\"hide\">Source</th>");
    }
    out.push_str("<th>Versions</th></tr></thead>\n<tbody>\n");
    for package in &catalog.packages {
        out.push_str(&format!("<tr><td class=\"pkg\">{}</td>", package_link(catalog.registry, package)));
        if has_source {
            out.push_str(&format!(
                "<td class=\"mute hide\">{}</td>",
                escape_html(package.source.as_deref().unwrap_or(""))
            ));
        }
        out.push_str(&format!("<td class=\"vers\">{}</td></tr>\n", versions_markup(package)));
    }
    out.push_str("</tbody>\n</table>\n</div>\n");
    out.push_str(&detail_sections(&[catalog]));
    out.push_str("</main>\n");
    out.push_str(&format!(
        "<footer><span>Generation <code>{generation}</code></span><a href=\"/generations/{generation}/catalog.json\">catalog.json</a><span>Schema version {schema}</span></footer>\n",
        generation = escape_html(&catalog.generation),
        schema = catalog.schema_version
    ));
    document(&format!("{label} registry browser"), &out)
}

pub fn render_unified(catalogs: &[Catalog]) -> String {
    let mut out = String::from("<header>stuff</header>\n<main>\n<div class=\"index\">\n");
    out.push_str("<h1>Cargo, npm and PyPI packages, served as static files.</h1>\n");
    out.push_str("<p class=\"lede\">Each publish creates a new generation. The current one is live.</p>\n");
    out.push_str("<ul class=\"registries\">\n");
    for catalog in catalogs {
        out.push_str(&format!(
            "<li><b>{}</b><span>{} packages, {} versions</span></li>\n",
            escape_html(registry_label(catalog.registry)),
            catalog.package_count(),
            catalog.version_count()
        ));
    }
    out.push_str("</ul>\n");
    out.push_str("<table>\n<thead><tr><th>Package</th><th>Registry</th><th class=\"hide\">Source</th><th class=\"r\">Latest</th></tr></thead>\n<tbody>\n");
    for catalog in catalogs {
        for package in &catalog.packages {
            out.push_str(&format!(
                "<tr><td class=\"pkg\">{}</td><td>{}</td><td class=\"mute hide\">{}</td><td class=\"r mono\">{}</td></tr>\n",
                package_link(catalog.registry, package),
                escape_html(registry_label(catalog.registry)),
                escape_html(package.source.as_deref().unwrap_or("")),
                escape_html(&package.latest)
            ));
        }
    }
    out.push_str("</tbody>\n</table>\n</div>\n");
    let all: Vec<&Catalog> = catalogs.iter().collect();
    out.push_str(&detail_sections(&all));
    out.push_str("</main>\n");
    document("stuff registry browser", &out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CargoInput, CargoSource, Dependencies, NpmInput, cargo_catalog, npm_catalog};
    use std::collections::BTreeMap;

    fn cargo_input(name: &str, version: &str, dependencies: &[&str]) -> CargoInput {
        CargoInput {
            name: name.to_string(),
            version: version.to_string(),
            yanked: false,
            source: CargoSource::Native,
            dependencies: Dependencies::runtime_only(dependencies.iter().map(|d| d.to_string()).collect()),
        }
    }

    fn section<'a>(html: &'a str, id: &str) -> &'a str {
        let start = html.find(&format!("<section class=\"detail\" id=\"{id}\">")).expect("detail section");
        let rest = &html[start..];
        &rest[..rest.find("</section>").expect("section end")]
    }

    #[test]
    fn registry_page_lists_packages_and_links_catalog() {
        let catalog = cargo_catalog("gen1", vec![cargo_input("h12tiny", "0.6.0", &[])]).unwrap();
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
                href.starts_with("/generations/gen1/") || href.starts_with('#'),
                "public page link is neither generation-pinned nor in-page: {href}"
            );
        }
    }

    #[test]
    fn unified_page_aggregates_registries() {
        let catalog = cargo_catalog("gen1", vec![cargo_input("h12tiny", "0.6.0", &[])]).unwrap();
        let html = render_unified(std::slice::from_ref(&catalog));
        assert!(html.contains("stuff"));
        assert!(html.contains("h12tiny"));
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
        assert!(app.contains("Not hosted here: <span class=\"pkg\">outside</span>"));
        assert!(!app.contains("href=\"#cargo/native/outside\""));
        assert!(app.contains("No package here depends on it."));

        let lib = section(&html, "cargo/native/lib");
        assert!(lib.contains("href=\"#cargo/native/app\""));
        assert!(lib.contains("href=\"#cargo/native/tool\""));
        assert!(lib.contains("No dependencies in 2.0.0."));
    }

    #[test]
    fn non_runtime_kinds_get_their_own_sections_and_labels() {
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
        assert!(app.contains("<summary>Peer dependencies (1)</summary>"));
        assert!(app.contains("<summary>Build dependencies (1)</summary>"));
        assert!(app.contains("<summary>Dev dependencies (2)</summary>"));
        assert!(app.contains("Not hosted here: <span class=\"pkg\">outside-dev</span>"));
        assert!(app.contains("href=\"#cargo/native/helper\""));

        let lib = section(&html, "cargo/native/lib");
        let used_by = &lib[lib.find("Used by").unwrap()..];
        assert!(used_by.contains("href=\"#cargo/native/app\""));
        assert!(!used_by.contains("<span class=\"mute\">build</span>"), "a runtime use outranks a build use");

        let helper = section(&html, "cargo/native/helper");
        assert!(helper.contains("<span class=\"mute\">dev</span>"));
        assert!(!lib.contains("<summary>"), "kinds with no names get no section");
    }

    #[test]
    fn schema_two_catalogs_do_not_claim_other_kinds_are_empty() {
        let text = r#"{"schema_version":2,"registry":"cargo","generation":"g","packages":[{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}],"dependencies":["b"]}]}"#;
        let html = render_html(&crate::model::parse_catalog_json(text).unwrap());
        let detail = section(&html, "cargo/native/a");
        assert!(detail.contains("Peer, build and dev dependencies were not recorded"));
        assert!(detail.contains("Not hosted here: <span class=\"pkg\">b</span>"));
    }

    #[test]
    fn scoped_npm_names_keep_a_working_fragment() {
        let catalog = npm_catalog(
            "gen1",
            vec![
                NpmInput { name: "@scope/a".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::runtime_only(vec!["@scope/b".to_string()]) },
                NpmInput { name: "@scope/b".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() },
            ],
            &BTreeMap::new(),
        )
        .unwrap();
        let html = render_html(&catalog);
        assert!(section(&html, "npm/@scope/a").contains("href=\"#npm/@scope/b\""));
        assert!(section(&html, "npm/@scope/b").contains("href=\"#npm/@scope/a\""));
    }

    #[test]
    fn dependencies_never_cross_registries() {
        let cargo = cargo_catalog("g", vec![cargo_input("shared", "1.0.0", &[])]).unwrap();
        let npm = npm_catalog(
            "g",
            vec![NpmInput { name: "shared".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::default() },
                 NpmInput { name: "user".to_string(), version: "1.0.0".to_string(), dependencies: Dependencies::runtime_only(vec!["shared".to_string()]) }],
            &BTreeMap::new(),
        )
        .unwrap();
        let html = render_unified(&[cargo, npm]);
        assert!(section(&html, "cargo/native/shared").contains("No package here depends on it."));
        assert!(section(&html, "npm/shared").contains("href=\"#npm/user\""));
    }

    #[test]
    fn schema_one_catalogs_do_not_claim_empty_dependencies() {
        let text = r#"{"schema_version":1,"registry":"cargo","generation":"g","packages":[{"name":"a","source":"native","latest":"1.0.0","versions":[{"version":"1.0.0","yanked":false}]}]}"#;
        let catalog = crate::model::parse_catalog_json(text).unwrap();
        let html = render_html(&catalog);
        let detail = section(&html, "cargo/native/a");
        assert!(detail.contains("published before dependencies were recorded"));
        assert!(!detail.contains("No dependencies"));
        assert!(!detail.contains("Used by"));
    }

    #[test]
    fn yanked_versions_are_marked_in_listings() {
        let mut input = cargo_input("a", "1.0.0", &[]);
        input.yanked = true;
        let catalog = cargo_catalog("g", vec![input, cargo_input("a", "1.1.0", &[])]).unwrap();
        let html = render_html(&catalog);
        assert!(html.contains("<span class=\"yanked\" title=\"Yanked\">1.0.0</span>"));
        assert!(html.contains("<b>1.1.0</b>"));
    }
}
