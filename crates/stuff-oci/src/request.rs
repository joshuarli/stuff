#![forbid(unsafe_code)]

use crate::digest::Digest;
use crate::error::{Error, ErrorCode};
use crate::reference::{Reference, RepositoryName};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Other,
}

impl Method {
    pub fn parse(text: &str) -> Self {
        match text {
            "GET" => Self::Get,
            "HEAD" => Self::Head,
            "POST" => Self::Post,
            "PUT" => Self::Put,
            "PATCH" => Self::Patch,
            "DELETE" => Self::Delete,
            _ => Self::Other,
        }
    }

    pub fn is_read(self) -> bool {
        matches!(self, Self::Get | Self::Head)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pagination {
    pub n: Option<usize>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Ping,
    Manifest { repository: RepositoryName, reference: Reference, head: bool },
    Blob { repository: RepositoryName, digest: Digest, head: bool },
    Tags { repository: RepositoryName, pagination: Pagination },
    Referrers {
        repository: RepositoryName,
        subject: Digest,
        artifact_type: Option<String>,
        pagination: Pagination,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Request(Request),
    MethodNotAllowed,
    NotFound,
    Invalid(Error),
}

pub fn parse_request(method: Method, raw_target: &str) -> Parsed {
    let (path, query) = match raw_target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (raw_target, ""),
    };
    if !(path == "/v2" || path == "/v2/" || path.starts_with("/v2/")) {
        return Parsed::NotFound;
    }
    if !method.is_read() {
        return Parsed::MethodNotAllowed;
    }
    if path == "/v2" || path == "/v2/" {
        return Parsed::Request(Request::Ping);
    }
    let rest = &path["/v2/".len()..];
    if rest.is_empty() {
        return Parsed::Request(Request::Ping);
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() < 2 {
        return Parsed::NotFound;
    }
    let tail = segments[segments.len() - 1];
    let kind = segments[segments.len() - 2];
    let repository_text = segments[..segments.len() - 2].join("/");
    let repository = || {
        if repository_text.is_empty() {
            Err(Error::name_invalid("empty repository name"))
        } else {
            RepositoryName::parse(&repository_text)
        }
    };
    match (kind, tail) {
        ("manifests", reference) => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(error) => return Parsed::Invalid(error),
            };
            match Reference::parse(reference) {
                Ok(reference) => Parsed::Request(Request::Manifest {
                    repository,
                    reference,
                    head: method == Method::Head,
                }),
                Err(error) => Parsed::Invalid(error),
            }
        }
        ("blobs", digest) => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(error) => return Parsed::Invalid(error),
            };
            match Digest::parse(digest) {
                Ok(digest) => {
                    Parsed::Request(Request::Blob { repository, digest, head: method == Method::Head })
                }
                Err(error) => Parsed::Invalid(error),
            }
        }
        ("tags", "list") => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(error) => return Parsed::Invalid(error),
            };
            match parse_query(query) {
                Ok(params) => match pagination(&params) {
                    Ok(pagination) => Parsed::Request(Request::Tags { repository, pagination }),
                    Err(error) => Parsed::Invalid(error),
                },
                Err(error) => Parsed::Invalid(error),
            }
        }
        ("referrers", digest) => {
            let repository = match repository() {
                Ok(repository) => repository,
                Err(error) => return Parsed::Invalid(error),
            };
            match Digest::parse(digest) {
                Ok(subject) => match parse_query(query) {
                    Ok(params) => match pagination(&params) {
                        Ok(pagination) => Parsed::Request(Request::Referrers {
                            repository,
                            subject,
                            artifact_type: params.get("artifactType").cloned(),
                            pagination,
                        }),
                        Err(error) => Parsed::Invalid(error),
                    },
                    Err(error) => Parsed::Invalid(error),
                },
                Err(error) => Parsed::Invalid(error),
            }
        }
        _ => Parsed::NotFound,
    }
}

pub(crate) fn parse_query(query: &str) -> Result<BTreeMap<String, String>, Error> {
    let mut params = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key)
            .ok_or_else(|| Error::new(ErrorCode::PaginationNumberInvalid, "invalid query encoding"))?;
        let value = percent_decode(value)
            .ok_or_else(|| Error::new(ErrorCode::PaginationNumberInvalid, "invalid query encoding"))?;
        params.entry(key).or_insert(value);
    }
    Ok(params)
}

