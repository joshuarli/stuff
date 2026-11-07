#![forbid(unsafe_code)]

use quick_xml::events::{BytesRef, Event};
use quick_xml::Reader;

use crate::publish::PublishError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedObject {
    pub key: String,
    pub size: u64,
    pub etag: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPage {
    pub objects: Vec<ListedObject>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for S3Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.code.is_empty(), self.message.is_empty()) {
            (false, false) => write!(formatter, "{}: {}", self.code, self.message),
            (false, true) => formatter.write_str(&self.code),
            (true, false) => formatter.write_str(&self.message),
            (true, true) => formatter.write_str("unspecified S3 error"),
        }
    }
}

pub fn parse_list_objects_v2(body: &[u8]) -> Result<ListPage, PublishError> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    let mut page = ListPage {
        objects: Vec::new(),
        is_truncated: false,
        next_continuation_token: None,
    };
    let mut current: Option<ListedObject> = None;
    let mut saw_root = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                let name = start.local_name().as_ref().to_vec();
                if path.is_empty() {
                    if name.as_slice() != b"ListBucketResult" {
                        return Err(PublishError::Integrity(format!(
                            "list response has an unexpected root <{}>",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    saw_root = true;
                }
                if matches(&path, &["ListBucketResult"]) && name.as_slice() == b"Contents" {
                    current = Some(ListedObject {
                        key: String::new(),
                        size: 0,
                        etag: String::new(),
                    });
                }
                path.push(name);
                text.clear();
            }
            Ok(Event::End(end)) => {
                let name = end.local_name().as_ref().to_vec();
                let parent = path.get(..path.len().saturating_sub(1)).unwrap_or_default();
                let value = text.trim();
                if matches(parent, &["ListBucketResult"]) && name.as_slice() == b"IsTruncated" {
                    page.is_truncated = value.eq_ignore_ascii_case("true");
                } else if matches(parent, &["ListBucketResult"])
                    && name.as_slice() == b"NextContinuationToken"
                    && !value.is_empty()
                {
                    page.next_continuation_token = Some(value.to_string());
                } else if matches(parent, &["ListBucketResult", "Contents"]) {
                    let object = current.as_mut().ok_or_else(|| {
                        PublishError::Integrity(
                            "list response ordered Contents out of place".into(),
                        )
                    })?;
                    if name.as_slice() == b"Key" {
                        object.key = value.to_string();
                    } else if name.as_slice() == b"Size" {
                        object.size = value.parse::<u64>().map_err(|_| {
                            PublishError::Integrity(format!(
                                "list response has an invalid size {value:?}"
                            ))
                        })?;
                    } else if name.as_slice() == b"ETag" {
                        object.etag = value.trim_matches('"').to_string();
                    }
                }
                let closing_contents =
                    matches(parent, &["ListBucketResult"]) && name.as_slice() == b"Contents";
                path.pop();
                text.clear();
                if closing_contents {
                    let object = current.take().ok_or_else(|| {
                        PublishError::Integrity(
                            "list response closed Contents out of place".into(),
                        )
                    })?;
                    if object.key.is_empty() {
                        return Err(PublishError::Integrity(
                            "list response has a Contents entry without a Key".into(),
                        ));
                    }
                    page.objects.push(object);
                }
            }
            Ok(Event::Text(chunk)) => text.push_str(&decode_text(&chunk)?),
            Ok(Event::CData(chunk)) => text.push_str(&decode_cdata(&chunk)?),
            Ok(Event::GeneralRef(reference)) => push_reference(&mut text, &reference)?,
            Ok(Event::Eof) => {
                if !path.is_empty() {
                    return Err(PublishError::Integrity(
                        "list response ended before every element closed".into(),
                    ));
                }
                break;
            }
            Ok(_) => {}
            Err(error) => {
                return Err(PublishError::Integrity(format!(
                    "list response is not valid XML: {error}"
                )));
            }
        }
        buffer.clear();
    }
    if !saw_root {
        return Err(PublishError::Integrity(
            "list response is not a ListBucketResult document".into(),
        ));
    }
    if page.is_truncated && page.next_continuation_token.is_none() {
        return Err(PublishError::Integrity(
            "list response was truncated without a continuation token".into(),
        ));
    }
    Ok(page)
}

