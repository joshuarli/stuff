#![forbid(unsafe_code)]

use mypypi::catalog::parse_catalog;
use mypypi::config::parse_project_settings;
use mypypi::models::{Target, ValidatedArtifact, WheelIdentity};
use mypypi::publish::{publish, PublishOptions};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FIXTURE_WHEEL: &str = "validation_fixture-1.0-py3-none-any.whl";

fn fixture_wheel() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/mypypi/wheels")
        .join(FIXTURE_WHEEL)
}

#[test]
fn publish_summary_matches_the_generation_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out");
    let state = dir.path().join("state");
    let wheels = state.join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    let staged = wheels.join(FIXTURE_WHEEL);
    std::fs::copy(fixture_wheel(), &staged).unwrap();

    let settings_text = "[project]\nrequires-python = \">=3.15,<3.16\"\n\n[tool.mypypi]\npublic_base_url = \"https://seed.example.test\"\n\n[tool.mypypi.targets]\nplatforms = [\"macos-arm64\"]\nlinux_repair_policy = \"manylinux_2_28\"\n";
    let mut settings = parse_project_settings(settings_text).unwrap();
    let catalog = parse_catalog("[[packages]]\nname = \"validation-fixture\"\nversion = \"1.0\"\n").unwrap();

    let identity = WheelIdentity::parse(FIXTURE_WHEEL).unwrap();
    let sha256 = mypypi::acquire::sha256_file(&staged).unwrap();
    let target = Target::new("3.15", "macos-arm64").unwrap();
    let artifact = ValidatedArtifact::new(identity, staged.clone(), sha256, vec![target]).unwrap();
    mypypi::store::publish(&[artifact], &output, &BTreeMap::new(), "https://seed.example.test").unwrap();

    settings.public_base_url = "https://fresh.example.test".to_string();
    let summary = publish(&PublishOptions {
        settings: &settings,
        output: &output,
        state_dir: &state,
        catalog: &catalog,
        platforms: &[],
    })
    .unwrap();

    assert_eq!(summary.new_wheels, 0);
    assert_eq!(summary.no_imports, 0);
    assert!(!summary.no_op);
    let absolute = std::path::absolute(&output).unwrap();
    assert_eq!(summary.generation, absolute.join("current"));
    assert_eq!(
        std::fs::read_link(&summary.generation).unwrap(),
        Path::new("generations").join(&summary.generation_id)
    );
    let generation_dir = absolute.join("generations").join(&summary.generation_id);
    assert!(generation_dir.is_dir());
    let llms = std::fs::read_to_string(generation_dir.join("llms.txt")).unwrap();
    assert!(llms.contains("https://fresh.example.test"), "{llms}");

    let repeated = publish(&PublishOptions {
        settings: &settings,
        output: &output,
        state_dir: &state,
        catalog: &catalog,
        platforms: &[],
    })
    .unwrap();
    assert!(repeated.no_op);
    assert_eq!(repeated.generation_id, summary.generation_id);
    assert_eq!(repeated.new_wheels, 0);
}
