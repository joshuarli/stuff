use mycrates::crate_file::parse_packaged_manifest;
use mycrates::index::publish_to_index_with_manifest;
use mycrates::model::{Dependency, PublishBody};
use std::collections::BTreeMap;

fn base_body(name: &str, vers: &str) -> PublishBody {
    PublishBody {
        name: name.to_string(),
        vers: vers.to_string(),
        deps: vec![],
        features: BTreeMap::new(),
        authors: None,
        description: None,
        documentation: None,
        homepage: None,
        keywords: None,
        license: None,
        license_file: None,
        readme: None,
        readme_file: None,
        repository: None,
        badges: BTreeMap::new(),
        links: None,
        rust_version: None,
        checksum: "abc".to_string(),
        yanked: false,
    }
}

fn dep(
    name: Option<&str>,
    req: &str,
    kind: Option<&str>,
    target: Option<&str>,
    registry: Option<&str>,
    package: Option<&str>,
    explicit: Option<&str>,
) -> Dependency {
    Dependency {
        name: name.map(|s| s.to_string()),
        version_req: req.to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: target.map(|s| s.to_string()),
        kind: kind.map(|s| s.to_string()),
        registry: registry.map(|s| s.to_string()),
        package: package.map(|s| s.to_string()),
        explicit_name_in_toml: explicit.map(|s| s.to_string()),
    }
}

#[test]
fn unrenamed_public_dependency_accepted() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nbar = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("bar"),
        "^1.0",
        None,
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn versioned_dev_dependency_accepted() {
    let packaged_src =
        "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dev-dependencies]\ndevdep = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("devdep"),
        "^1.0",
        Some("dev"),
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn same_crate_runtime_and_build_time() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nserde = \"1.0\"\n\n[build-dependencies]\nserde = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("serde"),
        "^1.0",
        None,
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    body.deps.push(dep(
        Some("serde"),
        "^1.0",
        Some("build"),
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn same_alias_in_two_target_sections() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[target.'cfg(unix)'.dependencies]\nserde = \"1.0\"\n\n[target.'cfg(windows)'.dependencies]\nserde = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("serde"),
        "^1.0",
        None,
        Some("cfg(unix)"),
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    body.deps.push(dep(
        Some("serde"),
        "^1.0",
        None,
        Some("cfg(windows)"),
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn renamed_crates_io_dependency_accepted() {
    let packaged_src =
        "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nrenamed = { package = \"serde_json\", version = \"1.0\" }\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("serde_json"),
        "^1.0",
        None,
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        Some("renamed"),
    ));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn same_registry_dependency_accepted() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nlib-a = { version = \"0.1.0\", registry-index = \"sparse+http://127.0.0.1:9999/current/registry/\" }\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(Some("lib-a"), "^0.1.0", None, None, None, None, None));
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn features_compared_semantically() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nbar = { version = \"1.0\", features = [\"a\", \"b\"] }\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    let mut d = dep(
        Some("bar"),
        "^1.0",
        None,
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    );
    d.features = vec!["b".to_string(), "a".to_string()];
    body.deps.push(d);
    publish_to_index_with_manifest(&body, Some(&packaged)).unwrap();
}

#[test]
fn wrong_kind_still_rejected() {
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n\n[dependencies]\nbar = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();
    let mut body = base_body("foo", "1.0.0");
    body.deps.push(dep(
        Some("bar"),
        "^1.0",
        Some("build"),
        None,
        Some("https://github.com/rust-lang/crates.io-index"),
        None,
        None,
    ));
    assert!(publish_to_index_with_manifest(&body, Some(&packaged)).is_err());
}
