#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use stuff_oci::{Digest, Reference, RepositoryName, Tag};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryReference {
    pub registry: String,
    pub repository: String,
    pub reference: Option<Reference>,
}

impl RegistryReference {
    pub fn parse(text: &str, context: &str) -> Result<Self> {
        let (name, reference) = split_reference(text, context)?;
        let (registry, repository) = split_registry(name, context)?;
        RepositoryName::parse(&repository)
            .map_err(|error| Error::Input(format!("{context}: {error}")))?;
        Ok(Self {
            registry,
            repository,
            reference,
        })
    }

    pub fn repository_id(&self) -> String {
        format!("{}/{}", self.registry, self.repository)
    }

    pub fn repository_name(&self) -> RepositoryName {
        RepositoryName::parse(&self.repository).expect("validated at parse")
    }
}

fn split_reference<'a>(text: &'a str, context: &str) -> Result<(&'a str, Option<Reference>)> {
    if let Some((name, digest_text)) = text.split_once('@') {
        let digest = Digest::parse(digest_text)
            .map_err(|error| Error::Input(format!("{context}: {error}")))?;
        if name.is_empty() {
            return Err(Error::Input(format!("{context}: reference has no repository")));
        }
        return Ok((name, Some(Reference::Digest(digest))));
    }
    let colon = text.rfind(':');
    let slash = text.rfind('/');
    match (colon, slash) {
        (Some(colon), Some(slash)) if colon > slash => {
            let tag = Tag::parse(&text[colon + 1..])
                .map_err(|error| Error::Input(format!("{context}: {error}")))?;
            Ok((&text[..colon], Some(Reference::Tag(tag))))
        }
        (Some(colon), None) => {
            let tag = Tag::parse(&text[colon + 1..])
                .map_err(|error| Error::Input(format!("{context}: {error}")))?;
            Ok((&text[..colon], Some(Reference::Tag(tag))))
        }
        _ => Ok((text, None)),
    }
}

pub fn normalize_registry(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    match lower.as_str() {
        "docker.io" | "index.docker.io" | "registry-1.docker.io" => "docker.io".to_string(),
        _ => lower,
    }
}

fn split_registry<'a>(name: &'a str, context: &str) -> Result<(String, String)> {
    let mut parts = name.split('/');
    let first = parts.next().unwrap_or_default();
    let looks_like_host = first.contains('.')
        || first.contains(':')
        || first == "localhost"
        || first.bytes().all(|b| b.is_ascii_digit());
    if looks_like_host {
        let registry = normalize_registry(first);
        validate_registry_host(&registry, context)?;
        let repository = parts.collect::<Vec<_>>().join("/");
        if repository.is_empty() {
            return Err(Error::Input(format!("{context}: reference has no repository path")));
        }
        return Ok((registry, repository));
    }
    let repository = if name.contains('/') {
        name.to_string()
    } else {
        format!("library/{name}")
    };
    Ok(("docker.io".to_string(), repository))
}

fn validate_registry_host(host: &str, context: &str) -> Result<()> {
    let (hostname, port) = match host.rsplit_once(':') {
        Some((hostname, port_text)) => {
            let port: u16 = port_text
                .parse()
                .map_err(|_| Error::Input(format!("{context}: registry port {port_text} is invalid")))?;
            if port == 0 {
                return Err(Error::Input(format!("{context}: registry port must not be zero")));
            }
            (hostname, Some(port))
        }
        None => (host, None),
    };
    if hostname.is_empty() || hostname.len() > 253 {
        return Err(Error::Input(format!("{context}: registry host is empty or too long")));
    }
    for label in hostname.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(Error::Input(format!("{context}: registry host label {label:?} is invalid")));
        }
        let bytes = label.as_bytes();
        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err(Error::Input(format!("{context}: registry host label {label:?} is invalid")));
        }
        if !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-') {
            return Err(Error::Input(format!("{context}: registry host label {label:?} is invalid")));
        }
    }
    let _ = port;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_docker_hub_shorthand() {
        let reference = RegistryReference::parse("alpine:3.20", "source").unwrap();
        assert_eq!(reference.repository_id(), "docker.io/library/alpine");
        let reference = RegistryReference::parse("foo/bar", "source").unwrap();
        assert_eq!(reference.repository_id(), "docker.io/foo/bar");
        for host in ["docker.io", "index.docker.io", "registry-1.docker.io"] {
            let reference = RegistryReference::parse(&format!("{host}/library/alpine"), "source").unwrap();
            assert_eq!(reference.repository_id(), "docker.io/library/alpine");
        }
    }

    #[test]
    fn preserves_registry_and_path_components() {
        let reference = RegistryReference::parse("quay.io/pypa/manylinux_2_28_x86_64", "destination").unwrap();
        assert_eq!(reference.repository_id(), "quay.io/pypa/manylinux_2_28_x86_64");
        let reference = RegistryReference::parse("example.com:5000/Team/Alpha", "destination").unwrap_err();
        assert!(format!("{reference}").contains("repository"));
        let reference = RegistryReference::parse("example.com:5000/team/alpha", "destination").unwrap();
        assert_eq!(reference.repository_id(), "example.com:5000/team/alpha");
    }

    #[test]
    fn splits_tags_and_digests() {
        let reference = RegistryReference::parse("quay.io/a/b:latest", "source").unwrap();
        assert!(matches!(reference.reference, Some(Reference::Tag(ref tag)) if tag.as_str() == "latest"));
        let reference = RegistryReference::parse("quay.io/a/b@sha256:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4", "source").unwrap();
        assert!(matches!(reference.reference, Some(Reference::Digest(_))));
        assert!(RegistryReference::parse("quay.io/a/b:bad tag", "source").is_err());
        assert!(RegistryReference::parse("quay.io/a/b@sha512:576055fc159808ebb0b91a55303a512f5730a93e036977f652e59adc769debf4", "source").is_err());
    }
}
