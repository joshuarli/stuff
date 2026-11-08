#![forbid(unsafe_code)]

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use md5::{Digest as _, Md5};
use quick_xml::events::Event;
use quick_xml::Reader;

use crate::publish::PublishError;

use super::client::{RawRequest, R2Client};
use super::sigv4;

pub const PART_SIZE: u64 = 64 * 1024 * 1024;
pub const DEFAULT_SINGLE_PUT_MAX: u64 = 300 * 1024 * 1024;

const HASH_CHUNK: usize = 1 << 20;
const PART_CONTENT_TYPE: &str = "application/octet-stream";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartUpload {
    pub key: String,
    pub size: u64,
    pub parts: usize,
    pub etag: String,
}

pub fn md5_hex(bytes: &[u8]) -> String {
    sigv4::hex(Md5::digest(bytes))
}

pub fn md5_file(path: &Path) -> Result<String, PublishError> {
    let mut file = open(path)?;
    let mut buffer = vec![0_u8; HASH_CHUNK];
    let mut hasher = Md5::new();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| read_error(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(sigv4::hex(hasher.finalize()))
}

pub fn plan_parts(size: u64, part_size: u64) -> Result<usize, PublishError> {
    if part_size == 0 {
        return Err(PublishError::Config(
            "multipart part size must be positive".to_string(),
        ));
    }
    Ok(size.div_ceil(part_size) as usize)
}

pub fn combined_etag(part_digests: &[Vec<u8>]) -> String {
    let mut joined = Vec::with_capacity(part_digests.iter().map(Vec::len).sum());
    for digest in part_digests {
        joined.extend_from_slice(digest);
    }
    format!("{}-{}", sigv4::hex(Md5::digest(&joined)), part_digests.len())
}

pub fn multipart_etag_file(path: &Path, part_size: u64) -> Result<String, PublishError> {
    if part_size == 0 {
        return Err(PublishError::Config(
            "multipart part size must be positive".to_string(),
        ));
    }
    let mut file = open(path)?;
    let mut digests = Vec::new();
    loop {
        let mut chunk = Vec::new();
        let read = (&mut file)
            .take(part_size)
            .read_to_end(&mut chunk)
            .map_err(|error| read_error(path, error))?;
        if read == 0 {
            break;
        }
        digests.push(Md5::digest(&chunk).to_vec());
    }
    Ok(combined_etag(&digests))
}

pub fn etag_file(
    path: &Path,
    single_put_max: u64,
    part_size: u64,
) -> Result<String, PublishError> {
    let size = std::fs::metadata(path)
        .map_err(|error| read_error(path, error))?
        .len();
    if size > single_put_max {
        multipart_etag_file(path, part_size)
    } else {
        md5_file(path)
    }
}

pub fn completion_error(body: &[u8]) -> String {
    if body.iter().all(u8::is_ascii_whitespace) {
        return String::new();
    }
    let code = element_text(body, "Code").unwrap_or_default();
    if !code.is_empty() {
        return code;
    }
    element_text(body, "Message").unwrap_or_default()
}

pub fn initiate(client: &R2Client, bucket: &str, key: &str) -> Result<String, PublishError> {
    let response = client.request(RawRequest {
        method: "POST",
        bucket,
        key,
        query: vec![("uploads", String::new())],
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    })?;
    if response.status != 200 {
        return Err(PublishError::Transport(format!(
            "initiate multipart upload failed: HTTP {}",
            response.status
        )));
    }
    let upload_id = element_text(&response.body, "UploadId").unwrap_or_default();
    if upload_id.is_empty() {
        return Err(PublishError::Integrity(
            "initiate multipart upload returned no UploadId".to_string(),
        ));
    }
    Ok(upload_id)
}

pub fn upload_part(
    client: &R2Client,
    bucket: &str,
    key: &str,
    number: usize,
    upload_id: &str,
    chunk: &[u8],
) -> Result<String, PublishError> {
    let response = client.request(RawRequest {
        method: "PUT",
        bucket,
        key,
        query: vec![
            ("partNumber", number.to_string()),
            ("uploadId", upload_id.to_string()),
        ],
        body: chunk,
        content_type: Some(PART_CONTENT_TYPE),
        extra_headers: Vec::new(),
    })?;
    if response.status != 200 {
        return Err(PublishError::Transport(format!(
            "part {number} upload failed: HTTP {}",
            response.status
        )));
    }
    match response.header("etag") {
        Some(etag) if !etag.is_empty() => Ok(etag.to_string()),
        _ => Err(PublishError::Integrity(format!(
            "part {number} upload returned no ETag"
        ))),
    }
}

pub fn complete(
    client: &R2Client,
    bucket: &str,
    key: &str,
    upload_id: &str,
    etags: &[(usize, String)],
    content_type: &str,
) -> Result<(), PublishError> {
    let mut parts = String::new();
    for (number, etag) in etags {
        parts.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><CompleteMultipartUpload>{parts}</CompleteMultipartUpload>"
    );
    let response = client.request(RawRequest {
        method: "POST",
        bucket,
        key,
        query: vec![("uploadId", upload_id.to_string())],
        body: body.as_bytes(),
        content_type: Some(content_type),
        extra_headers: Vec::new(),
    })?;
    let error = completion_error(&response.body);
    if response.status != 200 || !error.is_empty() {
        let detail = if error.is_empty() {
            String::new()
        } else {
            format!(": {error}")
        };
        return Err(PublishError::Transport(format!(
            "complete multipart upload failed: HTTP {}{detail}",
            response.status
        )));
    }
    Ok(())
}

pub fn abort(client: &R2Client, bucket: &str, key: &str, upload_id: &str) {
    let _ = client.request(RawRequest {
        method: "DELETE",
        bucket,
        key,
        query: vec![("uploadId", upload_id.to_string())],
        body: &[],
        content_type: None,
        extra_headers: Vec::new(),
    });
}

pub fn upload_parts(
    client: &R2Client,
    bucket: &str,
    key: &str,
    path: &Path,
    upload_id: &str,
    content_type: &str,
    part_size: u64,
) -> Result<MultipartUpload, PublishError> {
    match stream_parts(
        client,
        bucket,
        key,
        path,
        upload_id,
        content_type,
        part_size,
    ) {
        Ok(upload) => Ok(upload),
        Err(error) => {
            abort(client, bucket, key, upload_id);
            Err(error)
        }
    }
}

pub fn upload_file(
    client: &R2Client,
    bucket: &str,
    key: &str,
    path: &Path,
    content_type: &str,
    part_size: u64,
) -> Result<MultipartUpload, PublishError> {
    if part_size == 0 {
        return Err(PublishError::Config(
            "multipart part size must be positive".to_string(),
        ));
    }
    open(path)?;
    let upload_id = initiate(client, bucket, key)?;
    upload_parts(
        client,
        bucket,
        key,
        path,
        &upload_id,
        content_type,
        part_size,
    )
}

fn stream_parts(
    client: &R2Client,
    bucket: &str,
    key: &str,
    path: &Path,
    upload_id: &str,
    content_type: &str,
    part_size: u64,
) -> Result<MultipartUpload, PublishError> {
    let mut file = open(path)?;
    let mut digests = Vec::new();
    let mut etags = Vec::new();
    let mut size = 0_u64;
    loop {
        let mut chunk = Vec::new();
        let read = (&mut file)
            .take(part_size)
            .read_to_end(&mut chunk)
            .map_err(|error| read_error(path, error))?;
        if read == 0 {
            break;
        }
        size += read as u64;
        let number = etags.len() + 1;
        let etag = upload_part(client, bucket, key, number, upload_id, &chunk)?;
        etags.push((number, etag));
        digests.push(Md5::digest(&chunk).to_vec());
    }
    complete(client, bucket, key, upload_id, &etags, content_type)?;
    let expected = combined_etag(&digests);
    let metadata = client.head_object(bucket, key)?;
    if metadata.etag != expected {
        return Err(PublishError::Integrity(format!(
            "multipart {key}: expected ETag {expected}, got {:?}",
            metadata.etag
        )));
    }
    Ok(MultipartUpload {
        key: key.to_string(),
        size,
        parts: etags.len(),
        etag: expected,
    })
}

fn element_text(body: &[u8], wanted: &str) -> Option<String> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut stack: Vec<(String, String)> = Vec::new();
    let mut found = None;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                stack.push((start.local_name().as_ref().to_string(), String::new()))
            }
            Ok(Event::Text(chunk)) => {
                if let Some((_, text)) = stack.last_mut() {
                    text.push_str(chunk.as_ref());
                }
            }
            Ok(Event::CData(chunk)) => {
                if let Some((_, text)) = stack.last_mut() {
                    text.push_str(chunk.as_ref());
                }
            }
            Ok(Event::End(_)) => {
                if let Some((name, text)) = stack.pop() {
                    if found.is_none() && name == wanted {
                        found = Some(text.trim().to_string());
                    }
                    if let Some((_, parent)) = stack.last_mut() {
                        parent.push_str(&text);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            Ok(_) => {}
        }
        buffer.clear();
    }
    found
}

