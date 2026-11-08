use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use super::model::PublishRequest;
use super::{Rejection, RejectionKind, input, limit};

pub const MAX_METADATA_BYTES: u64 = 10 * 1024 * 1024;
pub const FRAMING_SLACK: u64 = 10 * 1024 * 1024;

const READ_CHUNK: usize = 65536;

static UPLOAD_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq)]
pub struct StagedUpload {
    pub body: PublishRequest,
    pub path: PathBuf,
    pub crate_len: u64,
    pub crate_sha256: String,
}

impl StagedUpload {
    pub fn discard(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn decode_publish_payload<R: Read>(
    reader: &mut R,
    content_length: u64,
    max_compressed: u64,
    staging_dir: &Path,
) -> Result<StagedUpload, Rejection> {
    if content_length < 8 {
        return Err(input("upload payload too short"));
    }
    if content_length > max_compressed.saturating_add(FRAMING_SLACK) {
        return Err(limit("upload payload exceeds size limit"));
    }
    fs::create_dir_all(staging_dir).map_err(|error| {
        operation(format!(
            "could not create {}: {error}",
            staging_dir.display()
        ))
    })?;

    let json_len = u64::from(u32::from_le_bytes(read_exact(reader, "json length")?));
    if json_len == 0 {
        return Err(input("empty json metadata"));
    }
    if json_len > MAX_METADATA_BYTES || json_len > content_length - 8 {
        return Err(limit("json metadata length invalid or too large"));
    }
    let metadata = read_bytes(reader, json_len, "json body")?;
    let text = String::from_utf8(metadata)
        .map_err(|_| input("invalid publish json: metadata is not UTF-8"))?;
    let body = PublishRequest::from_json(&text).map_err(|rejection| {
        input(format!(
            "invalid publish json: {}",
            message_of(&rejection)
        ))
    })?;

    let crate_len = u64::from(u32::from_le_bytes(read_exact(reader, "crate length")?));
    if crate_len == 0 {
        return Err(input("empty crate archive"));
    }
    if crate_len > max_compressed {
        return Err(limit("crate archive exceeds max compressed size"));
    }
    if 4 + json_len + 4 + crate_len != content_length {
        return Err(input("crate length does not match content length framing"));
    }

    let spill = staging_dir.join(format!(
        ".upload.{}.{}.{}",
        std::process::id(),
        unique_nanos(),
        UPLOAD_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let mut staged = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&spill)
        .map_err(|error| operation(format!("failed to stage upload: {error}")))?;
    let mut digest = Sha256::new();
    let mut remaining = crate_len;
    let mut buffer = [0u8; READ_CHUNK];
    let outcome = (|| -> Result<(), Rejection> {
        while remaining > 0 {
            let wanted = remaining.min(READ_CHUNK as u64) as usize;
            let read = reader
                .read(&mut buffer[..wanted])
                .map_err(|error| operation(format!("failed to read crate bytes: {error}")))?;
            if read == 0 {
                return Err(input("failed to read crate bytes: connection closed early"));
            }
            digest.update(&buffer[..read]);
            staged
                .write_all(&buffer[..read])
                .map_err(|error| operation(format!("failed to stage upload: {error}")))?;
            remaining -= read as u64;
        }
        staged
            .flush()
            .and_then(|()| staged.sync_all())
            .map_err(|error| operation(format!("failed to stage upload: {error}")))
    })();
    drop(staged);
    if let Err(rejection) = outcome {
        let _ = fs::remove_file(&spill);
        return Err(rejection);
    }
    Ok(StagedUpload {
        body,
        path: spill,
        crate_len,
        crate_sha256: crate::publish::lower_hex(digest.finalize()),
    })
}

fn read_exact<const N: usize>(reader: &mut impl Read, what: &str) -> Result<[u8; N], Rejection> {
    let mut buffer = [0u8; N];
    let mut filled = 0;
    while filled < N {
        let read = reader
            .read(&mut buffer[filled..])
            .map_err(|error| operation(format!("failed to read {what}: {error}")))?;
        if read == 0 {
            return Err(input(format!(
                "failed to read {what}: connection closed early"
            )));
        }
        filled += read;
    }
    Ok(buffer)
}

fn read_bytes(reader: &mut impl Read, count: u64, what: &str) -> Result<Vec<u8>, Rejection> {
    let mut data = Vec::with_capacity(count as usize);
    let mut chunk = [0u8; READ_CHUNK];
    let mut remaining = count;
    while remaining > 0 {
        let wanted = remaining.min(READ_CHUNK as u64) as usize;
        let read = reader
            .read(&mut chunk[..wanted])
            .map_err(|error| operation(format!("failed to read {what}: {error}")))?;
        if read == 0 {
            return Err(input(format!(
                "failed to read {what}: connection closed early"
            )));
        }
        data.extend_from_slice(&chunk[..read]);
        remaining -= read as u64;
    }
    Ok(data)
}

fn message_of(rejection: &Rejection) -> String {
    let prefix = format!("{}: ", rejection.kind.prefix());
    rejection
        .detail
        .strip_prefix(&prefix)
        .unwrap_or(&rejection.detail)
        .to_string()
}

fn unique_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn operation(message: impl Into<String>) -> Rejection {
    Rejection::new(RejectionKind::Operation, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::cargo::RejectionKind;

    fn metadata() -> String {
        r#"{"name":"fixture","vers":"1.0.0","deps":[],"features":{},"links":null,"rust_version":null,"checksum":"","yanked":false}"#.to_string()
    }

    fn payload(json: &str, crate_bytes: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(json.len() as u32).to_le_bytes());
        body.extend_from_slice(json.as_bytes());
        body.extend_from_slice(&(crate_bytes.len() as u32).to_le_bytes());
        body.extend_from_slice(crate_bytes);
        body
    }

    #[test]
    fn a_well_formed_upload_is_framed_and_hashed() {
        let dir = tempfile::tempdir().unwrap();
        let archive = b"crate bytes";
        let body = payload(&metadata(), archive);
        let length = body.len() as u64;
        let mut reader = std::io::Cursor::new(body);
        let staged = decode_publish_payload(&mut reader, length, 1024, dir.path()).unwrap();
        assert_eq!(staged.body.name, "fixture");
        assert_eq!(staged.body.vers, "1.0.0");
        assert_eq!(staged.crate_len, archive.len() as u64);
        assert_eq!(
            staged.crate_sha256,
            crate::publish::lower_hex(Sha256::digest(archive))
        );
        assert_eq!(fs::read(&staged.path).unwrap(), archive);
        staged.discard();
        assert!(!staged.path.exists());
    }

    #[test]
    fn framing_errors_match_the_oracle_messages() {
        let dir = tempfile::tempdir().unwrap();
        let cases: Vec<(Vec<u8>, u64, &str, RejectionKind)> = vec![
            (vec![0u8; 7], 7, "input: upload payload too short", RejectionKind::Input),
            (
                vec![0u8; 16],
                FRAMING_SLACK + 4096,
                "limit: upload payload exceeds size limit",
                RejectionKind::Limit,
            ),
            (
                {
                    let mut body = 0u32.to_le_bytes().to_vec();
                    body.extend_from_slice(&[0u8; 8]);
                    body
                },
                12,
                "input: empty json metadata",
                RejectionKind::Input,
            ),
            (
                {
                    let mut body = (MAX_METADATA_BYTES as u32 + 1).to_le_bytes().to_vec();
                    body.extend_from_slice(&[0u8; 8]);
                    body
                },
                16,
                "limit: json metadata length invalid or too large",
                RejectionKind::Limit,
            ),
            (
                {
                    let mut body = (4u32).to_le_bytes().to_vec();
                    body.extend_from_slice(b"{}\x00\x00");
                    body
                },
                12,
                "input: invalid publish json: ",
                RejectionKind::Input,
            ),
            (
                {
                    let json = metadata();
                    let mut body = (json.len() as u32).to_le_bytes().to_vec();
                    body.extend_from_slice(json.as_bytes());
                    body.extend_from_slice(&0u32.to_le_bytes());
                    body
                },
                4 + metadata().len() as u64 + 4,
                "input: empty crate archive",
                RejectionKind::Input,
            ),
            (
                {
                    let json = metadata();
                    let mut body = (json.len() as u32).to_le_bytes().to_vec();
                    body.extend_from_slice(json.as_bytes());
                    body.extend_from_slice(&8u32.to_le_bytes());
                    body.extend_from_slice(b"12345678");
                    body
                },
                4 + metadata().len() as u64 + 4 + 8 + 1,
                "input: crate length does not match content length framing",
                RejectionKind::Input,
            ),
        ];
        for (body, length, expected, kind) in cases {
            let mut reader = std::io::Cursor::new(body);
            let error = decode_publish_payload(&mut reader, length, 1024, dir.path())
                .expect_err(expected);
            assert_eq!(error.kind, kind, "{expected}");
            if expected.ends_with(": ") {
                assert!(error.detail.starts_with(expected), "{}", error.detail);
            } else {
                assert_eq!(error.detail, expected);
            }
        }
    }

    #[test]
    fn a_short_crate_stream_is_an_input_error() {
        let dir = tempfile::tempdir().unwrap();
        let json = metadata();
        let mut body = (json.len() as u32).to_le_bytes().to_vec();
        body.extend_from_slice(json.as_bytes());
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(b"short");
        let length = 4 + json.len() as u64 + 4 + 16;
        let mut reader = std::io::Cursor::new(body);
        let error =
            decode_publish_payload(&mut reader, length, 1024, dir.path()).expect_err("short");
        assert_eq!(error.kind, RejectionKind::Input);
        assert_eq!(
            error.detail,
            "input: failed to read crate bytes: connection closed early"
        );
    }
}