pub fn parse_error(body: &[u8]) -> Result<S3Error, PublishError> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut text = String::new();
    let mut error = S3Error { code: String::new(), message: String::new() };
    let mut saw_root = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                let name = start.local_name().as_ref().to_vec();
                if path.is_empty() {
                    if name.as_slice() != b"Error" {
                        return Err(PublishError::Integrity(format!(
                            "S3 error body has an unexpected root <{}>",
                            String::from_utf8_lossy(&name)
                        )));
                    }
                    saw_root = true;
                }
                path.push(name);
                text.clear();
            }
            Ok(Event::End(end)) => {
                let name = end.local_name().as_ref().to_vec();
                let parent = path.get(..path.len().saturating_sub(1)).unwrap_or_default();
                if matches(parent, &["Error"]) {
                    if name.as_slice() == b"Code" {
                        error.code = text.trim().to_string();
                    } else if name.as_slice() == b"Message" {
                        error.message = text.trim().to_string();
                    }
                }
                path.pop();
                text.clear();
            }
            Ok(Event::Text(chunk)) => text.push_str(&decode_text(&chunk)?),
            Ok(Event::CData(chunk)) => text.push_str(&decode_cdata(&chunk)?),
            Ok(Event::GeneralRef(reference)) => push_reference(&mut text, &reference)?,
            Ok(Event::Eof) => {
                if !path.is_empty() {
                    return Err(PublishError::Integrity(
                        "S3 error body ended before every element closed".into(),
                    ));
                }
                break;
            }
            Ok(_) => {}
            Err(error) => {
                return Err(PublishError::Integrity(format!(
                    "S3 error body is not valid XML: {error}"
                )));
            }
        }
        buffer.clear();
    }
    if !saw_root {
        return Err(PublishError::Integrity(
            "S3 error body is not an Error document".into(),
        ));
    }
    Ok(error)
}

fn matches(path: &[Vec<u8>], expected: &[&str]) -> bool {
    path.len() == expected.len()
        && path
            .iter()
            .zip(expected)
            .all(|(actual, wanted)| actual.as_slice() == wanted.as_bytes())
}

fn decode_text(chunk: &quick_xml::events::BytesText<'_>) -> Result<String, PublishError> {
    chunk
        .decode()
        .map(|text| text.into_owned())
        .map_err(|error| PublishError::Integrity(format!("XML text is not valid UTF-8: {error}")))
}

fn decode_cdata(chunk: &quick_xml::events::BytesCData<'_>) -> Result<String, PublishError> {
    chunk
        .decode()
        .map(|text| text.into_owned())
        .map_err(|error| PublishError::Integrity(format!("XML text is not valid UTF-8: {error}")))
}

fn push_reference(text: &mut String, reference: &BytesRef<'_>) -> Result<(), PublishError> {
    if let Some(character) = reference.resolve_char_ref().map_err(|error| {
        PublishError::Integrity(format!("XML character reference is invalid: {error}"))
    })? {
        text.push(character);
        return Ok(());
    }
    match reference.as_ref() {
        b"quot" => text.push('"'),
        b"amp" => text.push('&'),
        b"lt" => text.push('<'),
        b"gt" => text.push('>'),
        b"apos" => text.push('\''),
        other => {
            return Err(PublishError::Integrity(format!(
                "XML uses an unsupported entity &{};",
                String::from_utf8_lossy(other)
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::ExitCode;

    #[test]
    fn parses_a_paginated_listing_with_escaped_values() {
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
            <ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
              <IsTruncated>true</IsTruncated>
              <Contents>
                <Key>blobs/a&amp;b</Key>
                <Size>8</Size>
                <ETag>&quot;0123456789abcdef0123456789abcdef&quot;</ETag>
              </Contents>
              <NextContinuationToken>blobs/a&amp;b</NextContinuationToken>
            </ListBucketResult>"#;
        let page = parse_list_objects_v2(body).unwrap();
        assert!(page.is_truncated);
        assert_eq!(page.next_continuation_token.as_deref(), Some("blobs/a&b"));
        assert_eq!(
            page.objects,
            vec![ListedObject {
                key: "blobs/a&b".to_string(),
                size: 8,
                etag: "0123456789abcdef0123456789abcdef".to_string(),
            }]
        );
    }

    #[test]
    fn parses_an_s3_error_document() {
        let error = parse_error(
            b"<?xml version=\"1.0\"?><Error><Code>SignatureDoesNotMatch</Code><Message>bad key</Message></Error>",
        )
        .unwrap();
        assert_eq!(error.code, "SignatureDoesNotMatch");
        assert_eq!(error.message, "bad key");
        assert_eq!(error.to_string(), "SignatureDoesNotMatch: bad key");
    }

    #[test]
    fn malformed_xml_is_an_integrity_error() {
        let error = parse_list_objects_v2(b"<ListBucketResult><Contents>").unwrap_err();
        assert_eq!(error.classification(), ExitCode::Integrity);
        let error = parse_error(b"not xml").unwrap_err();
        assert_eq!(error.classification(), ExitCode::Integrity);
    }

    #[test]
    fn a_truncated_page_without_a_token_is_an_integrity_error() {
        let error = parse_list_objects_v2(
            b"<ListBucketResult><IsTruncated>true</IsTruncated></ListBucketResult>",
        )
        .unwrap_err();
        assert_eq!(error.classification(), ExitCode::Integrity);
    }
}
