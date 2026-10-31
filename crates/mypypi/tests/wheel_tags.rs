#![forbid(unsafe_code)]

use deser_value::Value;
use mypypi::models::Target;
use mypypi::platforms::{ordered_tag_strings, target_platform_tags};

fn string_field<'a>(map: &'a deser_value::Map, key: &str) -> &'a str {
    map.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn string_list(value: &Value) -> Vec<String> {
    value
        .as_seq()
        .expect("string array")
        .iter()
        .map(|v| v.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn matches_packaging_tag_corpus() {
    let value: Value = deser_json::from_str(include_str!("fixtures/wheel_tags.json")).unwrap();
    let cases = value.as_map().unwrap().get("cases").unwrap().as_seq().unwrap();
    for case in cases {
        let case = case.as_map().unwrap();
        let python = string_field(case, "python");
        let platform = string_field(case, "platform");
        let policy = string_field(case, "policy");
        let target = Target::new(python, platform).unwrap();
        let expected_platforms = string_list(case.get("platforms").unwrap());
        let expected_tags = string_list(case.get("tags").unwrap());
        assert_eq!(
            target_platform_tags(&target, policy).unwrap(),
            expected_platforms,
            "{platform} {python} platform tags"
        );
        assert_eq!(
            ordered_tag_strings(&target, policy).unwrap(),
            expected_tags,
            "{platform} {python} ordered tags"
        );
    }
}
