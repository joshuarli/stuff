#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::http::{is_transient_status, RegistryHttp, MANIFEST_ACCEPT};
use crate::mirror::inventory::{InventoryEntry, PlatformRequirement, ReferrersMode};
use crate::mirror::lock::LockedReferrer;
use rustc_hash::{FxHashMap, FxHashSet};
use stuff_oci::{parse_image, Descriptor, Digest, Image, Platform};
use std::collections::VecDeque;

pub const MAX_MANIFEST_BYTES: u64 = stuff_oci::MAX_MANIFEST_BYTES as u64;
pub const MAX_GRAPH_DEPTH: u32 = 32;
pub const MAX_GRAPH_DESCRIPTORS: u64 = 100_000;

#[derive(Debug)]
pub struct GraphOutcome {
    pub root_media_type: String,
    pub root_digest: Digest,
    pub root_size: u64,
    pub resolved_platforms: Vec<String>,
    pub additional_referrers: Vec<LockedReferrer>,
    pub objects: Vec<crate::mirror::cache::CacheObject>,
    pub manifests: usize,
    pub descriptors: u64,
    pub skipped_referrers: u64,
}

pub fn resolve_entry(
    http: &RegistryHttp,
    base: &str,
    repository: &str,
    entry: &InventoryEntry,
) -> Result<GraphOutcome> {
    let mut walker = Walker::new(http, base, repository, entry);
    let (root, image) = walker.resolve_root()?;
    walker.note_descriptor(&root)?;
    walker.note_object(&root, crate::mirror::cache::ObjectRole::Manifest);
    walker.walk_image(&root, image, None, 0)?;
    if entry.referrers == ReferrersMode::Snapshot {
        walker.observe_referrers()?;
    }
    walker.check_requirements()?;
    let mut resolved_platforms: Vec<String> = walker.platforms.into_iter().collect();
    resolved_platforms.sort();
    let mut additional_referrers: Vec<LockedReferrer> = walker.referrers.into_values().collect();
    additional_referrers.sort_by(|a, b| {
        (a.subject.as_str(), a.digest.as_str()).cmp(&(b.subject.as_str(), b.digest.as_str()))
    });
    let mut objects = walker.objects;
    objects.sort_by(|a, b| a.digest.cmp(&b.digest));
    objects.dedup_by(|a, b| a.digest == b.digest);
    Ok(GraphOutcome {
        root_media_type: root.media_type,
        root_digest: root.digest,
        root_size: root.size,
        resolved_platforms,
        additional_referrers,
        objects,
        manifests: walker.manifests,
        descriptors: walker.descriptor_count,
        skipped_referrers: walker.skipped_referrers,
    })
}

struct Walker<'a> {
    http: &'a RegistryHttp,
    base: &'a str,
    repository: &'a str,
    entry: &'a InventoryEntry,
    manifests: usize,
    descriptor_count: u64,
    platforms: FxHashSet<String>,
    referrer_subjects: Vec<Digest>,
    referrers: FxHashMap<(String, String), LockedReferrer>,
    visited_referrers: FxHashSet<Digest>,
    objects: Vec<crate::mirror::cache::CacheObject>,
    skipped_referrers: u64,
}

impl<'a> Walker<'a> {
    fn new(
        http: &'a RegistryHttp,
        base: &'a str,
        repository: &'a str,
        entry: &'a InventoryEntry,
    ) -> Self {
        Self {
            http,
            base,
            repository,
            entry,
            manifests: 0,
            descriptor_count: 0,
            platforms: FxHashSet::default(),
            referrer_subjects: Vec::new(),
            referrers: FxHashMap::default(),
            visited_referrers: FxHashSet::default(),
            objects: Vec::new(),
            skipped_referrers: 0,
        }
    }

