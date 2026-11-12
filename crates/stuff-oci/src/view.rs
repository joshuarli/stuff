#![forbid(unsafe_code)]

use crate::digest::Digest;
use crate::error::{Error, ErrorCode};
use crate::json;
use crate::manifest::Descriptor;
use crate::manifest::parse_descriptor;
use crate::reference::{RepositoryName, Tag};
use rustc_hash::{FxHashMap, FxHashSet};

pub const REPOSITORY_SCHEMA_VERSION: u32 = 1;
pub const BLOBS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryView {
    pub name: RepositoryName,
    pub tags: FxHashMap<Tag, Descriptor>,
    pub manifests: FxHashMap<Digest, Descriptor>,
}

impl RepositoryView {
    pub fn new(name: RepositoryName) -> Self {
        Self { name, tags: FxHashMap::default(), manifests: FxHashMap::default() }
    }

    pub fn insert_manifest(&mut self, descriptor: Descriptor) {
        self.manifests.insert(descriptor.digest.clone(), descriptor);
    }

    pub fn tag(&mut self, tag: Tag, descriptor: Descriptor) {
        self.tags.insert(tag, descriptor);
    }

    pub fn remove_tag(&mut self, tag: &Tag) -> Option<Descriptor> {
        self.tags.remove(tag)
    }

    pub fn is_manifest(&self, digest: &Digest) -> bool {
        self.manifests.contains_key(digest)
    }
}

pub fn render_repository_json(view: &RepositoryView) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"schemaVersion\": {REPOSITORY_SCHEMA_VERSION},\n"));
    out.push_str("  \"repository\": ");
    out.push_str(&json::quoted(view.name.as_str()));
    out.push_str(",\n  \"tags\": {");
    let mut tags: Vec<_> = view.tags.iter().collect();
    tags.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut first = true;
    for (tag, descriptor) in tags {
        out.push_str(if first { "\n" } else { ",\n" });
        first = false;
        out.push_str("    ");
        out.push_str(&json::quoted(tag.as_str()));
        out.push_str(": ");
        out.push_str(&descriptor.render_json());
    }
    if !first {
        out.push_str("\n  ");
    }
    out.push_str("},\n  \"manifests\": [");
    if view.manifests.is_empty() {
        out.push_str("]\n}\n");
        return out;
    }
    out.push('\n');
    let mut manifests: Vec<_> = view.manifests.iter().collect();
    manifests.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (i, (_, descriptor)) in manifests.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&descriptor.render_json());
        if i + 1 < manifests.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

pub fn parse_repository_json(name: &RepositoryName, text: &str) -> Result<RepositoryView, Error> {
    let value = json::parse(text, "repository")?;
    let map = value
        .as_map()
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository metadata must be an object"))?;
    let schema = map
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository.schemaVersion is required"))?;
    if schema != u64::from(REPOSITORY_SCHEMA_VERSION) {
        return Err(Error::new(
            ErrorCode::ManifestInvalid,
            format!("unsupported repository schemaVersion {schema}"),
        ));
    }
    let stored_name = map
        .get("repository")
        .and_then(|value| value.as_str())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository.repository is required"))?;
    if stored_name != name.as_str() {
        return Err(Error::new(
            ErrorCode::ManifestInvalid,
            format!("repository metadata names {stored_name}, expected {}", name.as_str()),
        ));
    }
    let mut view = RepositoryView::new(name.clone());
    let tags = map
        .get("tags")
        .and_then(|value| value.as_map())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository.tags must be an object"))?;
    for (tag, value) in tags.iter() {
        let tag_text = tag
            .as_str()
            .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository tag keys must be strings"))?;
        let tag = Tag::parse(tag_text)?;
        let descriptor = parse_descriptor(value, &format!("repository.tags.{tag_text}"))?;
        view.tags.insert(tag, descriptor);
    }
    let manifests = map
        .get("manifests")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "repository.manifests must be an array"))?;
    for (i, value) in manifests.iter().enumerate() {
        let descriptor = parse_descriptor(value, &format!("repository.manifests[{i}]"))?;
        if view.manifests.insert(descriptor.digest.clone(), descriptor).is_some() {
            return Err(Error::new(
                ErrorCode::ManifestInvalid,
                "repository.manifests contains a duplicate digest",
            ));
        }
    }
    Ok(view)
}

