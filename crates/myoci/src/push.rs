#![forbid(unsafe_code)]

use crate::error::Error;
use crate::publish;
use crate::state::State;
use stuff_oci::{
    parse_basic_auth, parse_write_request, plan_auth_required, plan_error, plan_manifest_put,
    plan_method_not_allowed, plan_upload_mounted, plan_upload_start, validate_manifest_push,
    Digest, Error as OciError, ErrorCode, Image, Method, ParsedWrite, Reference, RepositoryName,
    SessionToken, WriteMethod, DEFAULT_SESSION_TTL_SECS,
};
use rustc_hash::FxHashSet;
use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
static UPLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct PushOptions {
    pub state: State,
    pub site_dir: PathBuf,
    pub public_url: String,
    pub token: String,
    pub secret: Vec<u8>,
    pub writer_url: String,
}

pub fn handle_front(
    stream: &mut TcpStream,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    pending: &[u8],
    options: &PushOptions,
) {
    let parsed = parse_write_request(Method::parse(method), target);
    let request = match parsed {
        ParsedWrite::Request(request) => request,
        ParsedWrite::NotFound => {
            crate::serve::send_plan(stream, &plan_error(&OciError::name_unknown("unknown path")), None);
            return;
        }
        ParsedWrite::Unsupported => {
            crate::serve::send_plan(stream, &plan_method_not_allowed(), None);
            return;
        }
        ParsedWrite::Invalid(error) => {
            crate::serve::send_plan(stream, &plan_error(&error), None);
            return;
        }
    };
    if !authorized(headers, &options.token) {
        crate::serve::send_plan(stream, &plan_auth_required(), None);
        return;
    }
    match request.method {
        WriteMethod::UploadStart => {
            let plan = start_upload(&request.repository, options);
            crate::serve::send_plan(stream, &plan, None);
        }
        WriteMethod::UploadMount { mount, from: _ } => {
            if options.state.blob_path(&mount).is_file() {
                let location = format!("/v2/{}/blobs/{}", request.repository.as_str(), mount.as_str());
                crate::serve::send_plan(stream, &plan_upload_mounted(&location, &mount), None);
            } else {
                let plan = start_upload(&request.repository, options);
                crate::serve::send_plan(stream, &plan, None);
            }
        }
        WriteMethod::UploadStatus { .. }
        | WriteMethod::UploadPatch { .. }
        | WriteMethod::UploadPut { .. }
        | WriteMethod::UploadCancel { .. } => {
            let plan = plan_error(&OciError::new(
                ErrorCode::BlobUploadUnknown,
                "upload sessions live on the writer",
            ));
            crate::serve::send_plan(stream, &plan, None);
        }
        WriteMethod::ManifestPut { reference } => {
            let body = match read_body(stream, headers, pending, MAX_MANIFEST_BYTES) {
                Ok(body) => body,
                Err(error) => {
                    crate::serve::send_plan(stream, &plan_error(&error), None);
                    return;
                }
            };
            match commit_manifest(&request.repository, &reference, &body, options) {
                Ok((digest, subject)) => {
                    let location = format!("/v2/{}/manifests/{}", request.repository.as_str(), digest.as_str());
                    crate::serve::send_plan(stream, &plan_manifest_put(&location, &digest, subject.as_ref()), None);
                }
                Err(error) => crate::serve::send_plan(stream, &plan_error(&error), None),
            }
        }
    }
}

fn start_upload(repository: &RepositoryName, options: &PushOptions) -> stuff_oci::ResponsePlan {
    let upload_id = upload_id();
    let expires = now() + DEFAULT_SESSION_TTL_SECS;
    let token = SessionToken::new(repository.as_str(), &upload_id, expires).encode(&options.secret);
    let location = format!(
        "{}/v2/{}/blobs/uploads/{}?session={}&offset=0",
        options.writer_url.trim_end_matches('/'),
        repository.as_str(),
        upload_id,
        token
    );
    plan_upload_start(&location, &upload_id)
}

