#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use crate::publish::PublishError;

use super::client::{Precondition, R2Client, RawRequest};
use super::manifest::{self, Manifest};
use super::pull::is_generation_id;
use super::xml::ListedObject;

pub const CURRENT_KEY: &str = "current";
pub const MANIFEST_DIRECTORY: &str = "manifests/";
pub const MAX_GENERATION_LENGTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub generation: String,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub generation: String,
    pub promoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionAttempt {
    Promoted(Promotion),
    Precondition,
}

pub fn validate_generation(generation: &str) -> Result<(), PublishError> {
    if generation.len() > MAX_GENERATION_LENGTH || !is_generation_id(generation) {
        return Err(PublishError::Usage(format!(
            "bad generation id: {generation}"
        )));
    }
    Ok(())
}

pub fn read_marker(
    client: &R2Client,
    bucket: &str,
) -> Result<Option<Marker>, PublishError> {
    let response = client.request(RawRequest {
        method: "GET",
        bucket,
        key: CURRENT_KEY,
        query: Vec::new(),
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    })?;
    match response.status {
        200 => {
            let generation = String::from_utf8_lossy(&response.body)
                .trim()
                .to_string();
            let etag = response.header("etag").unwrap_or_default().to_string();
            if etag.is_empty() {
                return Err(PublishError::Integrity(format!(
                    "GET {bucket}/{CURRENT_KEY}: response has no ETag"
                )));
            }
            Ok(Some(Marker { generation, etag }))
        }
        404 => Ok(None),
        status => Err(PublishError::Integrity(format!(
            "could not read the current marker: HTTP {status}"
        ))),
    }
}

pub fn precondition(marker: Option<&Marker>) -> Precondition {
    match marker {
        None => Precondition::IfNoneMatchAny,
        Some(marker) => Precondition::IfMatch(marker.etag.clone()),
    }
}

pub fn check_expected(
    marker: Option<&Marker>,
    expected: Option<&str>,
    generation: &str,
) -> Result<(), PublishError> {
    let Some(expected) = expected else {
        return Ok(());
    };
    let current = marker.map(|marker| marker.generation.as_str());
    if current != Some(expected) && current != Some(generation) {
        return Err(PublishError::Conflict(format!(
            "remote current is '{}', expected '{expected}'; refusing to overwrite a competing publication",
            current.unwrap_or("<missing>")
        )));
    }
    Ok(())
}

pub fn verify_generation(
    client: &R2Client,
    bucket: &str,
    generation: &str,
) -> Result<Manifest, PublishError> {
    validate_generation(generation)?;
    let key = format!("{MANIFEST_DIRECTORY}{generation}.json");
    let response = client.request(RawRequest {
        method: "GET",
        bucket,
        key: &key,
        query: Vec::new(),
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    })?;
    if response.status == 404 {
        return Err(PublishError::Integrity(format!(
            "remote generation {generation} has no {key}"
        )));
    }
    if response.status != 200 {
        return Err(PublishError::Integrity(format!(
            "GET {bucket}/{key}: HTTP {}",
            response.status
        )));
    }
    let text = String::from_utf8_lossy(&response.body);
    let manifest = manifest::parse(text.as_ref())?;
    let remote: BTreeMap<String, ListedObject> = client
        .list_objects_v2(bucket)?
        .into_iter()
        .map(|object| (object.key.clone(), object))
        .collect();
    for object in &manifest.objects {
        match remote.get(&object.key) {
            None => {
                return Err(PublishError::Integrity(format!(
                    "generation is incomplete: missing {}",
                    object.key
                )));
            }
            Some(found) if found.size != object.size => {
                return Err(PublishError::Integrity(format!(
                    "generation object has the wrong size: {}",
                    object.key
                )));
            }
            Some(found) if found.etag != object.etag => {
                return Err(PublishError::Integrity(format!(
                    "generation object has the wrong ETag: {}",
                    object.key
                )));
            }
            Some(_) => {}
        }
    }
    Ok(manifest)
}