pub fn render_blobs_json(blobs: &FxHashSet<Digest>) -> String {
    let mut out = format!("{{\"schemaVersion\":{BLOBS_SCHEMA_VERSION},\"blobs\":[");
    let mut digests: Vec<_> = blobs.iter().collect();
    digests.sort_unstable();
    for (i, digest) in digests.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json::quoted(digest.as_str()));
    }
    out.push_str("]}\n");
    out
}

pub fn parse_blobs_json(text: &str) -> Result<FxHashSet<Digest>, Error> {
    let value = json::parse(text, "blobs")?;
    let map = value
        .as_map()
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "blobs metadata must be an object"))?;
    let schema = map
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "blobs.schemaVersion is required"))?;
    if schema != u64::from(BLOBS_SCHEMA_VERSION) {
        return Err(Error::new(
            ErrorCode::ManifestInvalid,
            format!("unsupported blobs schemaVersion {schema}"),
        ));
    }
    let seq = map
        .get("blobs")
        .and_then(|value| value.as_seq())
        .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "blobs.blobs must be an array"))?;
    let mut out = FxHashSet::default();
    for (i, value) in seq.iter().enumerate() {
        let text = value
            .as_str()
            .ok_or_else(|| Error::new(ErrorCode::ManifestInvalid, "blobs entries must be strings"))?;
        let digest = Digest::parse(text).map_err(|e| {
            Error::new(ErrorCode::ManifestInvalid, format!("blobs.blobs[{i}]: {}", e.message))
        })?;
        if !out.insert(digest) {
            return Err(Error::new(ErrorCode::ManifestInvalid, "blobs contains a duplicate digest"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_image;
    use crate::media;

    fn descriptor(digest: &str, media_type: &str, size: u64) -> Descriptor {
        Descriptor {
            media_type: media_type.to_string(),
            digest: Digest::parse(digest).unwrap(),
            size,
            artifact_type: None,
            annotations: FxHashMap::default(),
            platform: None,
            urls: Vec::new(),
        }
    }

    fn view() -> RepositoryView {
        let name = RepositoryName::parse("example/app").unwrap();
        let mut view = RepositoryView::new(name);
        let digest = "sha256:04237663eeeaf2ee4f6dec1a5d23c8a7c1bc70275cbfa205e6f35377603c6388";
        let descriptor = descriptor(digest, media::OCI_INDEX, 491);
        view.insert_manifest(descriptor.clone());
        view.tag(Tag::parse("stable").unwrap(), descriptor);
        view
    }

    #[test]
    fn round_trips_repository_metadata() {
        let view = view();
        let rendered = render_repository_json(&view);
        assert!(rendered.starts_with("{\n  \"schemaVersion\": 1,\n  \"repository\": \"example/app\",\n"));
        let parsed = parse_repository_json(&view.name, &rendered).unwrap();
        assert_eq!(parsed, view);
    }

    #[test]
    fn rejects_a_mismatched_repository_name() {
        let view = view();
        let rendered = render_repository_json(&view);
        let other = RepositoryName::parse("example/other").unwrap();
        assert!(parse_repository_json(&other, &rendered).is_err());
    }

    #[test]
    fn round_trips_blobs_and_rejects_duplicates() {
        let blobs: FxHashSet<Digest> = [
            Digest::parse("sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f").unwrap(),
            Digest::parse("sha256:bf2585f7b79b5ba8a5d7f256f7c2ce5a537ebc1c0d9e043ca90c461f2183f718").unwrap(),
        ]
        .into_iter()
        .collect();
        let rendered = render_blobs_json(&blobs);
        assert_eq!(parse_blobs_json(&rendered).unwrap(), blobs);
        let duplicate = r#"{"schemaVersion":1,"blobs":["sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f","sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f"]}"#;
        assert!(parse_blobs_json(duplicate).is_err());
    }

    #[test]
    fn descriptor_from_a_parsed_image_carries_subject_metadata() {
        let referrer = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.stuff.sbom.v1+json","config":{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[],"subject":{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4","size":401}}"#;
        let image = parse_image(referrer.as_bytes()).unwrap();
        let digest = Digest::from_bytes(referrer.as_bytes());
        let descriptor = image.descriptor(digest, referrer.len() as u64);
        assert_eq!(descriptor.artifact_type.as_deref(), Some("application/vnd.stuff.sbom.v1+json"));
        assert_eq!(image.subject().unwrap().size, 401);
    }
}
