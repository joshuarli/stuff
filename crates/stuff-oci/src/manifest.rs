#![forbid(unsafe_code)]

use crate::digest::Digest;
use crate::error::{Error, ErrorCode};
use crate::json;
use crate::media;
use deser_value::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
    pub variant: Option<String>,
    pub os_version: Option<String>,
    pub features: Vec<String>,
    pub os_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    pub media_type: String,
    pub digest: Digest,
    pub size: u64,
    pub artifact_type: Option<String>,
    pub annotations: BTreeMap<String, String>,
    pub platform: Option<Platform>,
    pub urls: Vec<String>,
}

impl Descriptor {
    pub fn render_json(&self) -> String {
        let mut out = String::from("{\"mediaType\":");
        out.push_str(&json::quoted(&self.media_type));
        out.push_str(",\"digest\":");
        out.push_str(&json::quoted(self.digest.as_str()));
        out.push_str(&format!(",\"size\":{}", self.size));
        if let Some(artifact_type) = &self.artifact_type {
            out.push_str(",\"artifactType\":");
            out.push_str(&json::quoted(artifact_type));
        }
        if let Some(platform) = &self.platform {
            out.push_str(",\"platform\":{\"architecture\":");
            out.push_str(&json::quoted(&platform.architecture));
            out.push_str(",\"os\":");
            out.push_str(&json::quoted(&platform.os));
            if let Some(variant) = &platform.variant {
                out.push_str(",\"variant\":");
                out.push_str(&json::quoted(variant));
            }
            if let Some(os_version) = &platform.os_version {
                out.push_str(",\"os.version\":");
                out.push_str(&json::quoted(os_version));
            }
            if !platform.features.is_empty() {
                out.push_str(",\"features\":[");
                out.push_str(&render_strings(&platform.features));
                out.push(']');
            }
            if !platform.os_features.is_empty() {
                out.push_str(",\"os.features\":[");
                out.push_str(&render_strings(&platform.os_features));
                out.push(']');
            }
            out.push('}');
        }
        if !self.annotations.is_empty() {
            out.push_str(",\"annotations\":");
            out.push_str(&render_string_map(&self.annotations));
        }
        if !self.urls.is_empty() {
            out.push_str(",\"urls\":[");
            out.push_str(&render_strings(&self.urls));
            out.push(']');
        }
        out.push('}');
        out
    }
}

fn render_strings(values: &[String]) -> String {
    values.iter().map(|value| json::quoted(value)).collect::<Vec<_>>().join(",")
}

fn render_string_map(map: &BTreeMap<String, String>) -> String {
    let entries: Vec<String> = map
        .iter()
        .map(|(key, value)| format!("{}:{}", json::quoted(key), json::quoted(value)))
        .collect();
    format!("{{{}}}", entries.join(","))
}

pub(crate) fn parse_descriptor(value: &Value, context: &str) -> Result<Descriptor, Error> {
    let map = value
        .as_map()
        .ok_or_else(|| Error::manifest_invalid(format!("{context} must be an object")))?;
    let media_type = json::string_field(map, "mediaType", context)?;
    if media_type.is_empty() {
        return Err(Error::manifest_invalid(format!("{context}.mediaType must not be empty")));
    }
    let digest_text = json::string_field(map, "digest", context)?;
    let digest = Digest::parse(&digest_text)?;
    let size = map
        .get("size")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::manifest_invalid(format!("{context}.size must be a non-negative integer")))?;
    let artifact_type = json::optional_string_field(map, "artifactType", context)?;
    let annotations = match map.get("annotations") {
        Some(value) => json::string_map(value, &format!("{context}.annotations"))?,
        None => BTreeMap::new(),
    };
    let platform = match map.get("platform") {
        Some(value) => Some(parse_platform(value, &format!("{context}.platform"))?),
        None => None,
    };
    let urls = match map.get("urls") {
        Some(value) => {
            let seq = value
                .as_seq()
                .ok_or_else(|| Error::manifest_invalid(format!("{context}.urls must be an array")))?;
            let mut urls = Vec::with_capacity(seq.len());
            for item in seq.iter() {
                let url = item
                    .as_str()
                    .ok_or_else(|| Error::manifest_invalid(format!("{context}.urls must be strings")))?;
                urls.push(url.to_string());
            }
            urls
        }
        None => Vec::new(),
    };
    Ok(Descriptor { media_type, digest, size, artifact_type, annotations, platform, urls })
}

fn parse_platform(value: &Value, context: &str) -> Result<Platform, Error> {
    let map = value
        .as_map()
        .ok_or_else(|| Error::manifest_invalid(format!("{context} must be an object")))?;
    let architecture = json::string_field(map, "architecture", context)?;
    let os = json::string_field(map, "os", context)?;
    let variant = json::optional_string_field(map, "variant", context)?;
    let os_version = json::optional_string_field(map, "os.version", context)?;
    let features = string_array(map.get("features"), &format!("{context}.features"))?;
    let os_features = string_array(map.get("os.features"), &format!("{context}.os.features"))?;
    Ok(Platform { architecture, os, variant, os_version, features, os_features })
}

