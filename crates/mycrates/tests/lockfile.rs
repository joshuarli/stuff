use mycrates::lockfile::{parse_lockfile, classify, PkgKind};

#[test]
fn parse_v3() {
    let s = r#"
version = 3

[[package]]
name = "foo"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc123"
"#;
    let lock = parse_lockfile(s).unwrap();
    assert_eq!(lock.version, Some(3));
    let c = classify(&lock.package[0]);
    assert!(matches!(c.kind, PkgKind::CratesIo));
}

#[test]
fn cratesio_needs_checksum() {
    let s = r#"
version = 3

[[package]]
name = "foo"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
    let lock = parse_lockfile(s).unwrap();
    assert!(lock.package[0].checksum.is_none());
}