    fn fetch_manifest_bytes(&self, reference: &str) -> Result<Vec<u8>> {
        let url = format!("{}/manifests/{reference}", self.base);
        let response = self
            .http
            .get_authed(&url, MANIFEST_ACCEPT, self.repository, MAX_MANIFEST_BYTES)?;
        match response.status {
            200 => Ok(response.body),
            401 | 403 => Err(Error::Network(format!(
                "registry requires credentials for {url}; anonymous read failed with HTTP {}",
                response.status
            ))),
            404 => Err(Error::Input(format!(
                "{url} was not found; the reference may be wrong or the manifest was deleted"
            ))),
            status if is_transient_status(status) => Err(Error::Network(format!(
                "{url} returned transient HTTP {status}"
            ))),
            status => Err(Error::Network(format!("{url} returned HTTP {status}"))),
        }
    }

    fn parse_manifest(&self, digest: &Digest, bytes: &[u8]) -> Result<Image> {
        parse_image(bytes).map_err(|error| Error::Integrity(format!("manifest {digest}: {error}")))
    }

    fn resolve_root(&mut self) -> Result<(Descriptor, Image)> {
        let reference = self
            .entry
            .source
            .reference
            .as_ref()
            .expect("inventory entries carry a source reference");
        match reference {
            stuff_oci::Reference::Digest(digest) => {
                let bytes = self.fetch_manifest_bytes(digest.as_str())?;
                verify_digest(&bytes, digest)?;
                let image = self.parse_manifest(digest, &bytes)?;
                Ok((root_descriptor(&image, digest.clone(), bytes.len() as u64), image))
            }
            stuff_oci::Reference::Tag(tag) => {
                let bytes = self.fetch_manifest_bytes(tag.as_str())?;
                let digest = Digest::from_bytes(&bytes);
                let verified = self.fetch_manifest_bytes(digest.as_str())?;
                if Digest::from_bytes(&verified) != digest {
                    return Err(Error::Network(format!(
                        "tag {} moved while resolving entry {:?}; retry the resolve", tag.as_str(),
                        self.entry.id
                    )));
                }
                if verified != bytes {
                    return Err(Error::Network(format!(
                        "tag {} moved to different bytes while resolving entry {:?}; retry the resolve", tag.as_str(),
                        self.entry.id
                    )));
                }
                let image = self.parse_manifest(&digest, &verified)?;
                Ok((root_descriptor(&image, digest.clone(), verified.len() as u64), image))
            }
        }
    }

    fn walk_image(
        &mut self,
        descriptor: &Descriptor,
        image: Image,
        platform_hint: Option<&Platform>,
        depth: u32,
    ) -> Result<()> {
        if depth > MAX_GRAPH_DEPTH {
            return Err(Error::Limit(format!(
                "entry {:?} exceeds the graph depth limit of {MAX_GRAPH_DEPTH}",
                self.entry.id
            )));
        }
        self.manifests += 1;
        self.referrer_subjects.push(descriptor.digest.clone());
        match &image {
            Image::Index(index) => {
                for child in &index.manifests {
                    self.note_descriptor(child)?;
                    if is_manifest_like(&child.media_type) {
                        self.note_object(child, crate::mirror::cache::ObjectRole::Manifest);
                    }
                    let hint = child.platform.as_ref();
                    if let Some(platform) = hint
                        && !is_unknown_platform(&platform.os, &platform.architecture)
                    {
                        self.platforms.insert(platform_string(platform));
                    }
                    if !is_manifest_like(&child.media_type) {
                        continue;
                    }
                    let (_, child_image) = self.fetch_child(child, depth + 1)?;
                    self.walk_image(child, child_image, hint, depth + 1)?;
                }
            }
            Image::Manifest(manifest) => {
                match platform_hint {
                    Some(platform) if is_unknown_platform(&platform.os, &platform.architecture) => {}
                    Some(platform) => {
                        self.platforms.insert(platform_string(platform));
                    }
                    None => {
                        let config = self.fetch_config(&manifest.config)?;
                        let platform = parse_config_platform(&config).ok_or_else(|| {
                            Error::Integrity(format!(
                                "config {} has no os/architecture",
                                manifest.config.digest
                            ))
                        })?;
                        self.platforms.insert(platform);
                    }
                }
                self.note_object(&manifest.config, crate::mirror::cache::ObjectRole::Config);
                for blob in manifest.layers.iter() {
                    self.note_object(blob, crate::mirror::cache::ObjectRole::Layer);
                }
                for blob in std::iter::once(&manifest.config).chain(manifest.layers.iter()) {
                    self.note_descriptor(blob)?;
                }
            }
        }
        Ok(())
    }

