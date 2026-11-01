#![forbid(unsafe_code)]

use stuff_catalog::{oci_catalog, render_catalog_json, render_html, render_llms_txt, OciInput};
use stuff_oci::{
    filter_referrers, generation_id, parse_blobs_json, parse_referrers, parse_repository_json,
    plan_upload_mounted, plan_upload_start, render_blobs_json, render_referrers,
    render_repository_json, validate_manifest_push, Descriptor, Digest, Error, ErrorCode, Image,
    Method, ParsedWrite, Reference, RepositoryName, RepositoryView, SessionToken, WriteMethod,
    DEFAULT_SESSION_TTL_SECS,
};
use std::collections::{BTreeMap, BTreeSet};

fn quote(text: &str) -> String {
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

fn parse(text: &str, what: &str) -> Result<deser_value::Value, String> {
    deser_json::from_str(text).map_err(|error| format!("{what}: {error}"))
}

fn strings(value: &deser_value::Value, what: &str) -> Result<Vec<String>, String> {
    let seq = value.as_seq().ok_or_else(|| format!("{what} must be an array"))?;
    let mut out = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        out.push(
            item.as_str()
                .ok_or_else(|| format!("{what} must be strings"))?
                .to_string(),
        );
    }
    Ok(out)
}

pub fn oci_auth_check(header: &str, token: &str) -> bool {
    let Some((user, password)) = stuff_oci::parse_basic_auth(header) else {
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

pub fn oci_write_request_json(method: &str, target: &str) -> String {
    let parsed = stuff_oci::parse_write_request(Method::parse(method), target);
    let mut out = String::from("{\"kind\":");
    match parsed {
        ParsedWrite::Request(request) => {
            out.push_str("\"write\",\"repository\":");
            out.push_str(&quote(request.repository.as_str()));
            out.push_str(",\"session\":");
            out.push_str(&request.session.as_deref().map(quote).unwrap_or_else(|| "null".to_string()));
            match request.method {
                WriteMethod::UploadStart => out.push_str(",\"method\":\"start\""),
                WriteMethod::UploadMount { mount, from } => {
                    out.push_str(",\"method\":\"mount\",\"mount\":");
                    out.push_str(&quote(mount.as_str()));
                    if let Some(from) = from {
                        out.push_str(",\"from\":");
                        out.push_str(&quote(from.as_str()));
                    }
                }
                WriteMethod::UploadStatus { upload_id } => {
                    out.push_str(",\"method\":\"status\",\"upload_id\":");
                    out.push_str(&quote(&upload_id));
                }
                WriteMethod::UploadPatch { upload_id } => {
                    out.push_str(",\"method\":\"patch\",\"upload_id\":");
                    out.push_str(&quote(&upload_id));
                }
                WriteMethod::UploadPut { upload_id, digest } => {
                    out.push_str(",\"method\":\"put\",\"upload_id\":");
                    out.push_str(&quote(&upload_id));
                    out.push_str(",\"digest\":");
                    out.push_str(&quote(digest.as_str()));
                }
                WriteMethod::UploadCancel { upload_id } => {
                    out.push_str(",\"method\":\"cancel\",\"upload_id\":");
                    out.push_str(&quote(&upload_id));
                }
                WriteMethod::ManifestPut { reference } => {
                    out.push_str(",\"method\":\"manifest_put\",\"reference\":");
                    out.push_str(&quote(reference.as_str()));
                }
            }
        }
        ParsedWrite::NotFound => out.push_str("\"not_found\""),
        ParsedWrite::Unsupported => out.push_str("\"unsupported\""),
        ParsedWrite::Invalid(error) => {
            out.push_str("\"error\",\"status\":");
            out.push_str(&error.status().to_string());
            out.push_str(",\"body\":");
            out.push_str(&quote(&error.to_json()));
        }
    }
    out.push('}');
    out
}

pub fn oci_session_json(
    repository: &str,
    writer_origin: &str,
    secret: &str,
    now: u64,
    upload_id: &str,
) -> String {
    let repository = match RepositoryName::parse(repository) {
        Ok(repository) => repository,
        Err(error) => return error_json(&error),
    };
    if upload_id.is_empty()
        || !upload_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'='))
    {
        return error_json(&Error::new(
            ErrorCode::BlobUploadInvalid,
            "invalid upload id",
        ));
    }
    let expires = now + DEFAULT_SESSION_TTL_SECS;
    let token = SessionToken::new(repository.as_str(), upload_id, expires).encode(secret.as_bytes());
    let location = format!(
        "{}/v2/{}/blobs/uploads/{}?session={}&offset=0",
        writer_origin.trim_end_matches('/'),
        repository.as_str(),
        upload_id,
        token
    );
    let plan = plan_upload_start(&location, upload_id);
    crate::oci::plan_to_json(plan, None, None, false)
}

