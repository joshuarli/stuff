//! Opt-in check that the checked-in mirror manifest builds against the real
//! upstreams: every pinned SHA-256 verifies, every keep list matches files,
//! and each entry produces a deterministic zip. Run with
//! `cargo test -p mymirror -- --ignored seed` — it downloads the real
//! artifacts.

use std::path::Path;

use mymirror::{HttpsAcquire, build, manifest::Manifest};

#[test]
#[ignore = "downloads the real upstream artifacts"]
fn the_checked_in_manifest_builds_against_the_real_upstreams() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("mirror/manifest.toml")).unwrap();
    let manifest = Manifest::parse(&text).unwrap();
    let scratch = root.join(".work/tmp/seed");
    std::fs::create_dir_all(&scratch).unwrap();
    let built = build(&manifest, "https://mirror.17166969.xyz", &HttpsAcquire::new(), None, &scratch).unwrap();
    assert_eq!(built.artifacts.len(), manifest.artifacts.len());
    for artifact in &built.artifacts {
        assert!(
            !artifact.files.is_empty(),
            "{} kept no files",
            artifact.id
        );
        let zip = std::fs::read(&artifact.path).unwrap();
        assert_eq!(zip.len() as u64, artifact.bytes);
        assert_eq!(mymirror::sha256_hex(&zip), artifact.sha256);
    }
    println!(
        "{} artifacts, generation {}",
        built.artifacts.len(),
        built.generation
    );
}