fn open(path: &Path) -> Result<File, PublishError> {
    File::open(path).map_err(|error| read_error(path, error))
}

fn read_error(path: &Path, error: std::io::Error) -> PublishError {
    PublishError::Integrity(format!("could not read {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_plans_cover_empty_exact_and_partial_files() {
        assert_eq!(plan_parts(0, 4), Ok(0));
        assert_eq!(plan_parts(4, 4), Ok(1));
        assert_eq!(plan_parts(5, 4), Ok(2));
        assert_eq!(plan_parts(10, 4), Ok(3));
        assert_eq!(plan_parts(PART_SIZE, PART_SIZE), Ok(1));
        assert_eq!(plan_parts(PART_SIZE + 1, PART_SIZE), Ok(2));
        assert!(plan_parts(10, 0).is_err());
    }

    #[test]
    fn multipart_etag_is_md5_of_part_md5s_with_the_part_count() {
        let digests = vec![
            Md5::digest(b"abcd").to_vec(),
            Md5::digest(b"efgh").to_vec(),
            Md5::digest(b"ij").to_vec(),
        ];
        let mut joined = Vec::new();
        for digest in &digests {
            joined.extend_from_slice(digest);
        }
        let expected = format!("{}-3", md5_hex(&joined));
        assert_eq!(combined_etag(&digests), expected);

        let mut file = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut file, b"abcdefghij").unwrap();
        assert_eq!(multipart_etag_file(file.path(), 4).unwrap(), expected);
        assert_eq!(md5_file(file.path()).unwrap(), md5_hex(b"abcdefghij"));
        assert_eq!(
            etag_file(file.path(), 300, 4).unwrap(),
            md5_hex(b"abcdefghij")
        );
        assert_eq!(etag_file(file.path(), 2, 4).unwrap(), expected);
    }

    #[test]
    fn completion_errors_come_from_the_body_even_on_http_200() {
        assert_eq!(
            completion_error(
                b"<?xml version=\"1.0\"?><Error><Code>InternalError</Code><Message>try again</Message></Error>"
            ),
            "InternalError"
        );
        assert_eq!(
            completion_error(b"<Error><Message>try again</Message></Error>"),
            "try again"
        );
        assert_eq!(completion_error(b""), "");
        assert_eq!(completion_error(b"  \n"), "");
        assert_eq!(completion_error(b"not xml"), "");
        assert_eq!(
            completion_error(b"<CompleteMultipartUploadResult></CompleteMultipartUploadResult>"),
            ""
        );
    }
}
