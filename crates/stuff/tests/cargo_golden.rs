use std::path::{Path, PathBuf};

use deser_value::Value;
use stuff::publish::cargo::{PUBLISH_RESPONSE, PublishRequest, accept};

fn golden_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../mycrates/tests/golden/publish")
}

fn run_case(name: &str) {
    let case = golden_root().join(name);
    let metadata = std::fs::read_to_string(case.join("metadata.json"))
        .unwrap_or_else(|error| panic!("{name}: metadata.json: {error}"));
    let expected_text = std::fs::read_to_string(case.join("expected.json"))
        .unwrap_or_else(|error| panic!("{name}: expected.json: {error}"));
    let expected: Value = deser_json::from_str(&expected_text)
        .unwrap_or_else(|error| panic!("{name}: expected.json is not JSON: {error}"));
    let archive = std::fs::read(case.join("crate.crate"))
        .unwrap_or_else(|error| panic!("{name}: crate.crate: {error}"));
    let request = PublishRequest::from_json(&metadata)
        .unwrap_or_else(|rejection| panic!("{name}: metadata rejected: {}", rejection.detail));
    let expected_status = expected
        .get("status")
        .and_then(|value| value.as_u64())
        .unwrap_or_else(|| panic!("{name}: expected.json has no status")) as u16;
    let expected_response = expected
        .get("response")
        .unwrap_or_else(|| panic!("{name}: expected.json has no response"));

    match accept(&request, &archive) {
        Ok(accepted) => {
            assert_eq!(expected_status, 200, "{name}: accepted but rejection expected");
            let body: Value = deser_json::from_str(PUBLISH_RESPONSE)
                .unwrap_or_else(|error| panic!("{name}: publish response is not JSON: {error}"));
            assert_eq!(&body, expected_response, "{name}: response body");
            let expected_line = expected
                .get("index_line")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{name}: expected.json has no index_line"));
            assert_eq!(accepted.index_line, expected_line, "{name}: index line");
            let expected_shard = expected
                .get("shard")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{name}: expected.json has no shard"));
            assert_eq!(accepted.shard, expected_shard, "{name}: shard");
            let expected_cksum = expected
                .get("cksum")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{name}: expected.json has no cksum"));
            assert_eq!(accepted.cksum, expected_cksum, "{name}: cksum");
        }
        Err(rejection) => {
            assert_eq!(
                rejection.status(),
                expected_status,
                "{name}: status (detail: {})",
                rejection.detail
            );
            let body: Value = deser_json::from_str(&rejection.body())
                .unwrap_or_else(|error| panic!("{name}: error body is not JSON: {error}"));
            assert_eq!(&body, expected_response, "{name}: error body");
            let expected_detail = expected_response
                .get("errors")
                .and_then(|value| value.as_seq())
                .and_then(|errors| errors.iter().next())
                .and_then(|error| error.get("detail"))
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| panic!("{name}: expected.json has no error detail"));
            assert_eq!(rejection.detail, expected_detail, "{name}: error detail");
        }
    }
}

#[test]
fn golden_links_rust_version() {
    run_case("links-rust-version");
}

#[test]
fn golden_plain() {
    run_case("plain");
}

#[test]
fn golden_reject_extra_dependency() {
    run_case("reject-extra-dependency");
}

#[test]
fn golden_reject_feature_mismatch() {
    run_case("reject-feature-mismatch");
}

#[test]
fn golden_reject_foreign_registry() {
    run_case("reject-foreign-registry");
}

#[test]
fn golden_reject_links_mismatch() {
    run_case("reject-links-mismatch");
}

#[test]
fn golden_reject_renamed_package_mismatch() {
    run_case("reject-renamed-package-mismatch");
}

#[test]
fn golden_reject_requirement_mismatch() {
    run_case("reject-requirement-mismatch");
}

#[test]
fn golden_reject_wrong_kind() {
    run_case("reject-wrong-kind");
}

#[test]
fn golden_renamed_targets_features() {
    run_case("renamed-targets-features");
}