pub fn oci_mount_json(repository: &str, digest: &str) -> String {
    match (RepositoryName::parse(repository), Digest::parse(digest)) {
        (Ok(repository), Ok(digest)) => {
            let location = format!("/v2/{}/blobs/{}", repository.as_str(), digest.as_str());
            crate::oci::plan_to_json(plan_upload_mounted(&location, &digest), None, None, false)
        }
        (Err(error), _) | (_, Err(error)) => error_json(&error),
    }
}

pub fn oci_manifest_refs_json(body: &str) -> String {
    let image = match stuff_oci::parse_image(body.as_bytes()) {
        Ok(image) => image,
        Err(error) => return error_json(&error),
    };
    let digest = Digest::from_bytes(body.as_bytes());
    let mut blobs = Vec::new();
    let mut children = Vec::new();
    match &image {
        Image::Manifest(manifest) => {
            blobs.push(manifest.config.digest.clone());
            for layer in &manifest.layers {
                blobs.push(layer.digest.clone());
            }
        }
        Image::Index(index) => {
            for child in &index.manifests {
                children.push(child.digest.clone());
            }
        }
    }
    let mut out = format!(
        "{{\"kind\":\"refs\",\"digest\":{},\"media_type\":{},\"blobs\":[",
        quote(digest.as_str()),
        quote(image.media_type())
    );
    out.push_str(&digests_json(&blobs));
    out.push_str("],\"children\":[");
    out.push_str(&digests_json(&children));
    out.push_str("]}");
    out
}

fn digests_json(digests: &[Digest]) -> String {
    digests.iter().map(|digest| quote(digest.as_str())).collect::<Vec<_>>().join(",")
}

fn error_json(error: &Error) -> String {
    format!(
        "{{\"kind\":\"error\",\"status\":{},\"body\":{}}}",
        error.status(),
        quote(&error.to_json())
    )
}

fn internal_error(message: &str) -> String {
    format!(
        "{{\"kind\":\"error\",\"status\":500,\"body\":{}}}",
        quote(&format!(
            "{{\"errors\":[{{\"code\":\"UNKNOWN\",\"message\":{}}}]}}",
            quote(message)
        ))
    )
}