fn pagination(params: &BTreeMap<String, String>) -> Result<Pagination, Error> {
    let n = match params.get("n") {
        Some(text) => Some(
            text.parse::<usize>()
                .map_err(|_| Error::pagination_number_invalid(text.clone()))?,
        ),
        None => None,
    };
    Ok(Pagination { n, last: params.get("last").cloned() })
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let high = hex(bytes[i + 1])?;
            let low = hex(bytes[i + 2])?;
            out.push(high * 16 + low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteMethod {
    UploadStart,
    UploadMount { mount: Digest, from: Option<RepositoryName> },
    UploadStatus { upload_id: String },
    UploadPatch { upload_id: String },
    UploadPut { upload_id: String, digest: Digest },
    UploadCancel { upload_id: String },
    ManifestPut { reference: Reference },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    pub method: WriteMethod,
    pub repository: RepositoryName,
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedWrite {
    Request(WriteRequest),
    NotFound,
    Unsupported,
    Invalid(Error),
}

pub fn parse_write_request(method: Method, raw_target: &str) -> ParsedWrite {
    let (path, query) = match raw_target.split_once('?') {
        Some((path, query)) => (path, query),
        None => (raw_target, ""),
    };
    if !(path == "/v2" || path == "/v2/" || path.starts_with("/v2/")) {
        return ParsedWrite::NotFound;
    }
    let rest = path.strip_prefix("/v2/").unwrap_or("");
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return ParsedWrite::NotFound;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    let length = segments.len();
    let repository = |segments: &[&str]| -> Result<RepositoryName, Error> {
        let text = segments.join("/");
        if text.is_empty() {
            return Err(Error::name_invalid("empty repository name"));
        }
        RepositoryName::parse(&text)
    };
    if length >= 2 && segments[length - 2] == "manifests" {
        let repository = match repository(&segments[..length - 2]) {
            Ok(repository) => repository,
            Err(error) => return ParsedWrite::Invalid(error),
        };
        return match method {
            Method::Put => match Reference::parse(segments[length - 1]) {
                Ok(reference) => ParsedWrite::Request(WriteRequest {
                    method: WriteMethod::ManifestPut { reference },
                    repository,
                    session: None,
                }),
                Err(error) => ParsedWrite::Invalid(error),
            },
            Method::Delete => ParsedWrite::Unsupported,
            _ => ParsedWrite::NotFound,
        };
    }
    if length >= 2 && segments[length - 2] == "blobs" && segments[length - 1] == "uploads" {
        let repository = match repository(&segments[..length - 2]) {
            Ok(repository) => repository,
            Err(error) => return ParsedWrite::Invalid(error),
        };
        if method != Method::Post {
            return ParsedWrite::Unsupported;
        }
        let params = match parse_query(query) {
            Ok(params) => params,
            Err(error) => return ParsedWrite::Invalid(error),
        };
        let method = match params.get("mount") {
            Some(mount) => {
                let mount = match Digest::parse(mount) {
                    Ok(mount) => mount,
                    Err(error) => return ParsedWrite::Invalid(error),
                };
                let from = match params.get("from") {
                    Some(from) => match RepositoryName::parse(from) {
                        Ok(from) => Some(from),
                        Err(error) => return ParsedWrite::Invalid(error),
                    },
                    None => None,
                };
                WriteMethod::UploadMount { mount, from }
            }
            None => WriteMethod::UploadStart,
        };
        return ParsedWrite::Request(WriteRequest { method, repository, session: None });
    }
    if length >= 3 && segments[length - 3] == "blobs" && segments[length - 2] == "uploads" {
        let repository = match repository(&segments[..length - 3]) {
            Ok(repository) => repository,
            Err(error) => return ParsedWrite::Invalid(error),
        };
        if length != 5 {
            return ParsedWrite::NotFound;
        }
        let upload_id = segments[4];
        if !valid_upload_id(upload_id) {
            return ParsedWrite::Invalid(Error::new(
                crate::error::ErrorCode::BlobUploadInvalid,
                "invalid upload id",
            ));
        }
        let params = match parse_query(query) {
            Ok(params) => params,
            Err(error) => return ParsedWrite::Invalid(error),
        };
        let method = match method {
            Method::Get => WriteMethod::UploadStatus { upload_id: upload_id.to_string() },
            Method::Patch => WriteMethod::UploadPatch { upload_id: upload_id.to_string() },
            Method::Delete => WriteMethod::UploadCancel { upload_id: upload_id.to_string() },
            Method::Put => {
                let digest = match params.get("digest") {
                    Some(digest) => match Digest::parse(digest) {
                        Ok(digest) => digest,
                        Err(error) => return ParsedWrite::Invalid(error),
                    },
                    None => return ParsedWrite::Invalid(Error::digest_invalid("missing digest query")),
                };
                WriteMethod::UploadPut { upload_id: upload_id.to_string(), digest }
            }
            _ => return ParsedWrite::Unsupported,
        };
        return ParsedWrite::Request(WriteRequest {
            method,
            repository,
            session: params.get("session").cloned(),
        });
    }
    ParsedWrite::NotFound
}

fn valid_upload_id(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'='))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(target: &str) -> Parsed {
        parse_request(Method::Get, target)
    }

    #[test]
    fn parses_ping_and_read_routes() {
        assert_eq!(get("/v2/"), Parsed::Request(Request::Ping));
        assert_eq!(get("/v2"), Parsed::Request(Request::Ping));
        let Parsed::Request(Request::Manifest { repository, reference, head }) =
            get("/v2/example/app/manifests/stable")
        else {
            panic!("manifest route")
        };
        assert_eq!(repository.as_str(), "example/app");
        assert_eq!(reference.as_str(), "stable");
        assert!(!head);
        let Parsed::Request(Request::Blob { digest, head, .. }) =
            get("/v2/example/app/blobs/sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f")
        else {
            panic!("blob route")
        };
        assert!(!head);
        assert!(digest.as_str().starts_with("sha256:882765"));
    }

    #[test]
    fn parses_write_routes() {
        let ParsedWrite::Request(WriteRequest { method, repository, .. }) =
            parse_write_request(Method::Post, "/v2/example/app/blobs/uploads/")
        else {
            panic!("upload start")
        };
        assert_eq!(method, WriteMethod::UploadStart);
        assert_eq!(repository.as_str(), "example/app");

        let digest = "sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f";
        let ParsedWrite::Request(WriteRequest { method, .. }) = parse_write_request(
            Method::Post,
            &format!("/v2/example/app/blobs/uploads/?mount={digest}&from=example/base"),
        ) else {
            panic!("mount")
        };
        assert_eq!(
            method,
            WriteMethod::UploadMount {
                mount: Digest::parse(digest).unwrap(),
                from: Some(RepositoryName::parse("example/base").unwrap()),
            }
        );

        let ParsedWrite::Request(WriteRequest { method, .. }) =
            parse_write_request(Method::Get, "/v2/example/app/blobs/uploads/abc-1?session=t")
        else {
            panic!("status")
        };
        assert_eq!(method, WriteMethod::UploadStatus { upload_id: "abc-1".to_string() });
        let ParsedWrite::Request(WriteRequest { method, .. }) =
            parse_write_request(Method::Patch, "/v2/example/app/blobs/uploads/abc-1")
        else {
            panic!("patch")
        };
        assert_eq!(method, WriteMethod::UploadPatch { upload_id: "abc-1".to_string() });
        let ParsedWrite::Request(WriteRequest { method, .. }) =
            parse_write_request(Method::Delete, "/v2/example/app/blobs/uploads/abc-1")
        else {
            panic!("cancel")
        };
        assert_eq!(method, WriteMethod::UploadCancel { upload_id: "abc-1".to_string() });
        let ParsedWrite::Request(WriteRequest { method, .. }) = parse_write_request(
            Method::Put,
            &format!("/v2/example/app/blobs/uploads/abc-1?digest={digest}"),
        ) else {
            panic!("put")
        };
        assert_eq!(
            method,
            WriteMethod::UploadPut { upload_id: "abc-1".to_string(), digest: Digest::parse(digest).unwrap() }
        );

        let ParsedWrite::Request(WriteRequest { method, .. }) =
            parse_write_request(Method::Put, "/v2/example/app/manifests/stable")
        else {
            panic!("manifest put")
        };
        assert_eq!(
            method,
            WriteMethod::ManifestPut { reference: Reference::parse("stable").unwrap() }
        );
        assert_eq!(
            parse_write_request(Method::Delete, "/v2/example/app/manifests/stable"),
            ParsedWrite::Unsupported
        );
        let ParsedWrite::Invalid(error) =
            parse_write_request(Method::Put, "/v2/example/app/blobs/uploads/abc-1")
        else {
            panic!("missing digest")
        };
        assert_eq!(error.code, crate::error::ErrorCode::DigestInvalid);
        assert_eq!(parse_write_request(Method::Post, "/v2/_catalog"), ParsedWrite::NotFound);
    }

    #[test]
    fn parses_tags_and_referrers_with_query() {
        let Parsed::Request(Request::Tags { pagination, .. }) = get("/v2/example/app/tags/list?n=2&last=beta")
        else {
            panic!("tags")
        };
        assert_eq!(pagination.n, Some(2));
        assert_eq!(pagination.last.as_deref(), Some("beta"));
        let Parsed::Request(Request::Referrers { artifact_type, .. }) =
            get("/v2/example/app/referrers/sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4?artifactType=application%2Fvnd.stuff.sbom.v1%2Bjson")
        else {
            panic!("referrers")
        };
        assert_eq!(artifact_type.as_deref(), Some("application/vnd.stuff.sbom.v1+json"));
    }

    #[test]
    fn routes_beyond_the_supported_subset() {
        assert_eq!(parse_request(Method::Post, "/v2/example/app/blobs/uploads/"), Parsed::MethodNotAllowed);
        assert_eq!(parse_request(Method::Delete, "/v2/example/app/manifests/sha256:00"), Parsed::MethodNotAllowed);
        assert_eq!(get("/v2/_catalog"), Parsed::NotFound);
        assert_eq!(get("/v2/example/app"), Parsed::NotFound);
        assert_eq!(get("/"), Parsed::NotFound);
    }
}