fn string_array(value: Option<&Value>, context: &str) -> Result<Vec<String>, Error> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let seq = value
        .as_seq()
        .ok_or_else(|| Error::manifest_invalid(format!("{context} must be an array")))?;
    let mut out = Vec::with_capacity(seq.len());
    for item in seq.iter() {
        let text = item
            .as_str()
            .ok_or_else(|| Error::manifest_invalid(format!("{context} must be strings")))?;
        out.push(text.to_string());
    }
    Ok(out)
}

fn parse_annotations(map: &deser_value::Map, context: &str) -> Result<BTreeMap<String, String>, Error> {
    match map.get("annotations") {
        Some(value) => json::string_map(value, &format!("{context}.annotations")),
        None => Ok(BTreeMap::new()),
    }
}

fn parse_subject(map: &deser_value::Map, context: &str) -> Result<Option<Descriptor>, Error> {
    map.get("subject")
        .map(|value| parse_descriptor(value, &format!("{context}.subject")))
        .transpose()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub media_type: String,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
    pub subject: Option<Descriptor>,
    pub artifact_type: Option<String>,
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    pub media_type: String,
    pub manifests: Vec<Descriptor>,
    pub subject: Option<Descriptor>,
    pub artifact_type: Option<String>,
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Image {
    Manifest(Box<Manifest>),
    Index(Box<Index>),
}

impl Image {
    pub fn media_type(&self) -> &str {
        match self {
            Self::Manifest(manifest) => &manifest.media_type,
            Self::Index(index) => &index.media_type,
        }
    }

    pub fn subject(&self) -> Option<&Descriptor> {
        match self {
            Self::Manifest(manifest) => manifest.subject.as_ref(),
            Self::Index(index) => index.subject.as_ref(),
        }
    }

    pub fn artifact_type(&self) -> Option<&str> {
        match self {
            Self::Manifest(manifest) => manifest.artifact_type.as_deref(),
            Self::Index(index) => index.artifact_type.as_deref(),
        }
    }

    pub fn annotations(&self) -> &BTreeMap<String, String> {
        match self {
            Self::Manifest(manifest) => &manifest.annotations,
            Self::Index(index) => &index.annotations,
        }
    }

    pub fn descriptor(&self, digest: Digest, size: u64) -> Descriptor {
        Descriptor {
            media_type: self.media_type().to_string(),
            digest,
            size,
            artifact_type: self.artifact_type().map(str::to_string),
            annotations: self.annotations().clone(),
            platform: None,
            urls: Vec::new(),
        }
    }
}

pub fn parse_image(bytes: &[u8]) -> Result<Image, Error> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::manifest_invalid(format!(
            "manifest exceeds the {MAX_MANIFEST_BYTES}-byte limit"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::manifest_invalid("manifest is not valid UTF-8"))?;
    let value = json::parse(text, "manifest")?;
    let map = value
        .as_map()
        .ok_or_else(|| Error::manifest_invalid("manifest must be an object"))?;
    let schema_version = map
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| Error::manifest_invalid("manifest.schemaVersion must be an integer"))?;
    if schema_version != 2 {
        return Err(Error::manifest_invalid(format!(
            "unsupported manifest schemaVersion {schema_version}"
        )));
    }
    let media_type = json::string_field(map, "mediaType", "manifest")?;
    if media_type == media::DOCKER_SCHEMA1 || media_type == media::DOCKER_SCHEMA1_SIGNED {
        return Err(Error::manifest_invalid("schema 1 manifests are not supported"));
    }
    if media::is_index(&media_type) {
        let manifests_value = map
            .get("manifests")
            .ok_or_else(|| Error::manifest_invalid("index.manifests is required"))?;
        let seq = manifests_value
            .as_seq()
            .ok_or_else(|| Error::manifest_invalid("index.manifests must be an array"))?;
        let mut manifests = Vec::with_capacity(seq.len());
        for (i, item) in seq.iter().enumerate() {
            manifests.push(parse_descriptor(item, &format!("index.manifests[{i}]"))?);
        }
        return Ok(Image::Index(Box::new(Index {
            media_type,
            manifests,
            subject: parse_subject(map, "index")?,
            artifact_type: json::optional_string_field(map, "artifactType", "index")?,
            annotations: parse_annotations(map, "index")?,
        })));
    }
    if !media::is_manifest(&media_type) {
        return Err(Error::manifest_invalid(format!("unsupported manifest mediaType {media_type}")));
    }
    let config_value = map
        .get("config")
        .ok_or_else(|| Error::manifest_invalid("manifest.config is required"))?;
    let config = parse_descriptor(config_value, "manifest.config")?;
    let layers_value = map
        .get("layers")
        .ok_or_else(|| Error::manifest_invalid("manifest.layers is required"))?;
    let layers_seq = layers_value
        .as_seq()
        .ok_or_else(|| Error::manifest_invalid("manifest.layers must be an array"))?;
    let mut layers = Vec::with_capacity(layers_seq.len());
    for (i, item) in layers_seq.iter().enumerate() {
        layers.push(parse_descriptor(item, &format!("manifest.layers[{i}]"))?);
    }
    Ok(Image::Manifest(Box::new(Manifest {
        media_type,
        config,
        layers,
        subject: parse_subject(map, "manifest")?,
        artifact_type: json::optional_string_field(map, "artifactType", "manifest")?,
        annotations: parse_annotations(map, "manifest")?,
    })))
}