    fn fetch_child(&mut self, descriptor: &Descriptor, depth: u32) -> Result<(Vec<u8>, Image)> {
        if depth > MAX_GRAPH_DEPTH {
            return Err(Error::Limit(format!(
                "entry {:?} exceeds the graph depth limit of {MAX_GRAPH_DEPTH}",
                self.entry.id
            )));
        }
        let bytes = self.fetch_manifest_bytes(descriptor.digest.as_str())?;
        if bytes.len() as u64 != descriptor.size {
            return Err(Error::Integrity(format!(
                "manifest {} size is {}, expected {}",
                descriptor.digest,
                bytes.len(),
                descriptor.size
            )));
        }
        verify_digest(&bytes, &descriptor.digest)?;
        let image = self.parse_manifest(&descriptor.digest, &bytes)?;
        Ok((bytes, image))
    }

    fn fetch_config(&self, descriptor: &Descriptor) -> Result<Vec<u8>> {
        let url = format!("{}/blobs/{}", self.base, descriptor.digest);
        let response = self
            .http
            .get_authed(&url, "application/octet-stream", self.repository, MAX_MANIFEST_BYTES)?;
        if response.status != 200 {
            return Err(Error::Network(format!(
                "could not fetch config {url}: HTTP {}",
                response.status
            )));
        }
        if response.body.len() as u64 != descriptor.size {
            return Err(Error::Integrity(format!(
                "config {} size is {}, expected {}",
                descriptor.digest,
                response.body.len(),
                descriptor.size
            )));
        }
        verify_digest(&response.body, &descriptor.digest)?;
        Ok(response.body)
    }

    fn note_object(&mut self, descriptor: &Descriptor, role: crate::mirror::cache::ObjectRole) {
        self.objects.push(crate::mirror::cache::CacheObject {
            digest: descriptor.digest.clone(),
            size: descriptor.size,
            media_type: descriptor.media_type.clone(),
            role,
        });
    }

    fn note_descriptor(&mut self, _descriptor: &Descriptor) -> Result<()> {
        self.descriptor_count += 1;
        if self.descriptor_count > MAX_GRAPH_DESCRIPTORS {
            return Err(Error::Limit(format!(
                "entry {:?} exceeds the {MAX_GRAPH_DESCRIPTORS}-descriptor limit",
                self.entry.id
            )));
        }
        Ok(())
    }

    fn observe_referrers(&mut self) -> Result<()> {
        let initial: Vec<Digest> = self.referrer_subjects.clone();
        let mut pending: VecDeque<(Digest, u32)> =
            initial.into_iter().map(|digest| (digest, 0)).collect();
        while let Some((subject, depth)) = pending.pop_front() {
            if !self.visited_referrers.insert(subject.clone()) {
                continue;
            }
            if depth > MAX_GRAPH_DEPTH {
                return Err(Error::Limit(format!(
                    "entry {:?} exceeds the referrer recursion depth of {MAX_GRAPH_DEPTH}",
                    self.entry.id
                )));
            }
            for referrer in self.fetch_referrers(&subject)? {
                self.note_descriptor(&referrer)?;
                if referrer.digest == subject {
                    continue;
                }
                let key = (
                    subject.as_str().to_string(),
                    referrer.digest.as_str().to_string(),
                );
                if self.referrers.contains_key(&key) {
                    continue;
                }
                self.note_object(&referrer, crate::mirror::cache::ObjectRole::Manifest);
                self.referrers.insert(
                    key,
                    LockedReferrer {
                        subject: subject.clone(),
                        media_type: referrer.media_type.clone(),
                        digest: referrer.digest.clone(),
                        size: referrer.size,
                        artifact_type: referrer.artifact_type.clone(),
                    },
                );
                pending.push_back((referrer.digest.clone(), depth + 1));
            }
        }
        Ok(())
    }

