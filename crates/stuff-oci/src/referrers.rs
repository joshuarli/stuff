#![forbid(unsafe_code)]

use crate::digest::Digest;
use crate::json;
use crate::manifest::{Descriptor, Image};
use crate::media;

pub fn referrer_descriptor(image: &Image, digest: Digest, size: u64) -> Option<Descriptor> {
    image.subject()?;
    Some(image.descriptor(digest, size))
}

pub fn filter_referrers(descriptors: &[Descriptor], artifact_type: Option<&str>) -> Vec<Descriptor> {
    let mut out: Vec<Descriptor> = descriptors
        .iter()
        .filter(|descriptor| match artifact_type {
            Some(expected) => descriptor.artifact_type.as_deref() == Some(expected),
            None => true,
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| a.digest.cmp(&b.digest));
    out
}

pub fn render_referrers(descriptors: &[Descriptor]) -> String {
    let mut out = format!(
        "{{\"schemaVersion\":2,\"mediaType\":{},\"manifests\":[",
        json::quoted(media::OCI_INDEX)
    );
    for (i, descriptor) in descriptors.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&descriptor.render_json());
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_image;

    fn image_with_subject() -> (Image, Descriptor) {
        let body = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.stuff.sbom.v1+json","annotations":{"org.opencontainers.image.created":"2026-11-01T00:00:00Z"},"config":{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a","size":2},"layers":[],"subject":{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4","size":401}}"#;
        let image = parse_image(body.as_bytes()).unwrap();
        let digest = Digest::from_bytes(body.as_bytes());
        let descriptor = referrer_descriptor(&image, digest, body.len() as u64).unwrap();
        (image, descriptor)
    }

    #[test]
    fn builds_and_filters_referrers() {
        let (_, sbom) = image_with_subject();
        let mut other = sbom.clone();
        other.digest = Digest::from_bytes(b"other");
        other.artifact_type = Some("application/vnd.example.signature".to_string());
        let all = vec![other.clone(), sbom.clone()];
        let filtered = filter_referrers(&all, Some("application/vnd.stuff.sbom.v1+json"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].digest, sbom.digest);
        let rendered = render_referrers(&all);
        assert!(rendered.starts_with("{\"schemaVersion\":2,\"mediaType\":\"application/vnd.oci.image.index.v1+json\",\"manifests\":["));
        assert!(rendered.contains("\"artifactType\":\"application/vnd.stuff.sbom.v1+json\""));
        assert_eq!(sbom.annotations["org.opencontainers.image.created"], "2026-11-01T00:00:00Z");
    }

    #[test]
    fn ignores_manifests_without_a_subject() {
        let body = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:1c17d23d45cae9129edb6a0f9ce247f5713ae939577e1b8f3e0b1bdf66972b55","size":292},"layers":[]}"#;
        let image = parse_image(body.as_bytes()).unwrap();
        assert!(referrer_descriptor(&image, Digest::from_bytes(body.as_bytes()), 1).is_none());
    }
}
