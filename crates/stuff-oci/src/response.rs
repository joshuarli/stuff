#![forbid(unsafe_code)]

use crate::error::{Error, ErrorCode};
use crate::json;
use crate::manifest::Descriptor;
use crate::media;
use crate::reference::{Reference, RepositoryName, Tag};
use crate::referrers::{filter_referrers, render_referrers};
use crate::request::{Method, Pagination};
use crate::Digest;

const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";
const CACHE_POINTER: &str = "public, max-age=60, stale-if-error=0";
const CACHE_ERROR: &str = "no-store";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Empty,
    Inline(Vec<u8>),
    Opaque { length: u64 },
    Range { offset: u64, length: u64 },
}

impl Body {
    pub fn len(&self) -> u64 {
        match self {
            Body::Empty => 0,
            Body::Inline(bytes) => bytes.len() as u64,
            Body::Opaque { length } | Body::Range { length, .. } => *length,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsePlan {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl ResponsePlan {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn push_header(&mut self, name: &str, value: impl Into<String>) {
        self.headers.push((name.to_string(), value.into()));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestHeaders {
    pub range: Option<String>,
}

pub fn plan_ping() -> ResponsePlan {
    let body = b"{}".to_vec();
    let mut plan = ResponsePlan { status: 200, headers: Vec::new(), body: Body::Inline(body) };
    plan.push_header("content-type", "application/json");
    plan.push_header("content-length", "2");
    plan.push_header("docker-distribution-api-version", "registry/2.0");
    plan.push_header("cache-control", CACHE_POINTER);
    plan
}

pub fn plan_error(error: &Error) -> ResponsePlan {
    let body = error.to_json().into_bytes();
    let status = error.status();
    let mut plan = ResponsePlan { status, headers: Vec::new(), body: Body::Inline(body) };
    plan.push_header("content-type", "application/json");
    plan.push_header("content-length", plan.body.len().to_string());
    plan.push_header("cache-control", CACHE_ERROR);
    plan
}

pub fn plan_method_not_allowed() -> ResponsePlan {
    let mut plan = plan_error(&Error::unsupported("method not allowed"));
    plan.push_header("allow", "GET, HEAD");
    plan
}

pub fn plan_manifest(
    method: Method,
    reference: &Reference,
    media_type: &str,
    digest: &Digest,
    size: u64,
) -> ResponsePlan {
    if !method.is_read() {
        return plan_method_not_allowed();
    }
    let mut plan = ResponsePlan { status: 200, headers: Vec::new(), body: Body::Empty };
    plan.push_header("content-type", media_type);
    plan.push_header("content-length", size.to_string());
    plan.push_header("docker-content-digest", digest.as_str());
    plan.push_header("etag", json::quoted(digest.as_str()));
    plan.push_header(
        "cache-control",
        if reference.is_digest() { CACHE_IMMUTABLE } else { CACHE_POINTER },
    );
    if method == Method::Get {
        plan.body = Body::Opaque { length: size };
    }
    plan
}

pub fn plan_blob(
    method: Method,
    digest: &Digest,
    size: u64,
    headers: &RequestHeaders,
) -> ResponsePlan {
    if !method.is_read() {
        return plan_method_not_allowed();
    }
    let mut base = ResponsePlan { status: 200, headers: Vec::new(), body: Body::Empty };
    base.push_header("content-type", "application/octet-stream");
    base.push_header("docker-content-digest", digest.as_str());
    base.push_header("etag", json::quoted(digest.as_str()));
    base.push_header("accept-ranges", "bytes");
    base.push_header("cache-control", CACHE_IMMUTABLE);
    if method == Method::Head {
        base.push_header("content-length", size.to_string());
        return base;
    }
    match headers.range.as_deref().map(|range| parse_single_range(range, size)) {
        Some(SingleRange::Partial { start, end }) => {
            let length = end - start + 1;
            base.status = 206;
            base.push_header("content-length", length.to_string());
            base.push_header("content-range", format!("bytes {start}-{end}/{size}"));
            base.body = Body::Range { offset: start, length };
        }
        Some(SingleRange::Unsatisfiable) => {
            base.status = 416;
            base.push_header("content-length", "0");
            base.push_header("content-range", format!("bytes */{size}"));
        }
        Some(SingleRange::Full) | None => {
            base.push_header("content-length", size.to_string());
            base.body = Body::Opaque { length: size };
        }
    }
    base
}

enum SingleRange {
    Full,
    Partial { start: u64, end: u64 },
    Unsatisfiable,
}

fn parse_single_range(header: &str, size: u64) -> SingleRange {
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return SingleRange::Full;
    };
    let spec = spec.trim();
    if spec.is_empty() || spec.contains(',') {
        return SingleRange::Full;
    }
    let Some((first, second)) = spec.split_once('-') else {
        return SingleRange::Full;
    };
    if first.is_empty() {
        let Ok(suffix) = second.parse::<u64>() else {
            return SingleRange::Full;
        };
        if suffix == 0 {
            return SingleRange::Full;
        }
        if size == 0 {
            return SingleRange::Unsatisfiable;
        }
        let start = size.saturating_sub(suffix);
        return SingleRange::Partial { start, end: size - 1 };
    }
    let Ok(start) = first.parse::<u64>() else {
        return SingleRange::Full;
    };
    if start >= size {
        return SingleRange::Unsatisfiable;
    }
    let end = if second.is_empty() {
        size - 1
    } else {
        let Ok(end) = second.parse::<u64>() else {
            return SingleRange::Full;
        };
        if end < start {
            return SingleRange::Unsatisfiable;
        }
        end.min(size - 1)
    };
    SingleRange::Partial { start, end }
}

pub fn plan_tags(
    method: Method,
    repository: &RepositoryName,
    tags: &[Tag],
    pagination: &Pagination,
) -> ResponsePlan {
    if !method.is_read() {
        return plan_method_not_allowed();
    }
    let mut sorted: Vec<&Tag> = tags.iter().collect();
    sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let filtered: Vec<&Tag> = match &pagination.last {
        Some(last) => sorted
            .into_iter()
            .filter(|tag| tag.as_str() > last.as_str())
            .collect(),
        None => sorted,
    };
    let page: Vec<&Tag> = match pagination.n {
        Some(n) => filtered.iter().take(n).copied().collect(),
        None => filtered.clone(),
    };
    let has_more = filtered.len() > page.len();
    let body = render_tags_json(repository, &page);
    let length = body.len();
    let mut plan = ResponsePlan {
        status: 200,
        headers: Vec::new(),
        body: if method == Method::Head { Body::Empty } else { Body::Inline(body) },
    };
    plan.push_header("content-type", "application/json");
    plan.push_header("content-length", length.to_string());
    plan.push_header("cache-control", CACHE_POINTER);
    if has_more && pagination.n.unwrap_or(0) > 0
        && let Some(last) = page.last()
    {
        plan.push_header(
            "link",
            format!(
                "</v2/{}/tags/list?n={}&last={}>; rel=\"next\"",
                repository.as_str(),
                page.len(),
                encode_query_value(last.as_str())
            ),
        );
    }
    plan
}

fn render_tags_json(repository: &RepositoryName, tags: &[&Tag]) -> Vec<u8> {
    let mut out = String::from("{\"name\":");
    out.push_str(&json::quoted(repository.as_str()));
    out.push_str(",\"tags\":[");
    for (i, tag) in tags.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json::quoted(tag.as_str()));
    }
    out.push_str("]}");
    out.into_bytes()
}

pub fn plan_referrers(
    method: Method,
    repository: &RepositoryName,
    subject: &Digest,
    descriptors: &[Descriptor],
    artifact_type: Option<&str>,
    pagination: &Pagination,
) -> ResponsePlan {
    if !method.is_read() {
        return plan_method_not_allowed();
    }
    let filtered = filter_referrers(descriptors, artifact_type);
    let filtered: Vec<Descriptor> = match &pagination.last {
        Some(last) => filtered
            .into_iter()
            .filter(|descriptor| descriptor.digest.as_str() > last.as_str())
            .collect(),
        None => filtered,
    };
    let page: Vec<Descriptor> = match pagination.n {
        Some(n) => filtered.iter().take(n).cloned().collect(),
        None => filtered.clone(),
    };
    let has_more = filtered.len() > page.len();
    let body = render_referrers(&page);
    let length = body.len();
    let mut plan = ResponsePlan {
        status: 200,
        headers: Vec::new(),
        body: if method == Method::Head { Body::Empty } else { Body::Inline(body.into_bytes()) },
    };
    plan.push_header("content-type", media::OCI_INDEX);
    plan.push_header("content-length", length.to_string());
    plan.push_header("cache-control", CACHE_POINTER);
    if artifact_type.is_some() {
        plan.push_header("oci-filters-applied", "artifactType");
    }
    if has_more && pagination.n.unwrap_or(0) > 0
        && let Some(last) = page.last()
    {
        let mut link = format!(
            "</v2/{}/referrers/{}?",
            repository.as_str(),
            subject.as_str()
        );
        if let Some(artifact_type) = artifact_type {
            link.push_str("artifactType=");
            link.push_str(&encode_query_value(artifact_type));
            link.push('&');
        }
        link.push_str(&format!(
            "n={}&last={}>; rel=\"next\"",
            page.len(),
            encode_query_value(last.digest.as_str())
        ));
        plan.push_header("link", link);
    }
    plan
}

fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn empty_write_plan(status: u16) -> ResponsePlan {
    let mut plan = ResponsePlan { status, headers: Vec::new(), body: Body::Empty };
    plan.push_header("docker-distribution-api-version", "registry/2.0");
    plan.push_header("content-length", "0");
    plan.push_header("cache-control", "no-store");
    plan
}

fn range_header(offset: u64) -> String {
    if offset == 0 {
        "0-0".to_string()
    } else {
        format!("0-{}", offset - 1)
    }
}

pub fn plan_auth_required() -> ResponsePlan {
    let mut plan = plan_error(&Error::new(ErrorCode::Unauthorized, "authentication required"));
    plan.push_header("www-authenticate", "Basic realm=\"stuff-oci\"");
    plan
}

pub fn plan_upload_start(location: &str, upload_id: &str) -> ResponsePlan {
    let mut plan = empty_write_plan(202);
    plan.push_header("location", location);
    plan.push_header("docker-upload-uuid", upload_id);
    plan.push_header("range", range_header(0));
    plan
}

pub fn plan_upload_mounted(blob_location: &str, digest: &Digest) -> ResponsePlan {
    let mut plan = empty_write_plan(201);
    plan.push_header("location", blob_location);
    plan.push_header("docker-content-digest", digest.as_str());
    plan
}

pub fn plan_upload_status(upload_id: &str, offset: u64) -> ResponsePlan {
    let mut plan = empty_write_plan(204);
    plan.push_header("docker-upload-uuid", upload_id);
    plan.push_header("range", range_header(offset));
    plan
}

pub fn plan_upload_patch(location: &str, upload_id: &str, offset: u64) -> ResponsePlan {
    let mut plan = empty_write_plan(202);
    plan.push_header("location", location);
    plan.push_header("docker-upload-uuid", upload_id);
    plan.push_header("range", range_header(offset));
    plan
}

pub fn plan_upload_complete(blob_location: &str, digest: &Digest) -> ResponsePlan {
    let mut plan = empty_write_plan(201);
    plan.push_header("location", blob_location);
    plan.push_header("docker-content-digest", digest.as_str());
    plan
}

pub fn plan_upload_cancel() -> ResponsePlan {
    empty_write_plan(204)
}

pub fn plan_manifest_put(
    manifest_location: &str,
    digest: &Digest,
    subject: Option<&Digest>,
) -> ResponsePlan {
    let mut plan = empty_write_plan(201);
    plan.push_header("location", manifest_location);
    plan.push_header("docker-content-digest", digest.as_str());
    if let Some(subject) = subject {
        plan.push_header("oci-subject", subject.as_str());
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::request::{parse_request, Pagination, Parsed, Request};
    use std::collections::BTreeMap;

    const INDEX_DIGEST: &str =
        "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388";
    const ARM64_DIGEST: &str =
        "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
    const LAYER_DIGEST: &str =
        "sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f";
    const SUBJECT: &str = "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";

    fn repository() -> RepositoryName {
        RepositoryName::parse("example/app").unwrap()
    }

    fn descriptor(digest: &str, media_type: &str, size: u64, artifact_type: Option<&str>) -> Descriptor {
        Descriptor {
            media_type: media_type.to_string(),
            digest: Digest::parse(digest).unwrap(),
            size,
            artifact_type: artifact_type.map(str::to_string),
            annotations: BTreeMap::new(),
            platform: None,
            urls: Vec::new(),
        }
    }

    fn request(target: &str) -> Request {
        match parse_request(Method::Get, target) {
            Parsed::Request(request) => request,
            other => panic!("expected request, got {other:?}"),
        }
    }

    fn tags(names: &[&str]) -> Vec<Tag> {
        names.iter().map(|name| Tag::parse(name).unwrap()).collect()
    }

    #[test]
    fn ping() {
        assert_eq!(request("/v2/"), Request::Ping);
        let plan = plan_ping();
        assert_eq!(plan.status, 200);
        assert_eq!(plan.header("docker-distribution-api-version"), Some("registry/2.0"));
        assert_eq!(plan.body, Body::Inline(b"{}".to_vec()));
    }

    #[test]
    fn manifest_plans_follow_method_and_reference_kind() {
        let by_digest = format!("/v2/example/app/manifests/{ARM64_DIGEST}");
        let index_by_digest = format!("/v2/example/app/manifests/{INDEX_DIGEST}");
        let cases = [
            (Method::Get, "/v2/example/app/manifests/stable", media::OCI_INDEX, INDEX_DIGEST, 491, CACHE_POINTER),
            (Method::Head, "/v2/example/app/manifests/stable", media::OCI_INDEX, INDEX_DIGEST, 491, CACHE_POINTER),
            (Method::Get, by_digest.as_str(), media::OCI_MANIFEST, ARM64_DIGEST, 401, CACHE_IMMUTABLE),
            (Method::Head, by_digest.as_str(), media::OCI_MANIFEST, ARM64_DIGEST, 401, CACHE_IMMUTABLE),
            (Method::Get, index_by_digest.as_str(), media::OCI_INDEX, INDEX_DIGEST, 491, CACHE_IMMUTABLE),
        ];
        for (method, target, media_type, digest, size, cache) in cases {
            let Request::Manifest { reference, head, .. } = request(target) else {
                panic!("{target} is not a manifest route")
            };
            assert!(!head, "{target}");
            let plan = plan_manifest(method, &reference, media_type, &Digest::parse(digest).unwrap(), size);
            assert_eq!(plan.status, 200, "{method:?} {target}");
            assert_eq!(plan.header("content-type"), Some(media_type), "{method:?} {target}");
            assert_eq!(plan.header("docker-content-digest"), Some(digest), "{method:?} {target}");
            assert_eq!(plan.header("cache-control"), Some(cache), "{method:?} {target}");
            let expected = if method == Method::Head { Body::Empty } else { Body::Opaque { length: size } };
            assert_eq!(plan.body, expected, "{method:?} {target}");
            if method == Method::Head {
                assert_eq!(plan.header("content-length"), Some(size.to_string().as_str()), "{target}");
            }
        }
    }

    #[test]
    fn blob_plans_cover_get_head_and_ranges() {
        let Request::Blob { digest, head, .. } =
            request(&format!("/v2/example/app/blobs/{LAYER_DIGEST}"))
        else {
            panic!("blob")
        };
        assert!(!head);
        let plan = plan_blob(Method::Get, &digest, 117, &RequestHeaders::default());
        assert_eq!(plan.status, 200);
        assert_eq!(plan.header("content-type"), Some("application/octet-stream"));
        assert_eq!(plan.header("content-length"), Some("117"));
        assert_eq!(plan.header("docker-content-digest"), Some(LAYER_DIGEST));
        assert_eq!(plan.header("accept-ranges"), Some("bytes"));
        assert_eq!(plan.body, Body::Opaque { length: 117 });

        let plan = plan_blob(Method::Head, &digest, 117, &RequestHeaders::default());
        assert_eq!(plan.status, 200);
        assert_eq!(plan.header("content-length"), Some("117"));
        assert_eq!(plan.body, Body::Empty);

        let headers = RequestHeaders { range: Some("bytes=0-9".to_string()) };
        let plan = plan_blob(Method::Get, &digest, 117, &headers);
        assert_eq!(plan.status, 206);
        assert_eq!(plan.header("content-range"), Some("bytes 0-9/117"));
        assert_eq!(plan.header("content-length"), Some("10"));
        assert_eq!(plan.body, Body::Range { offset: 0, length: 10 });

        let headers = RequestHeaders { range: Some("bytes=117-".to_string()) };
        let plan = plan_blob(Method::Get, &digest, 117, &headers);
        assert_eq!(plan.status, 416);
        assert_eq!(plan.header("content-range"), Some("bytes */117"));

        let headers = RequestHeaders { range: Some("bytes=-10".to_string()) };
        let plan = plan_blob(Method::Get, &digest, 117, &headers);
        assert_eq!(plan.status, 206);
        assert_eq!(plan.header("content-range"), Some("bytes 107-116/117"));
    }

    #[test]
    fn tags_lists_sorted_and_paginated() {
        assert_eq!(request("/v2/example/app/tags/list"), Request::Tags {
            repository: repository(),
            pagination: Pagination::default(),
        });
        let plan = plan_tags(Method::Get, &repository(), &tags(&["stable", "beta"]), &Pagination::default());
        assert_eq!(plan.status, 200);
        assert_eq!(plan.header("content-type"), Some("application/json"));
        let body = match plan.body {
            Body::Inline(body) => String::from_utf8(body).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert_eq!(body, "{\"name\":\"example/app\",\"tags\":[\"beta\",\"stable\"]}");

        let all = tags(&["a", "b", "c"]);
        let first = plan_tags(
            Method::Get,
            &repository(),
            &all,
            &Pagination { n: Some(2), last: None },
        );
        let body = match &first.body {
            Body::Inline(body) => String::from_utf8(body.clone()).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert_eq!(body, "{\"name\":\"example/app\",\"tags\":[\"a\",\"b\"]}");
        assert_eq!(
            first.header("link"),
            Some("</v2/example/app/tags/list?n=2&last=b>; rel=\"next\"")
        );
        let second = plan_tags(
            Method::Get,
            &repository(),
            &all,
            &Pagination { n: Some(2), last: Some("b".to_string()) },
        );
        let body = match &second.body {
            Body::Inline(body) => String::from_utf8(body.clone()).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert_eq!(body, "{\"name\":\"example/app\",\"tags\":[\"c\"]}");
        assert_eq!(second.header("link"), None);

        let head = plan_tags(Method::Head, &repository(), &all, &Pagination::default());
        assert_eq!(head.status, 200);
        assert_eq!(head.body, Body::Empty);
    }

    #[test]
    fn referrers_list_and_filter_by_artifact_type() {
        let descriptors = vec![
            descriptor(ARM64_DIGEST, media::OCI_MANIFEST, 401, None),
            descriptor(
                "sha256:3a91f0fb91009745e3a72651ec98d02a4c758c4f580bd9ab8683467f1f264f25",
                media::OCI_MANIFEST,
                671,
                Some("application/vnd.stuff.sbom.v1+json"),
            ),
        ];
        let subject = Digest::parse(SUBJECT).unwrap();
        let plan = plan_referrers(Method::Get, &repository(), &subject, &descriptors, None, &Pagination::default());
        assert_eq!(plan.status, 200);
        assert_eq!(plan.header("content-type"), Some(media::OCI_INDEX));
        assert_eq!(plan.header("oci-filters-applied"), None);
        let body = match plan.body {
            Body::Inline(body) => String::from_utf8(body).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert!(body.contains("\"schemaVersion\":2"));
        assert!(body.contains(&format!("\"digest\":\"{ARM64_DIGEST}\"")));

        let plan = plan_referrers(
            Method::Get,
            &repository(),
            &subject,
            &descriptors,
            Some("application/vnd.stuff.sbom.v1+json"),
            &Pagination::default(),
        );
        assert_eq!(plan.header("oci-filters-applied"), Some("artifactType"));
        let body = match plan.body {
            Body::Inline(body) => String::from_utf8(body).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert!(body.contains("3a91f0fb"));
        assert!(!body.contains(ARM64_DIGEST));
    }

    #[test]
    fn unsupported() {
        assert_eq!(
            parse_request(Method::Post, "/v2/example/app/blobs/uploads/"),
            Parsed::MethodNotAllowed
        );
        let plan = plan_method_not_allowed();
        assert_eq!(plan.status, 405);
        assert_eq!(plan.header("allow"), Some("GET, HEAD"));
        let body = match plan.body {
            Body::Inline(body) => String::from_utf8(body).unwrap(),
            other => panic!("unexpected body {other:?}"),
        };
        assert!(body.contains("UNSUPPORTED"));
    }

    #[test]
    fn upload_plans_and_auth_challenge() {
        let auth = plan_auth_required();
        assert_eq!(auth.status, 401);
        assert_eq!(auth.header("www-authenticate"), Some("Basic realm=\"stuff-oci\""));

        let start = plan_upload_start("https://writer.example/v2/example/app/blobs/uploads/u1?session=t", "u1");
        assert_eq!(start.status, 202);
        assert_eq!(start.header("docker-upload-uuid"), Some("u1"));
        assert_eq!(start.header("range"), Some("0-0"));
        assert_eq!(start.header("docker-distribution-api-version"), Some("registry/2.0"));

        let mounted = plan_upload_mounted(
            "/v2/example/app/blobs/sha256:00",
            &Digest::parse(INDEX_DIGEST).unwrap(),
        );
        assert_eq!(mounted.status, 201);
        assert_eq!(mounted.header("docker-content-digest"), Some(INDEX_DIGEST));

        let status = plan_upload_status("u1", 12);
        assert_eq!(status.status, 204);
        assert_eq!(status.header("range"), Some("0-11"));

        let patch = plan_upload_patch("https://writer.example/u1", "u1", 12);
        assert_eq!(patch.status, 202);
        assert_eq!(patch.header("range"), Some("0-11"));

        let complete = plan_upload_complete(
            "/v2/example/app/blobs/sha256:00",
            &Digest::parse(INDEX_DIGEST).unwrap(),
        );
        assert_eq!(complete.status, 201);
        assert_eq!(complete.header("docker-content-digest"), Some(INDEX_DIGEST));
        assert_eq!(plan_upload_cancel().status, 204);

        let manifest = plan_manifest_put(
            "/v2/example/app/manifests/stable",
            &Digest::parse(INDEX_DIGEST).unwrap(),
            None,
        );
        assert_eq!(manifest.status, 201);
        assert_eq!(manifest.header("docker-content-digest"), Some(INDEX_DIGEST));
    }

    #[test]
    fn invalid_input() {
        let Parsed::Invalid(error) = parse_request(Method::Get, "/v2/Example/app/manifests/stable") else {
            panic!("invalid name")
        };
        assert_eq!(error.code, ErrorCode::NameInvalid);
        let Parsed::Invalid(error) = parse_request(Method::Get, "/v2/example/app/manifests/sha256:00") else {
            panic!("invalid digest")
        };
        assert_eq!(error.code, ErrorCode::DigestInvalid);
        let Parsed::Invalid(error) = parse_request(Method::Get, "/v2/example/app/manifests/-bad") else {
            panic!("invalid tag")
        };
        assert_eq!(error.code, ErrorCode::TagInvalid);
        let Parsed::Invalid(error) = parse_request(Method::Get, "/v2/example/app/tags/list?n=-1") else {
            panic!("invalid pagination")
        };
        assert_eq!(error.code, ErrorCode::PaginationNumberInvalid);
        assert_eq!(plan_error(&error).status, 400);
    }
}
