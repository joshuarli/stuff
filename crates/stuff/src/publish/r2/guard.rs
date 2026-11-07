#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

use deser_value::Value;

use crate::publish::PublishError;

use super::client::{R2Client, RawRequest};
use super::promote;

pub const LISTED_DROPS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub source: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardReport {
    pub current: Option<String>,
    pub dropped: Vec<Version>,
    pub allowed: bool,
}

pub fn check(
    client: &R2Client,
    bucket: &str,
    site: &Path,
    generation: &str,
    allow_drops: bool,
) -> Result<GuardReport, PublishError> {
    let Some(marker) = promote::read_marker(client, bucket)? else {
        return Ok(GuardReport {
            current: None,
            dropped: Vec::new(),
            allowed: allow_drops,
        });
    };
    let current = marker.generation;
    if current == generation {
        return Ok(GuardReport {
            current: Some(current),
            dropped: Vec::new(),
            allowed: allow_drops,
        });
    }
    let live_key = format!("generations/{current}/catalog.json");
    let live_text = match read_remote(client, bucket, &live_key)? {
        Some(text) => text,
        None if allow_drops => {
            return Ok(GuardReport {
                current: Some(current),
                dropped: Vec::new(),
                allowed: true,
            });
        }
        None => {
            return Err(PublishError::Conflict(format!(
                "live generation {current} has no catalog.json, so dropped versions cannot be checked; pass --allow-drops to promote anyway"
            )));
        }
    };
    let live = versions(&live_text, &format!("remote {live_key}"))?;
    let local_path = site
        .join("generations")
        .join(generation)
        .join("catalog.json");
    let local_text = std::fs::read_to_string(&local_path).map_err(|error| {
        PublishError::Integrity(format!("cannot read {}: {error}", local_path.display()))
    })?;
    let local = versions(&local_text, &local_path.display().to_string())?;
    let dropped: Vec<Version> = live.difference(&local).cloned().collect();
    if dropped.is_empty() {
        return Ok(GuardReport {
            current: Some(current),
            dropped,
            allowed: allow_drops,
        });
    }
    if allow_drops {
        return Ok(GuardReport {
            current: Some(current),
            dropped,
            allowed: true,
        });
    }
    Err(PublishError::Conflict(format!(
        "generation {generation} drops {} versions served by live generation {current}:\n{}\nre-adopt the live publication into the local state, or pass --allow-drops to remove them on purpose",
        dropped.len(),
        format_drops(&dropped)
    )))
}

pub fn versions(text: &str, origin: &str) -> Result<BTreeSet<Version>, PublishError> {
    let value: Value = deser_json::from_str(text)
        .map_err(|error| PublishError::Integrity(format!("{origin} is invalid JSON: {error}")))?;
    let packages = value
        .as_map()
        .and_then(|map| map.get("packages"))
        .and_then(|value| value.as_seq())
        .ok_or_else(|| {
            PublishError::Integrity(format!("{origin} has no packages list"))
        })?;
    let mut found = BTreeSet::new();
    for package in packages {
        let package = package.as_map().ok_or_else(|| {
            PublishError::Integrity(format!("{origin} has a malformed package entry"))
        })?;
        let name = package
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| {
                PublishError::Integrity(format!("{origin} has a malformed package entry"))
            })?;
        let source = package
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let entries = package
            .get("versions")
            .and_then(|value| value.as_seq())
            .ok_or_else(|| {
                PublishError::Integrity(format!("{origin} has a malformed package entry"))
            })?;
        for entry in entries {
            let entry = entry.as_map().ok_or_else(|| {
                PublishError::Integrity(format!("{origin} has a malformed package entry"))
            })?;
            let version = entry
                .get("version")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    PublishError::Integrity(format!("{origin} has a malformed package entry"))
                })?;
            found.insert(Version {
                source: source.clone(),
                name: name.to_string(),
                version: version.to_string(),
            });
        }
    }
    Ok(found)
}

