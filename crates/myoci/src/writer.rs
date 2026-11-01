#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::state::State;
use stuff_oci::{
    parse_write_request, plan_error, plan_upload_cancel, plan_upload_complete, plan_upload_patch,
    plan_upload_status, Digest, Error as OciError, ErrorCode, Method, ParsedWrite, SessionToken,
    WriteMethod,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_CONNECTIONS: usize = 16;

#[derive(Debug, Clone)]
pub enum WriterSink {
    Local,
    R2 { bucket: String, deploy_dir: std::path::PathBuf },
}

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub bind: String,
    pub state: State,
    pub secret: Vec<u8>,
    pub max_connections: usize,
    pub sink: WriterSink,
}

pub fn run(options: WriterOptions) -> Result<()> {
    options.state.init()?;
    std::fs::create_dir_all(options.state.uploads_dir())?;
    let listener = TcpListener::bind(&options.bind)
        .map_err(|error| Error::Operation(format!("could not bind {}: {error}", options.bind)))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| Error::Operation(format!("could not configure listener: {error}")))?;
    println!(
        "listening on {}",
        listener
            .local_addr()
            .map_err(|error| Error::Operation(format!("could not read listener address: {error}")))?
    );
    let _ = std::io::stdout().flush();
    let active = Arc::new(AtomicUsize::new(0));
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let current = active.fetch_add(1, Ordering::SeqCst);
                if current >= options.max_connections {
                    active.fetch_sub(1, Ordering::SeqCst);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let options = options.clone();
                let active_thread = active.clone();
                std::thread::spawn(move || {
                    handle_connection(stream, &options);
                    active_thread.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => {
                eprintln!("myoci writer: accept failed: {error}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_connection(mut stream: TcpStream, options: &WriterOptions) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
    let Some(head) = crate::serve::read_request(&mut stream) else {
        return;
    };
    let crate::serve::RequestHead { method, target, headers, body } = head;
    let parsed = parse_write_request(Method::parse(&method), &target);
    let request = match parsed {
        ParsedWrite::Request(request) => request,
        ParsedWrite::NotFound => {
            crate::serve::send_plan(&mut stream, &plan_error(&OciError::name_unknown("unknown path")), None);
            return;
        }
        ParsedWrite::Unsupported => {
            crate::serve::send_plan(&mut stream, &plan_error(&OciError::unsupported("unsupported")), None);
            return;
        }
        ParsedWrite::Invalid(error) => {
            crate::serve::send_plan(&mut stream, &plan_error(&error), None);
            return;
        }
    };
    let session = match request.session.as_deref() {
        Some(session) => session,
        None => {
            crate::serve::send_plan(
                &mut stream,
                &plan_error(&OciError::new(ErrorCode::Denied, "missing upload session")),
                None,
            );
            return;
        }
    };
    let token = match SessionToken::verify(session, &options.secret, now(), request.repository.as_str()) {
        Ok(token) => token,
        Err(error) => {
            crate::serve::send_plan(&mut stream, &plan_error(&error), None);
            return;
        }
    };
    let upload_id = match &request.method {
        WriteMethod::UploadStatus { upload_id }
        | WriteMethod::UploadPatch { upload_id }
        | WriteMethod::UploadPut { upload_id, .. }
        | WriteMethod::UploadCancel { upload_id } => upload_id.clone(),
        _ => {
            crate::serve::send_plan(
                &mut stream,
                &plan_error(&OciError::new(ErrorCode::BlobUploadUnknown, "not an upload")),
                None,
            );
            return;
        }
    };
    if token.upload_id != upload_id {
        crate::serve::send_plan(
            &mut stream,
            &plan_error(&OciError::new(ErrorCode::Denied, "session does not match the upload")),
            None,
        );
        return;
    }
    let temp = options.state.uploads_dir().join(&upload_id);
    match request.method {
        WriteMethod::UploadStatus { .. } => {
            let length = std::fs::metadata(&temp).map(|meta| meta.len()).unwrap_or(0);
            crate::serve::send_plan(&mut stream, &plan_upload_status(&upload_id, length), None);
        }
        WriteMethod::UploadPatch { .. } => match append_body(&mut stream, &headers, &body, &temp) {
            Ok(length) => {
                crate::serve::send_plan(&mut stream, &plan_upload_patch(&target, &upload_id, length), None);
            }
            Err(error) => {
                crate::serve::send_plan(&mut stream, &plan_error(&error), None);
            }
        },
        WriteMethod::UploadPut { digest, .. } => match append_body(&mut stream, &headers, &body, &temp) {
            Ok(_) => match verify_and_store(&options.state, &temp, &digest, &options.sink) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&temp);
                    let location = format!("/v2/{}/blobs/{}", request.repository.as_str(), digest.as_str());
                    crate::serve::send_plan(&mut stream, &plan_upload_complete(&location, &digest), None);
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temp);
                    crate::serve::send_plan(&mut stream, &plan_error(&error), None);
                }
            },
            Err(error) => {
                crate::serve::send_plan(&mut stream, &plan_error(&error), None);
            }
        },
        WriteMethod::UploadCancel { .. } => {
            let _ = std::fs::remove_file(&temp);
            crate::serve::send_plan(&mut stream, &plan_upload_cancel(), None);
        }
        _ => {}
    }
}