pub fn promote(
    client: &R2Client,
    bucket: &str,
    generation: &str,
    expected: Option<&str>,
) -> Result<Promotion, PublishError> {
    match promote_attempt(client, bucket, generation, expected)? {
        PromotionAttempt::Promoted(promotion) => Ok(promotion),
        PromotionAttempt::Precondition => Err(PublishError::Conflict(
            "current changed during promotion; refusing to overwrite a competing publication"
                .to_string(),
        )),
    }
}

pub fn promote_attempt(
    client: &R2Client,
    bucket: &str,
    generation: &str,
    expected: Option<&str>,
) -> Result<PromotionAttempt, PublishError> {
    validate_generation(generation)?;
    let marker = read_marker(client, bucket)?;
    check_expected(marker.as_ref(), expected, generation)?;
    verify_generation(client, bucket, generation)?;
    if marker
        .as_ref()
        .is_some_and(|marker| marker.generation == generation)
    {
        return Ok(PromotionAttempt::Promoted(Promotion {
            generation: generation.to_string(),
            promoted: false,
        }));
    }
    let condition = precondition(marker.as_ref());
    match client.put_object(
        bucket,
        CURRENT_KEY,
        generation.as_bytes(),
        "text/plain",
        condition,
    ) {
        Ok(_) => {}
        Err(PublishError::Conflict(_)) => return Ok(PromotionAttempt::Precondition),
        Err(error) => return Err(error),
    }
    let committed = read_marker(client, bucket)?;
    let actual = committed
        .as_ref()
        .map(|marker| marker.generation.as_str());
    if actual != Some(generation) {
        return Err(PublishError::Integrity(format!(
            "current is '{}' after promotion, expected '{generation}'",
            actual.unwrap_or("<missing>")
        )));
    }
    Ok(PromotionAttempt::Promoted(Promotion {
        generation: generation.to_string(),
        promoted: true,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;

    fn marker(etag: &str) -> Marker {
        Marker {
            generation: "g0".to_string(),
            etag: etag.to_string(),
        }
    }

    #[test]
    fn the_first_publication_uses_if_none_match_star() {
        assert_eq!(precondition(None), Precondition::IfNoneMatchAny);
    }

    #[test]
    fn an_existing_marker_uses_if_match_on_the_observed_etag() {
        assert_eq!(
            precondition(Some(&marker("\"e0\""))),
            Precondition::IfMatch("\"e0\"".to_string())
        );
    }

    #[test]
    fn a_stale_or_competing_expected_generation_is_a_conflict() {
        let error = check_expected(Some(&marker("\"e0\"")), Some("g4"), "g6").unwrap_err();
        assert_eq!(error.classification(), ExitCode::Conflict);
        assert_eq!(error.exit_code(), 3);
        assert!(
            error
                .to_string()
                .contains("remote current is 'g0', expected 'g4'"),
            "{error}"
        );
        let missing = check_expected(None, Some("g4"), "g6").unwrap_err();
        assert!(missing.to_string().contains("'<missing>'"), "{missing}");
    }

    #[test]
    fn expected_accepts_the_recorded_or_the_requested_generation() {
        check_expected(Some(&marker("\"e0\"")), Some("g0"), "g1").unwrap();
        let requested = Marker {
            generation: "g1".to_string(),
            etag: "\"e0\"".to_string(),
        };
        check_expected(Some(&requested), Some("g1"), "g1").unwrap();
        check_expected(Some(&marker("\"e0\"")), None, "g1").unwrap();
    }

    #[test]
    fn generation_ids_are_bounded_and_alphanumeric() {
        let longest = "a".repeat(MAX_GENERATION_LENGTH);
        let too_long = "a".repeat(MAX_GENERATION_LENGTH + 1);
        for good in ["g1", "aZ0_-", longest.as_str()] {
            assert!(validate_generation(good).is_ok(), "{good}");
        }
        for bad in ["", "has space", "slash/", "dot.", too_long.as_str()] {
            let error = validate_generation(bad).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{bad}");
        }
    }
}
