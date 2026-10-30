use mycrates::index::{publish_to_index, serialize_index_line, shard_for_name, native_config_json, mirror_config_json};
use mycrates::model::{Dependency, PublishBody};
use std::collections::BTreeMap;

fn base_body() -> PublishBody {
    PublishBody {
        name: "foo".to_string(),
        vers: "1.0.0".to_string(),
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
        checksum: "abc123".to_string(),
        yanked: false,
    }
}

#[test]
fn registry_null_vs_cratesio_url() {
    // dependency on same registry has no registry key -> becomes null
    let mut body = base_body();
    body.deps.push(Dependency {
        name: Some("bar".to_string()),
        version_req: "^1.0".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: None,
        package: None,
        explicit_name_in_toml: None,
    });
    let entry = publish_to_index(&body).unwrap();
    let line = serialize_index_line(&entry).unwrap();
    assert!(line.contains("\"registry\":null"));
}

#[test]
fn cratesio_dependency_kept_verbatim() {
    let mut body = base_body();
    body.deps.push(Dependency {
        name: Some("itoa".to_string()),
        version_req: "^1.0".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: Some("https://github.com/rust-lang/crates.io-index".to_string()),
        package: None,
        explicit_name_in_toml: None,
    });
    let entry = publish_to_index(&body).unwrap();
    let line = serialize_index_line(&entry).unwrap();
    assert!(line.contains("https://github.com/rust-lang/crates.io-index"));
}

#[test]
fn rename_dependency() {
    let mut body = base_body();
    body.deps.push(Dependency {
        name: Some("serde_json".to_string()),
        version_req: "^1.0".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: None,
        package: None,
        explicit_name_in_toml: Some("json".to_string()),
    });
    let entry = publish_to_index(&body).unwrap();
    let line = serialize_index_line(&entry).unwrap();
    // alias in index name, package is real crate name
    assert!(line.contains("\"name\":\"json\""));
    assert!(line.contains("\"package\":\"serde_json\""));
}

#[test]
fn features_v2() {
    let mut body = base_body();
    let mut feats = BTreeMap::new();
    feats.insert("default".to_string(), vec!["dep:bar".to_string()]);
    body.features = feats;
    let entry = publish_to_index(&body).unwrap();
    assert_eq!(entry.v, Some(2));
}

#[test]
fn links_and_rust_version() {
    let mut body = base_body();
    body.links = Some("foo".to_string());
    body.rust_version = Some("1.70".to_string());
    let entry = publish_to_index(&body).unwrap();
    let line = serialize_index_line(&entry).unwrap();
    assert!(line.contains("\"links\":\"foo\""));
    assert!(line.contains("\"rust_version\":\"1.70\""));
}

#[test]
fn sharding() {
    assert_eq!(shard_for_name("a"), "1/a");
    assert_eq!(shard_for_name("ab"), "2/ab");
    assert_eq!(shard_for_name("abc"), "3/a/abc");
    assert_eq!(shard_for_name("abcd"), "ab/cd/abcd");
}

#[test]
fn configs() {
    let n = native_config_json("https://example.com/base");
    assert!(n.contains("\"api\""));
    let m = mirror_config_json("https://example.com/base");
    assert!(!m.contains("\"api\""));
}

#[test]
fn malformed_dependency_rejected() {
    let mut body = base_body();
    body.deps.push(Dependency {
        name: Some("bad dep!".to_string()),
        version_req: "1.0".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: None,
        package: None,
        explicit_name_in_toml: None,
    });
    assert!(publish_to_index(&body).is_err());

    let mut body2 = base_body();
    body2.deps.push(Dependency {
        name: Some("good".to_string()),
        version_req: "not-a-req!!!".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: None,
        package: None,
        explicit_name_in_toml: None,
    });
    assert!(publish_to_index(&body2).is_err());
}

#[test]
fn manifest_reconciliation_rejects_divergence() {
    use mycrates::crate_file::parse_packaged_manifest;
    let packaged_src = "[package]\nname = \"foo\"\nversion = \"1.0.0\"\nlinks = \"native\"\nrust-version = \"1.70\"\n\n[features]\ndefault = []\n\n[dependencies]\nbar = \"1.0\"\n";
    let packaged = parse_packaged_manifest(packaged_src).unwrap();

    let mut diverged = base_body();
    diverged.links = Some("other".to_string());
    assert!(mycrates::index::publish_to_index_with_manifest(&diverged, Some(&packaged)).is_err());

    let mut diverged2 = base_body();
    diverged2.features.insert("extra".to_string(), vec!["x".to_string()]);
    assert!(mycrates::index::publish_to_index_with_manifest(&diverged2, Some(&packaged)).is_err());

    let mut diverged3 = base_body();
    diverged3.deps.push(Dependency {
        name: Some("other".to_string()),
        version_req: "1.0".to_string(),
        features: vec![],
        optional: false,
        default_features: true,
        target: None,
        kind: None,
        registry: None,
        package: None,
        explicit_name_in_toml: None,
    });
    assert!(mycrates::index::publish_to_index_with_manifest(&diverged3, Some(&packaged)).is_err());
}

#[test]
fn upstream_unknown_field_preserved() {
    // simulate upstream line with extra field
    let line = r#"{"name":"serde","vers":"1.0.0","deps":[],"cksum":"x","features":{},"yanked":false,"pubtime":12345}"#;
    let entry: mycrates::model::IndexEntry = deser_json::from_str(line).unwrap();
    assert!(entry.extra.contains_key("pubtime"));
    let s = serialize_index_line(&entry).unwrap();
    assert!(s.contains("pubtime"));
}