pub fn validate_manifest_push(
    bytes: &[u8],
    blobs: &BTreeSet<Digest>,
    manifests: &BTreeSet<Digest>,
) -> Result<(Digest, Image), Error> {
    let image = parse_image(bytes)?;
    let digest = Digest::from_bytes(bytes);
    match &image {
        Image::Manifest(manifest) => {
            for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
                if !blob.urls.is_empty() {
                    continue;
                }
                if !blobs.contains(&blob.digest) {
                    return Err(Error::new(
                        ErrorCode::ManifestBlobUnknown,
                        "manifest references a blob that is not present",
                    )
                    .with_detail(blob.digest.as_str()));
                }
            }
        }
        Image::Index(index) => {
            for child in &index.manifests {
                if !manifests.contains(&child.digest) {
                    return Err(Error::new(
                        ErrorCode::ManifestBlobUnknown,
                        "index references a manifest that is not present",
                    )
                    .with_detail(child.digest.as_str()));
                }
            }
        }
    }
    Ok((digest, image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    pub(crate) fn manifest_json(layers: &str) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55","size":292}},"layers":[{layers}]}}"#
        )
    }

    #[test]
    fn validates_manifest_push_references() {
        use std::collections::BTreeSet;
        let layer = "882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f";
        let config = "1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55";
        let body = manifest_json(&format!(
            r#"{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:{layer}","size":117}}"#
        ));
        let mut blobs = BTreeSet::new();
        blobs.insert(Digest::parse(&format!("sha256:{config}")).unwrap());
        blobs.insert(Digest::parse(&format!("sha256:{layer}")).unwrap());
        let manifests = BTreeSet::new();
        let (digest, _) = validate_manifest_push(body.as_bytes(), &blobs, &manifests).unwrap();
        assert_eq!(digest, Digest::from_bytes(body.as_bytes()));
        let error = validate_manifest_push(body.as_bytes(), &BTreeSet::new(), &manifests).unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestBlobUnknown);

        let child = "576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4";
        let index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{child}","size":401}}]}}"#
        );
        let mut children = BTreeSet::new();
        children.insert(Digest::parse(&format!("sha256:{child}")).unwrap());
        assert!(validate_manifest_push(index.as_bytes(), &blobs, &children).is_ok());
        assert_eq!(
            validate_manifest_push(index.as_bytes(), &blobs, &BTreeSet::new()).unwrap_err().code,
            ErrorCode::ManifestBlobUnknown
        );
    }

    #[test]
    fn parses_an_oci_manifest_and_descriptors() {
        let layer = r#"{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f","size":117,"annotations":{"org.opencontainers.image.title":"hello.txt"}}"#;
        let image = parse_image(manifest_json(layer).as_bytes()).unwrap();
        let Image::Manifest(manifest) = image else { panic!("expected manifest") };
        assert_eq!(manifest.media_type, media::OCI_MANIFEST);
        assert_eq!(manifest.config.size, 292);
        assert_eq!(manifest.layers.len(), 1);
        assert_eq!(manifest.layers[0].annotations["org.opencontainers.image.title"], "hello.txt");
        let rendered = manifest.layers[0].render_json();
        assert!(rendered.contains("\"annotations\":{\"org.opencontainers.image.title\":\"hello.txt\"}"));
    }

    #[test]
    fn parses_indexes_and_platforms() {
        let body = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4","size":401,"platform":{"architecture":"arm64","os":"linux"}}]}"#;
        let Image::Index(index) = parse_image(body.as_bytes()).unwrap() else { panic!("expected index") };
        assert_eq!(index.manifests[0].platform.as_ref().unwrap().architecture, "arm64");
        assert_eq!(index.manifests[0].platform.as_ref().unwrap().os, "linux");
    }

    #[test]
    fn rejects_schema_one_and_bad_digests() {
        let schema1 = br#"{"schemaVersion":1,"mediaType":"application/vnd.docker.distribution.manifest.v1+json","fsLayers":[]}"#;
        assert_eq!(parse_image(schema1).unwrap_err().code, ErrorCode::ManifestInvalid);
        let foreign = r#"{"mediaType":"application/vnd.docker.image.rootfs.foreign.diff.tar.gzip","digest":"sha256:882765efc0f62ee831f26df7bd0a5098a1c10dbbe340dd03d84311b2d8cdeb6f","size":117}"#;
        assert!(parse_image(manifest_json(foreign).as_bytes()).is_ok());
        let bad = r#"{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha512:00","size":1}"#;
        assert_eq!(parse_image(manifest_json(bad).as_bytes()).unwrap_err().code, ErrorCode::DigestInvalid);
        assert!(parse_image(&[0xff]).is_err());
    }
}