#[allow(clippy::too_many_arguments)]
pub fn oci_generation_json(
    repository: &str,
    repositories_json: &str,
    blobs_text: &str,
    referrers_json: &str,
    manifest_body: &str,
    reference: &str,
    available_blobs_json: &str,
    available_manifests_json: &str,
    public_url: &str,
) -> String {
    let repository = match RepositoryName::parse(repository) {
        Ok(repository) => repository,
        Err(error) => return error_json(&error),
    };
    let reference = match Reference::parse(reference) {
        Ok(reference) => reference,
        Err(error) => return error_json(&error),
    };
    let available_blobs = match parse(available_blobs_json, "available blobs")
        .and_then(|value| strings(&value, "available blobs"))
    {
        Ok(values) => values,
        Err(error) => return internal_error(&error),
    };
    let available_blobs: BTreeSet<Digest> = available_blobs
        .iter()
        .filter_map(|value| Digest::parse(value).ok())
        .collect();
    let available_manifests = match parse(available_manifests_json, "available manifests")
        .and_then(|value| strings(&value, "available manifests"))
    {
        Ok(values) => values,
        Err(error) => return internal_error(&error),
    };
    let available_manifests: BTreeSet<Digest> = available_manifests
        .iter()
        .filter_map(|value| Digest::parse(value).ok())
        .collect();

    let repositories_value = match parse(repositories_json, "repositories") {
        Ok(value) => value,
        Err(error) => return internal_error(&error),
    };
    let Some(repository_entries) = repositories_value.as_seq() else {
        return internal_error("repositories must be an array");
    };
    let mut repositories: BTreeMap<String, String> = BTreeMap::new();
    for entry in repository_entries.iter() {
        let Some(map) = entry.as_map() else {
            return internal_error("repository entry must be an object");
        };
        let (Some(name), Some(text)) = (
            map.get("name").and_then(|value| value.as_str()),
            map.get("text").and_then(|value| value.as_str()),
        ) else {
            return internal_error("repository entry must have name and text");
        };
        repositories.insert(name.to_string(), text.to_string());
    }
    let updated = match repository_text(
        repository.as_str(),
        repositories.get(repository.as_str()).map(String::as_str),
        manifest_body,
        &reference,
        &available_blobs,
        &available_manifests,
    ) {
        Ok(text) => text,
        Err(error) => return error_json(&error),
    };
    repositories.insert(repository.as_str().to_string(), updated);
    let Some(view_text) = repositories.get(repository.as_str()) else {
        return internal_error("repository text missing");
    };
    if let Err(error) = parse_repository_json(&repository, view_text) {
        return error_json(&error);
    }
    let mut blobs = parse_blobs_json(blobs_text).unwrap_or_default();
    if let Image::Manifest(manifest) = match stuff_oci::parse_image(manifest_body.as_bytes()) {
        Ok(image) => image,
        Err(error) => return error_json(&error),
    } {
        blobs.insert(manifest.config.digest.clone());
        for layer in &manifest.layers {
            blobs.insert(layer.digest.clone());
        }
    }
    let mut referrers: BTreeMap<Digest, Vec<Descriptor>> = BTreeMap::new();
    if let Ok(value) = parse(referrers_json, "referrers")
        && let Some(seq) = value.as_seq()
    {
        for entry in seq.iter() {
            let Some(map) = entry.as_map() else { continue };
            let (Some(subject), Some(text)) = (
                map.get("subject").and_then(|value| value.as_str()),
                map.get("text").and_then(|value| value.as_str()),
            ) else {
                continue;
            };
            let Ok(subject) = Digest::parse(subject) else { continue };
            if let Ok(descriptors) = parse_referrers(text) {
                referrers.entry(subject).or_default().extend(descriptors);
            }
        }
    }
    let image = match stuff_oci::parse_image(manifest_body.as_bytes()) {
        Ok(image) => image,
        Err(error) => return error_json(&error),
    };
    let digest = Digest::from_bytes(manifest_body.as_bytes());
    if let Reference::Digest(expected) = &reference
        && expected != &digest
    {
        return error_json(&Error::digest_invalid(format!(
            "manifest digest is {digest}, not {expected}"
        )));
    }
    let descriptor = image.descriptor(digest.clone(), manifest_body.len() as u64);
    if let Some(subject) = image.subject() {
        referrers.entry(subject.digest.clone()).or_default().push(descriptor);
    }

    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut order: Vec<String> = repositories.keys().cloned().collect();
    order.sort();
    for name in &order {
        let text = repositories.get(name).expect("repository");
        files.insert(format!("repositories/{name}/repository.json"), text.as_bytes().to_vec());
    }
    for (subject, descriptors) in &referrers {
        let sorted = filter_referrers(descriptors, None);
        files.insert(
            format!(
                "repositories/{}/referrers/sha256/{}.json",
                repository.as_str(),
                subject.hex()
            ),
            render_referrers(&sorted).into_bytes(),
        );
    }
    files.insert("blobs.json".to_string(), render_blobs_json(&blobs).into_bytes());
    let generation = generation_id(&files);
    let mut inputs = Vec::new();
    for name in &order {
        let Ok(name) = RepositoryName::parse(name) else { continue };
        let Ok(view) = parse_repository_json(&name, repositories.get(name.as_str()).expect("repository"))
        else {
            continue;
        };
        inputs.push(OciInput {
            name: name.as_str().to_string(),
            tags: view.tags.keys().map(|tag| tag.as_str().to_string()).collect(),
        });
    }
    let catalog = match oci_catalog(&generation, inputs) {
        Ok(catalog) => catalog,
        Err(error) => return internal_error(&format!("catalog: {error}")),
    };
    files.insert("catalog.json".to_string(), render_catalog_json(&catalog).into_bytes());
    files.insert("index.html".to_string(), render_html(&catalog).into_bytes());
    files.insert("llms.txt".to_string(), render_llms_txt(&catalog, public_url).into_bytes());

    let mut out = format!(
        "{{\"kind\":\"ok\",\"generation\":{},\"digest\":{},\"manifest_key\":{},\"files\":[",
        quote(&generation),
        quote(digest.as_str()),
        quote(&format!("oci/manifests/sha256/{}", digest.hex()))
    );
    for (index, (key, bytes)) in files.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "{{\"key\":{},\"text\":{}}}",
            quote(key),
            quote(&String::from_utf8_lossy(bytes))
        ));
    }
    out.push_str("]}");
    out
}

fn repository_text(
    name: &str,
    existing: Option<&str>,
    manifest_body: &str,
    reference: &Reference,
    available_blobs: &BTreeSet<Digest>,
    available_manifests: &BTreeSet<Digest>,
) -> Result<String, Error> {
    let repository = RepositoryName::parse(name)?;
    let mut view = match existing {
        Some(text) => parse_repository_json(&repository, text)?,
        None => RepositoryView::new(repository),
    };
    let (digest, image) =
        validate_manifest_push(manifest_body.as_bytes(), available_blobs, available_manifests)?;
    let descriptor = image.descriptor(digest.clone(), manifest_body.len() as u64);
    view.insert_manifest(descriptor.clone());
    if let Reference::Tag(tag) = reference {
        view.tag(tag.clone(), descriptor);
    }
    Ok(render_repository_json(&view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_a_session_plan() {
        let value = oci_session_json("example/app", "https://writer.test", "secret", 1000, "upload1");
        assert!(value.contains("\"kind\":\"plan\""), "{value}");
        assert!(value.contains("\"status\":202"), "{value}");
        assert!(value.contains("session="), "{value}");
    }
}