fn append_body(
    stream: &mut TcpStream,
    headers: &[(String, String)],
    pending: &[u8],
    temp: &Path,
) -> std::result::Result<u64, OciError> {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    if header("transfer-encoding").is_some() {
        return Err(OciError::new(
            ErrorCode::SizeInvalid,
            "chunked uploads are not supported",
        ));
    }
    let length: u64 = header("content-length")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| OciError::new(ErrorCode::SizeInvalid, "content-length is required"))?;
    let existing = std::fs::metadata(temp).map(|meta| meta.len()).unwrap_or(0);
    if existing.saturating_add(length) > MAX_UPLOAD_BYTES {
        return Err(OciError::new(
            ErrorCode::SizeInvalid,
            "upload exceeds the 20 GiB limit",
        ));
    }
    if let Some(parent) = temp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(temp)
        .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, format!("could not open upload: {error}")))?;
    let mut remaining = length;
    let copied = pending.len().min(length as usize);
    if copied > 0 {
        file.write_all(&pending[..copied])
            .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, format!("write upload: {error}")))?;
        remaining -= copied as u64;
    }
    let mut buffer = [0u8; 1 << 20];
    while remaining > 0 {
        let want = remaining.min(buffer.len() as u64) as usize;
        let read = stream
            .read(&mut buffer[..want])
            .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, format!("read upload: {error}")))?;
        if read == 0 {
            return Err(OciError::new(
                ErrorCode::BlobUploadInvalid,
                "upload body ended early",
            ));
        }
        file.write_all(&buffer[..read])
            .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, format!("write upload: {error}")))?;
        remaining -= read as u64;
    }
    file.sync_all()
        .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, format!("sync upload: {error}")))?;
    Ok(existing + length)
}

fn verify_and_store(
    state: &State,
    temp: &Path,
    digest: &Digest,
    sink: &WriterSink,
) -> std::result::Result<(), OciError> {
    let hash = stuff_store::stream_file_hash(temp, stuff_store::HashAlg::Sha256)
        .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, error.to_string()))?;
    if hash != digest.hex() {
        return Err(OciError::digest_invalid(format!(
            "uploaded blob hashes to sha256:{hash}"
        )));
    }
    match sink {
        WriterSink::Local => state
            .store_blob(temp, digest)
            .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, error.to_string())),
        WriterSink::R2 { bucket, deploy_dir } => upload_r2(state, temp, digest, bucket, deploy_dir),
    }
}

fn upload_r2(
    state: &State,
    temp: &Path,
    digest: &Digest,
    bucket: &str,
    deploy_dir: &Path,
) -> std::result::Result<(), OciError> {
    let size = std::fs::metadata(temp)
        .map_err(|error| OciError::new(ErrorCode::BlobUploadInvalid, error.to_string()))?
        .len();
    let key = format!("sha256/{}", digest.hex());
    let single_max = std::env::var("R2_SINGLE_PUT_MAX")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300 * 1024 * 1024);
    let fail = |detail: String| OciError::new(ErrorCode::BlobUploadInvalid, detail);
    if size > single_max {
        let status = std::process::Command::new("python3")
            .arg(deploy_dir.join("r2-multipart.py"))
            .arg(temp)
            .arg(bucket)
            .arg(&key)
            .arg("application/octet-stream")
            .status()
            .map_err(|error| fail(format!("could not run r2-multipart.py: {error}")))?;
        if !status.success() {
            return Err(fail("R2 multipart upload failed".to_string()));
        }
        return Ok(());
    }
    let stage = state.uploads_dir().join(format!(".r2-{}", digest.hex()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("sha256"))
        .map_err(|error| fail(format!("could not create R2 staging: {error}")))?;
    let staged = stage.join(&key);
    std::fs::copy(temp, &staged).map_err(|error| fail(format!("could not stage blob: {error}")))?;
    let keys = stage.join("keys");
    std::fs::write(&keys, format!("{key}\n")).map_err(|error| fail(format!("could not write keys: {error}")))?;
    let status = std::process::Command::new("python3")
        .arg(deploy_dir.join("r2-put.py"))
        .arg(&stage)
        .arg(bucket)
        .arg(&keys)
        .status()
        .map_err(|error| fail(format!("could not run r2-put.py: {error}")))?;
    let _ = std::fs::remove_dir_all(&stage);
    if !status.success() {
        return Err(fail("R2 blob upload failed".to_string()));
    }
    Ok(())
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