    fn fetch_referrers(&mut self, subject: &Digest) -> Result<Vec<Descriptor>> {
        let url = format!("{}/referrers/{subject}", self.base);
        let response = self.http.get_authed(
            &url,
            "application/vnd.oci.image.index.v1+json",
            self.repository,
            MAX_MANIFEST_BYTES,
        )?;
        if response.status != 200 {
            return Err(Error::Network(format!(
                "referrer discovery failed for {subject}: {url} returned HTTP {}",
                response.status
            )));
        }
        let image = parse_image(&response.body).map_err(|error| {
            Error::Integrity(format!("referrer index for {subject}: {error}"))
        })?;
        let Image::Index(index) = image else {
            return Err(Error::Integrity(format!(
                "referrer response for {subject} is not an index"
            )));
        };
        let mut descriptors = Vec::with_capacity(index.manifests.len());
        for descriptor in index.manifests {
            if !is_manifest_like(&descriptor.media_type) {
                self.skipped_referrers += 1;
                continue;
            }
            if descriptor.size > MAX_MANIFEST_BYTES {
                return Err(Error::Limit(format!(
                    "referrer {} for {subject} exceeds the {MAX_MANIFEST_BYTES}-byte manifest limit",
                    descriptor.digest
                )));
            }
            descriptors.push(descriptor);
        }
        Ok(descriptors)
    }

    fn check_requirements(&self) -> Result<()> {
        for requirement in &self.entry.platforms {
            let satisfied = self
                .platforms
                .iter()
                .any(|candidate| requirement_matches(requirement, candidate));
            if !satisfied {
                return Err(Error::Integrity(format!(
                    "entry {:?}: required platform {} is not in the resolved graph",
                    self.entry.id,
                    requirement.render()
                )));
            }
        }
        Ok(())
    }
}

fn root_descriptor(image: &Image, digest: Digest, size: u64) -> Descriptor {
    Descriptor {
        media_type: image.media_type().to_string(),
        digest,
        size,
        artifact_type: image.artifact_type().map(str::to_string),
        annotations: image.annotations().clone(),
        platform: None,
        urls: Vec::new(),
    }
}

fn is_manifest_like(media_type: &str) -> bool {
    stuff_oci::is_manifest_or_index(media_type)
}

fn is_unknown_platform(os: &str, architecture: &str) -> bool {
    os == "unknown" && architecture == "unknown"
}

fn platform_string(platform: &Platform) -> String {
    PlatformRequirement {
        os: platform.os.clone(),
        architecture: platform.architecture.clone(),
        variant: platform.variant.clone(),
    }
    .render()
}

fn requirement_matches(requirement: &PlatformRequirement, candidate: &str) -> bool {
    match PlatformRequirement::parse(candidate, "resolved platform") {
        Ok(parsed) => {
            parsed.os == requirement.os
                && parsed.architecture == requirement.architecture
                && match &requirement.variant {
                    Some(variant) => parsed.variant.as_deref() == Some(variant.as_str()),
                    None => true,
                }
        }
        Err(_) => false,
    }
}

fn verify_digest(bytes: &[u8], digest: &Digest) -> Result<()> {
    let actual = Digest::from_bytes(bytes);
    if actual != *digest {
        return Err(Error::Integrity(format!("manifest {digest} hashes to {actual}")));
    }
    Ok(())
}

fn parse_config_platform(bytes: &[u8]) -> Option<String> {
    let value: deser_value::Value = deser_json::from_slice(bytes).ok()?;
    let map = value.as_map()?;
    let os = map.get("os")?.as_str()?;
    let architecture = map.get("architecture")?.as_str()?;
    Some(match map.get("variant").and_then(|value| value.as_str()) {
        Some(variant) => format!("{os}/{architecture}/{variant}"),
        None => format!("{os}/{architecture}"),
    })
}