fn commit_manifest(
    repository: &RepositoryName,
    reference: &Reference,
    body: &[u8],
    options: &PushOptions,
) -> std::result::Result<(Digest, Option<Digest>), OciError> {
    let _lock = options.state.lock().map_err(map_error)?;
    let _ = crate::mirror::transaction::recover_locked(&options.state).map_err(map_error)?;
    let mut view = options
        .state
        .read_repository(repository)
        .map_err(map_error)?
        .unwrap_or_else(|| stuff_oci::RepositoryView::new(repository.clone()));
    let blobs = options.state.blob_digests_on_disk().map_err(map_error)?;
    let children: FxHashSet<Digest> = view.manifests.keys().cloned().collect();
    let (digest, image) = validate_manifest_push(body, &blobs, &children)?;
    if let Reference::Digest(expected) = reference
        && expected != &digest
    {
        return Err(OciError::digest_invalid(format!(
            "manifest digest is {digest}, not {expected}"
        )));
    }
    options.state.store_manifest(&digest, body).map_err(map_error)?;
    let descriptor = image.descriptor(digest.clone(), body.len() as u64);
    view.insert_manifest(descriptor.clone());
    if let Reference::Tag(tag) = reference {
        view.tag(tag.clone(), descriptor);
    }
    let mut index = options.state.read_blobs().map_err(map_error)?;
    if let Image::Manifest(manifest) = &image {
        index.insert(manifest.config.digest.clone());
        for layer in &manifest.layers {
            if layer.urls.is_empty() {
                index.insert(layer.digest.clone());
            }
        }
    }
    options.state.write_blobs(&index).map_err(map_error)?;
    options.state.write_repository(&view).map_err(map_error)?;
    options.state.rebuild_referrers(&view).map_err(map_error)?;
    publish::publish_locked(&options.state, &options.site_dir, &options.public_url).map_err(map_error)?;
    let subject = image.subject().map(|subject| subject.digest.clone());
    Ok((digest, subject))
}

fn map_error(error: Error) -> OciError {
    match error {
        Error::Conflict(detail) => OciError::new(ErrorCode::Conflict, detail),
        Error::Integrity(detail) => OciError::manifest_invalid(detail),
        Error::Input(detail) => OciError::manifest_invalid(detail),
        Error::Limit(detail) => OciError::new(ErrorCode::SizeInvalid, detail),
        other => OciError::new(ErrorCode::ManifestInvalid, other.to_string()),
    }
}

fn read_body(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    pending: &[u8],
    limit: usize,
) -> std::result::Result<Vec<u8>, OciError> {
    if header(headers, "transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        return read_chunked_body(stream, pending, limit);
    }
    let length = header(headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| OciError::new(ErrorCode::SizeInvalid, "content-length is required"))?;
    if length > limit {
        return Err(OciError::new(
            ErrorCode::SizeInvalid,
            format!("request body exceeds {limit} bytes"),
        ));
    }
    let mut body = vec![0u8; length];
    let copied = pending.len().min(length);
    body[..copied].copy_from_slice(&pending[..copied]);
    if copied < length {
        stream
            .read_exact(&mut body[copied..])
            .map_err(|error| OciError::new(ErrorCode::SizeInvalid, format!("could not read body: {error}")))?;
    }
    Ok(body)
}

fn read_chunked_body(
    stream: &mut TcpStream,
    pending: &[u8],
    limit: usize,
) -> std::result::Result<Vec<u8>, OciError> {
    use std::io::{BufRead, BufReader, Read};

    let mut reader = BufReader::new(std::io::Cursor::new(pending.to_vec()).chain(stream));
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| OciError::new(ErrorCode::SizeInvalid, format!("could not read chunk size: {error}")))?;
        let size_text = line
            .trim_end_matches(['\r', '\n'])
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| OciError::new(ErrorCode::SizeInvalid, format!("invalid chunk size {size_text:?}")))?;
        if size == 0 {
            loop {
                let mut trailer = String::new();
                let read = reader
                    .read_line(&mut trailer)
                    .map_err(|error| OciError::new(ErrorCode::SizeInvalid, format!("could not read trailers: {error}")))?;
                if read == 0 || trailer == "\r\n" || trailer == "\n" {
                    break;
                }
            }
            break;
        }
        if body.len() + size > limit {
            return Err(OciError::new(
                ErrorCode::SizeInvalid,
                format!("request body exceeds {limit} bytes"),
            ));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(&mut body[start..])
            .map_err(|error| OciError::new(ErrorCode::SizeInvalid, format!("could not read chunk: {error}")))?;
        let mut crlf = [0u8; 2];
        reader
            .read_exact(&mut crlf)
            .map_err(|error| OciError::new(ErrorCode::SizeInvalid, format!("could not read chunk terminator: {error}")))?;
        if crlf != *b"\r\n" {
            return Err(OciError::new(ErrorCode::SizeInvalid, "chunk is not CRLF terminated"));
        }
    }
    Ok(body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn authorized(headers: &[(String, String)], token: &str) -> bool {
    let Some(value) = header(headers, "authorization") else {
        return false;
    };
    let Some((user, password)) = parse_basic_auth(value) else {
        return false;
    };
    user == "stuff" && constant_time_eq(password.as_bytes(), token.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

fn upload_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0);
    let counter = UPLOAD_COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{nanos:x}{counter:x}")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
