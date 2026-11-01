#![forbid(unsafe_code)]

use stuff_oci::{
    filter_referrers, parse_referrers, parse_repository_json, plan_blob, plan_error,
    plan_manifest, plan_method_not_allowed, plan_ping, plan_referrers, plan_tags, Body, Digest,
    Method, Pagination, Parsed, Reference, RepositoryName, Request, RequestHeaders, ResponsePlan,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct OciRequestJson {
    pub kind: String,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub repository: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub reference: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub digest: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub subject: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub artifact_type: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub n: Option<usize>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub last: Option<String>,
    #[deser(default)]
    pub head: bool,
    #[deser(default)]
    pub status: u16,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub body: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, deser::Serialize, deser::Deserialize)]
pub struct OciPlanJson {
    pub kind: String,
    #[deser(default)]
    pub status: u16,
    #[deser(default)]
    pub headers: BTreeMap<String, String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub body: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub bucket: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub key: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub range_start: Option<u64>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub range_length: Option<u64>,
    #[deser(default)]
    pub head: bool,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub message: Option<String>,
}

fn to_json<T: deser::Serialize + ?Sized>(value: &T, fallback_kind: &str) -> String {
    deser_json::to_string(value).unwrap_or_else(|_| format!(r#"{{"kind":"{fallback_kind}"}}"#))
}

fn request(kind: &str) -> OciRequestJson {
    OciRequestJson {
        kind: kind.to_string(),
        repository: None,
        reference: None,
        digest: None,
        subject: None,
        artifact_type: None,
        n: None,
        last: None,
        head: false,
        status: 0,
        body: None,
        message: None,
    }
}

pub fn oci_request_json(method: &str, raw_target: &str) -> String {
    let parsed = stuff_oci::parse_request(Method::parse(method), raw_target);
    match parsed {
        Parsed::Request(Request::Ping) => to_json(&request("ping"), "error"),
        Parsed::Request(Request::Manifest { repository, reference, head }) => {
            let mut value = request("manifest");
            value.repository = Some(repository.as_str().to_string());
            value.reference = Some(reference.as_str().to_string());
            value.head = head;
            to_json(&value, "error")
        }
        Parsed::Request(Request::Blob { repository, digest, head }) => {
            let mut value = request("blob");
            value.repository = Some(repository.as_str().to_string());
            value.digest = Some(digest.as_str().to_string());
            value.head = head;
            to_json(&value, "error")
        }
        Parsed::Request(Request::Tags { repository, pagination }) => {
            let mut value = request("tags");
            value.repository = Some(repository.as_str().to_string());
            value.n = pagination.n;
            value.last = pagination.last;
            to_json(&value, "error")
        }
        Parsed::Request(Request::Referrers { repository, subject, artifact_type, pagination }) => {
            let mut value = request("referrers");
            value.repository = Some(repository.as_str().to_string());
            value.subject = Some(subject.as_str().to_string());
            value.artifact_type = artifact_type;
            value.n = pagination.n;
            value.last = pagination.last;
            to_json(&value, "error")
        }
        Parsed::MethodNotAllowed => to_json(&request("method_not_allowed"), "error"),
        Parsed::NotFound => to_json(&request("not_found"), "error"),
        Parsed::Invalid(error) => {
            let mut value = request("error");
            value.status = error.status();
            value.body = Some(error.to_json());
            to_json(&value, "error")
        }
    }
}

pub(crate) fn plan_to_json(plan: ResponsePlan, bucket: Option<&str>, key: Option<String>, head: bool) -> String {
    let headers: BTreeMap<String, String> = plan.headers.into_iter().collect();
    let (body, range_start, range_length) = match plan.body {
        Body::Empty => (None, None, None),
        Body::Inline(bytes) => (Some(String::from_utf8_lossy(&bytes).into_owned()), None, None),
        Body::Opaque { .. } => (None, None, None),
        Body::Range { offset, length } => (None, Some(offset), Some(length)),
    };
    to_json(
        &OciPlanJson {
            kind: "plan".to_string(),
            status: plan.status,
            headers,
            body,
            bucket: bucket.map(str::to_string),
            key,
            range_start,
            range_length,
            head,
            message: None,
        },
        "error",
    )
}

fn internal_error(message: String) -> String {
    let body = format!(
        "{{\"errors\":[{{\"code\":\"UNKNOWN\",\"message\":{}}}]}}",
        json_string(&message)
    );
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("content-length".to_string(), body.len().to_string());
    headers.insert("cache-control".to_string(), "no-store".to_string());
    to_json(
        &OciPlanJson {
            kind: "plan".to_string(),
            status: 500,
            headers,
            body: Some(body),
            bucket: None,
            key: None,
            range_start: None,
            range_length: None,
            head: false,
            message: None,
        },
        "error",
    )
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn oci_ping_json() -> String {
    plan_to_json(plan_ping(), None, None, false)
}

pub fn oci_manifest_json(
    repository: &str,
    repository_text: &str,
    reference: &str,
    head: bool,
) -> String {
    let repository = match RepositoryName::parse(repository) {
        Ok(repository) => repository,
        Err(error) => return plan_to_json(plan_error(&error), None, None, head),
    };
    if repository_text.is_empty() {
        return plan_to_json(
            plan_error(&stuff_oci::Error::name_unknown(repository.as_str())),
            None,
            None,
            head,
        );
    }
    let view = match parse_repository_json(&repository, repository_text) {
        Ok(view) => view,
        Err(error) => return internal_error(error.to_string()),
    };
    let reference = match Reference::parse(reference) {
        Ok(reference) => reference,
        Err(error) => return plan_to_json(plan_error(&error), None, None, head),
    };
    let descriptor = match &reference {
        Reference::Tag(tag) => view.tags.get(tag).cloned(),
        Reference::Digest(digest) => view.manifests.get(digest).cloned(),
    };
    let Some(descriptor) = descriptor else {
        return plan_to_json(
            plan_error(&stuff_oci::Error::manifest_unknown(reference.as_str())),
            None,
            None,
            head,
        );
    };
    let method = if head { Method::Head } else { Method::Get };
    let plan = plan_manifest(
        method,
        &reference,
        &descriptor.media_type,
        &descriptor.digest,
        descriptor.size,
    );
    let key = format!("oci/manifests/sha256/{}", descriptor.digest.hex());
    plan_to_json(plan, Some("OCI"), Some(key), head)
}

pub fn oci_tags_json(
    repository: &str,
    repository_text: &str,
    n: Option<usize>,
    last: Option<&str>,
) -> String {
    let repository = match RepositoryName::parse(repository) {
        Ok(repository) => repository,
        Err(error) => return plan_to_json(plan_error(&error), None, None, false),
    };
    if repository_text.is_empty() {
        return plan_to_json(
            plan_error(&stuff_oci::Error::name_unknown(repository.as_str())),
            None,
            None,
            false,
        );
    }
    let view = match parse_repository_json(&repository, repository_text) {
        Ok(view) => view,
        Err(error) => return internal_error(error.to_string()),
    };
    let tags: Vec<stuff_oci::Tag> = view.tags.keys().cloned().collect();
    let pagination = Pagination { n, last: last.map(str::to_string) };
    plan_to_json(plan_tags(Method::Get, &repository, &tags, &pagination), None, None, false)
}

pub fn oci_referrers_json(
    repository: &str,
    subject: &str,
    referrers_text: &str,
    artifact_type: Option<&str>,
    n: Option<usize>,
    last: Option<&str>,
) -> String {
    let repository = match RepositoryName::parse(repository) {
        Ok(repository) => repository,
        Err(error) => return plan_to_json(plan_error(&error), None, None, false),
    };
    let subject = match Digest::parse(subject) {
        Ok(subject) => subject,
        Err(error) => return plan_to_json(plan_error(&error), None, None, false),
    };
    let descriptors = if referrers_text.is_empty() {
        Vec::new()
    } else {
        match parse_referrers(referrers_text) {
            Ok(descriptors) => descriptors,
            Err(error) => return internal_error(error.to_string()),
        }
    };
    let descriptors = filter_referrers(&descriptors, artifact_type);
    let pagination = Pagination { n, last: last.map(str::to_string) };
    plan_to_json(
        plan_referrers(
            Method::Get,
            &repository,
            &subject,
            &descriptors,
            artifact_type,
            &pagination,
        ),
        None,
        None,
        false,
    )
}

#[derive(Debug, Clone, Default, deser::Deserialize)]
pub struct OciHeadersJson {
    #[deser(default)]
    pub range: Option<String>,
}

pub fn oci_blob_json(method: &str, digest: &str, size: u64, headers_json: &str) -> String {
    let digest = match Digest::parse(digest) {
        Ok(digest) => digest,
        Err(error) => return plan_to_json(plan_error(&error), None, None, false),
    };
    let headers: OciHeadersJson = match deser_json::from_str(headers_json) {
        Ok(headers) => headers,
        Err(error) => return internal_error(format!("headers: {error}")),
    };
    let method = Method::parse(method);
    let head = method == Method::Head;
    let plan = plan_blob(method, &digest, size, &RequestHeaders { range: headers.range });
    let key = format!("sha256/{}", digest.hex());
    plan_to_json(plan, Some("OCI_BLOBS"), Some(key), head)
}

pub fn oci_method_not_allowed_json() -> String {
    plan_to_json(plan_method_not_allowed(), None, None, false)
}

pub fn oci_auth_challenge_json() -> String {
    plan_to_json(stuff_oci::plan_auth_required(), None, None, false)
}

pub fn oci_manifest_put_json(repository: &str, digest: &str) -> String {
    match (RepositoryName::parse(repository), Digest::parse(digest)) {
        (Ok(repository), Ok(digest)) => {
            let location = format!("/v2/{}/manifests/{}", repository.as_str(), digest.as_str());
            plan_to_json(stuff_oci::plan_manifest_put(&location, &digest, None), None, None, false)
        }
        (Err(error), _) | (_, Err(error)) => plan_to_json(plan_error(&error), None, None, false),
    }
}

pub fn oci_error_json(code: &str, detail: &str) -> String {
    let error = match code {
        "BLOB_UNKNOWN" => stuff_oci::Error::blob_unknown(detail),
        "MANIFEST_UNKNOWN" => stuff_oci::Error::manifest_unknown(detail),
        "MANIFEST_BLOB_UNKNOWN" => stuff_oci::Error::new(
            stuff_oci::ErrorCode::ManifestBlobUnknown,
            "manifest references a missing blob",
        )
        .with_detail(detail),
        "NAME_UNKNOWN" => stuff_oci::Error::name_unknown(detail),
        "UNSUPPORTED" => stuff_oci::Error::unsupported(detail),
        "CONFLICT" => stuff_oci::Error::new(stuff_oci::ErrorCode::Conflict, detail),
        other => stuff_oci::Error::new(stuff_oci::ErrorCode::NameUnknown, other).with_detail(detail),
    };
    plan_to_json(plan_error(&error), None, None, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_request(s: &str) -> OciRequestJson {
        deser_json::from_str(s).expect("request json")
    }

    fn parse_plan(s: &str) -> OciPlanJson {
        deser_json::from_str(s).expect("plan json")
    }

    const REPOSITORY: &str = r#"{"schemaVersion":1,"repository":"example/app","tags":{"stable":{"mediaType":"application/vnd.oci.image.index.v1+json","digest":"sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388","size":491}},"manifests":[{"mediaType":"application/vnd.oci.image.index.v1+json","digest":"sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388","size":491}]}"#;

    #[test]
    fn parses_each_read_route() {
        assert_eq!(parse_request(&oci_request_json("GET", "/v2/")).kind, "ping");
        let manifest = parse_request(&oci_request_json("HEAD", "/v2/example/app/manifests/stable"));
        assert_eq!(manifest.kind, "manifest");
        assert_eq!(manifest.repository.as_deref(), Some("example/app"));
        assert_eq!(manifest.reference.as_deref(), Some("stable"));
        assert!(manifest.head);
        let blob = parse_request(&oci_request_json(
            "GET",
            "/v2/example/app/blobs/sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f",
        ));
        assert_eq!(blob.kind, "blob");
        let tags = parse_request(&oci_request_json("GET", "/v2/example/app/tags/list?n=2&last=beta"));
        assert_eq!(tags.kind, "tags");
        assert_eq!(tags.n, Some(2));
        assert_eq!(tags.last.as_deref(), Some("beta"));
        let referrers = parse_request(&oci_request_json(
            "GET",
            "/v2/example/app/referrers/sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4?artifactType=application%2Fjson",
        ));
        assert_eq!(referrers.kind, "referrers");
        assert_eq!(referrers.artifact_type.as_deref(), Some("application/json"));
        assert_eq!(parse_request(&oci_request_json("POST", "/v2/example/app/blobs/uploads/")).kind, "method_not_allowed");
        assert_eq!(parse_request(&oci_request_json("GET", "/v2/_catalog")).kind, "not_found");
        assert_eq!(parse_request(&oci_request_json("GET", "/v2/example/app/manifests/sha256:00")).kind, "error");
    }

    #[test]
    fn plans_manifests_and_blobs_with_keys() {
        let plan = parse_plan(&oci_manifest_json("example/app", REPOSITORY, "stable", false));
        assert_eq!(plan.kind, "plan");
        assert_eq!(plan.status, 200);
        assert_eq!(plan.bucket.as_deref(), Some("OCI"));
        assert_eq!(
            plan.key.as_deref(),
            Some("oci/manifests/sha256/04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388")
        );
        assert_eq!(
            plan.headers.get("docker-content-digest").map(String::as_str),
            Some("sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388")
        );
        let head = parse_plan(&oci_manifest_json("example/app", REPOSITORY, "stable", true));
        assert!(head.head);
        assert_eq!(head.body, None);
        let missing = parse_plan(&oci_manifest_json("example/app", REPOSITORY, "nope", false));
        assert_eq!(missing.status, 404);
        assert!(missing.body.unwrap().contains("MANIFEST_UNKNOWN"));
        let blob = parse_plan(&oci_blob_json(
            "GET",
            "sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f",
            117,
            r#"{"range":"bytes=0-9"}"#,
        ));
        assert_eq!(blob.status, 206);
        assert_eq!((blob.range_start, blob.range_length), (Some(0), Some(10)));
        assert_eq!(blob.bucket.as_deref(), Some("OCI_BLOBS"));
    }

    #[test]
    fn plans_tags_referrers_and_errors() {
        let tags = parse_plan(&oci_tags_json("example/app", REPOSITORY, Some(1), None));
        assert_eq!(tags.status, 200);
        assert_eq!(tags.body.as_deref(), Some(r#"{"name":"example/app","tags":["stable"]}"#));
        let referrers = parse_plan(&oci_referrers_json(
            "example/app",
            "sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4",
            "",
            Some("application/json"),
            None,
            None,
        ));
        assert_eq!(referrers.status, 200);
        assert_eq!(
            referrers.headers.get("oci-filters-applied").map(String::as_str),
            Some("artifactType")
        );
        assert!(referrers.body.unwrap().contains("\"manifests\":[]"));
        let error = parse_plan(&oci_error_json("BLOB_UNKNOWN", "sha256:00"));
        assert_eq!(error.status, 404);
        assert!(error.body.unwrap().contains("BLOB_UNKNOWN"));
        let method = parse_plan(&oci_method_not_allowed_json());
        assert_eq!(method.status, 405);
        assert_eq!(method.headers.get("allow").map(String::as_str), Some("GET, HEAD"));
    }
}