pub fn format_drops(dropped: &[Version]) -> String {
    let shown = dropped
        .iter()
        .take(LISTED_DROPS)
        .map(|entry| {
            if entry.source.is_empty() {
                format!("  {} {}", entry.name, entry.version)
            } else {
                format!("  {} {} ({})", entry.name, entry.version, entry.source)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let more = dropped.len().saturating_sub(LISTED_DROPS);
    if more > 0 {
        format!("{shown}\n  ... and {more} more")
    } else {
        shown
    }
}

fn read_remote(
    client: &R2Client,
    bucket: &str,
    key: &str,
) -> Result<Option<String>, PublishError> {
    let response = client.request(RawRequest {
        method: "GET",
        bucket,
        key,
        query: Vec::new(),
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    })?;
    match response.status {
        200 => Ok(Some(
            String::from_utf8_lossy(&response.body).into_owned(),
        )),
        404 => Ok(None),
        status => Err(PublishError::Integrity(format!(
            "GET {bucket}/{key}: HTTP {status}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;

    const LIVE: &str = r#"{"packages":[
        {"name":"a","source":"native","versions":[{"version":"1.0.0"}]},
        {"name":"b","source":"upstream","versions":[{"version":"2.0.0"}]}
    ]}"#;

    #[test]
    fn versions_are_the_source_name_version_triples() {
        let found = versions(LIVE, "live").unwrap();
        assert!(found.contains(&Version {
            source: "native".to_string(),
            name: "a".to_string(),
            version: "1.0.0".to_string(),
        }));
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn the_mycrates_golden_catalog_yields_every_served_version() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../mycrates/tests/golden/adopt-publish/expected-generation/catalog.json"
        ))
        .unwrap();
        let found = versions(&text, "golden").unwrap();
        let version = |source: &str, name: &str, version: &str| Version {
            source: source.to_string(),
            name: name.to_string(),
            version: version.to_string(),
        };
        assert!(found.contains(&version("native", "alpha", "0.2.0")));
        assert!(found.contains(&version("upstream", "serde", "1.0.1")));
        assert_eq!(found.len(), 10);
    }

    #[test]
    fn malformed_catalogs_are_integrity_errors() {
        for text in [
            "not json",
            "[]",
            r#"{"packages":{}}"#,
            r#"{"packages":[7]}"#,
            r#"{"packages":[{"source":"x","versions":[]}]}"#,
            r#"{"packages":[{"name":"a"}]}"#,
            r#"{"packages":[{"name":"a","versions":[7]}]}"#,
            r#"{"packages":[{"name":"a","versions":[{}]}]}"#,
        ] {
            let error = versions(text, "test").unwrap_err();
            assert_eq!(error.classification(), ExitCode::Integrity, "{text}");
        }
    }

    #[test]
    fn drops_are_the_live_versions_missing_from_the_new_catalog() {
        let live = versions(LIVE, "live").unwrap();
        let local = versions(
            r#"{"packages":[{"name":"b","source":"upstream","versions":[{"version":"2.0.0"}]}]}"#,
            "local",
        )
        .unwrap();
        let dropped: Vec<Version> = live.difference(&local).cloned().collect();
        assert_eq!(
            dropped,
            vec![Version {
                source: "native".to_string(),
                name: "a".to_string(),
                version: "1.0.0".to_string(),
            }]
        );
        assert_eq!(format_drops(&dropped), "  a 1.0.0 (native)");
    }

    #[test]
    fn yanking_is_not_a_drop_and_sources_stay_distinct() {
        let yanked = versions(
            r#"{"packages":[{"name":"a","source":"native","versions":[{"version":"1.0.0","yanked":true}]}]}"#,
            "new",
        )
        .unwrap();
        assert_eq!(yanked.len(), 1);
        let other_source = versions(
            r#"{"packages":[{"name":"a","source":"upstream","versions":[{"version":"1.0.0"}]}]}"#,
            "new",
        )
        .unwrap();
        assert!(yanked.is_disjoint(&other_source));
    }

    #[test]
    fn long_drop_lists_are_truncated_like_the_oracle() {
        let dropped: Vec<Version> = (0..25)
            .map(|index| Version {
                source: String::new(),
                name: format!("pkg-{index}"),
                version: "1.0.0".to_string(),
            })
            .collect();
        let text = format_drops(&dropped);
        assert!(text.ends_with("  ... and 5 more"), "{text}");
        assert_eq!(text.lines().count(), 21);
    }
}
