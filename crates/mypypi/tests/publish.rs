#![forbid(unsafe_code)]

use mypypi::catalog::parse_catalog;
use mypypi::config::parse_project_settings;
use mypypi::error::Kind;
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

fn settings_and_catalog() -> (mypypi::config::ProjectSettings, mypypi::catalog::Catalog) {
    let platform = mypypi::validate::local_platform().expect("test host has a supported platform");
    let settings_text = format!(
        "[project]\nrequires-python = \">=3.15,<3.16\"\n\n[tool.mypypi]\npublic_base_url = \"https://seed.example.test\"\n\n[tool.mypypi.targets]\nplatforms = [\"{platform}\"]\nlinux_repair_policy = \"manylinux_2_28\"\n"
    );
    let settings = parse_project_settings(&settings_text).unwrap();
    let catalog = parse_catalog("[[packages]]\nname = \"validation-fixture\"\nversion = \"1.0\"\n").unwrap();
    (settings, catalog)
}

fn staged_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let output = dir.join("out");
    let state = dir.join("state");
    let wheels = state.join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    std::fs::copy(fixture_wheel(), wheels.join(FIXTURE_WHEEL)).unwrap();
    (output, state)
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

#[test]
fn identical_publishes_report_no_op_false_then_true() {
    let dir = tempfile::tempdir().unwrap();
    let (output, state) = staged_fixture(dir.path());
    let (mut settings, catalog) = settings_and_catalog();
    let staged = state.join("wheels").join(FIXTURE_WHEEL);
    let identity = WheelIdentity::parse(FIXTURE_WHEEL).unwrap();
    let sha256 = mypypi::acquire::sha256_file(&staged).unwrap();
    let artifact =
        ValidatedArtifact::new(identity, staged, sha256, settings.targets()).unwrap();
    mypypi::store::publish(
        &[artifact],
        &output,
        &BTreeMap::new(),
        &settings.public_base_url,
    )
    .unwrap();
    settings.public_base_url = "https://fresh.example.test".to_string();
    let options = || PublishOptions {
        settings: &settings,
        output: &output,
        state_dir: &state,
        catalog: &catalog,
        platforms: &[],
    };

    let first = publish(&options()).unwrap();
    assert!(!first.no_op, "first publish must activate a new generation");

    let second = publish(&options()).unwrap();
    assert!(second.no_op, "identical second publish must report no_op");
    assert_eq!(second.generation_id, first.generation_id);
    assert_eq!(second.generation, first.generation);
}

#[test]
fn garbage_current_pointer_is_an_error_not_a_silent_non_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let (settings, catalog) = settings_and_catalog();

    let regular = dir.path().join("regular");
    std::fs::create_dir(&regular).unwrap();
    std::fs::write(regular.join("current"), b"not a link").unwrap();
    let err = publish(&PublishOptions {
        settings: &settings,
        output: &regular,
        state_dir: &dir.path().join("regular-state"),
        catalog: &catalog,
        platforms: &[],
    })
    .unwrap_err();
    assert!(matches!(err.kind, Kind::Validation), "kind: {:?}", err.kind);
    assert!(err.detail.contains("not a symlink"), "{}", err.detail);

    let dangling = dir.path().join("dangling");
    std::fs::create_dir(&dangling).unwrap();
    std::os::unix::fs::symlink("elsewhere", dangling.join("current")).unwrap();
    let err = publish(&PublishOptions {
        settings: &settings,
        output: &dangling,
        state_dir: &dir.path().join("dangling-state"),
        catalog: &catalog,
        platforms: &[],
    })
    .unwrap_err();
    assert!(matches!(err.kind, Kind::Validation), "kind: {:?}", err.kind);
    assert!(err.detail.contains("invalid current link"), "{}", err.detail);
}
