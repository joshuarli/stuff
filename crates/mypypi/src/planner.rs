#![forbid(unsafe_code)]

use crate::catalog::Catalog;
use crate::error::{Error, Kind, Result, Stage};
use crate::models::{PackageVersion, Target, WheelIdentity};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub wheel: WheelIdentity,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamWheel {
    pub wheel: WheelIdentity,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamSdist {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamArtifact {
    Wheel(UpstreamWheel),
    Sdist(UpstreamSdist),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOutcome {
    AlreadyPublished {
        package: PackageVersion,
        target: Target,
        wheel: WheelIdentity,
    },
    DownloadWheel {
        package: PackageVersion,
        target: Target,
        wheel: WheelIdentity,
        url: String,
        sha256: String,
    },
    BuildSdist {
        package: PackageVersion,
        target: Target,
        url: String,
        sha256: String,
    },
    ExcludedTarget {
        package: PackageVersion,
        target: Target,
        reason: String,
    },
    PendingValidation {
        package: PackageVersion,
        target: Target,
        wheel: WheelIdentity,
    },
    ArtifactConflict {
        package: PackageVersion,
        target: Target,
        filename: String,
        existing_sha256: String,
        candidate_sha256: String,
    },
}

fn missing(package: &PackageVersion, target: &Target) -> Error {
    Error::new(
        Kind::MissingArtifact,
        format!(
            "no compatible upstream wheel or source archive for {} on {}",
            package, target.platform
        ),
        Stage::Planning,
    )
}

pub fn validate_catalog_targets(
    catalog: &Catalog,
    configured: &[Target],
) -> Result<()> {
    let configured_platforms: FxHashSet<&str> =
        configured.iter().map(|t| t.platform.as_str()).collect();
    for e in &catalog.entries {
        for (platform, _) in &e.excluded_platforms {
            if !configured_platforms.contains(platform.as_str()) {
                return Err(Error::new(
                    Kind::Configuration,
                    format!(
                        "unsupported excluded platform for {}: {platform}",
                        e.package
                    ),
                    Stage::Configuration,
                ));
            }
        }
        if let Some(platforms) = e.platforms.as_ref() {
            for platform in platforms {
                if !configured_platforms.contains(platform.as_str()) {
                    return Err(Error::new(
                        Kind::Configuration,
                        format!(
                            "unsupported target platform for {}: {platform}",
                            e.package
                        ),
                        Stage::Configuration,
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn plan(
    catalog: &Catalog,
    targets: &[Target],
    published: &FxHashMap<(PackageVersion, Target), ArtifactRecord>,
    upstream: &FxHashMap<(PackageVersion, Target), Vec<UpstreamArtifact>>,
    pending: &FxHashMap<(PackageVersion, Target), ArtifactRecord>,
    configured_targets: Option<&[Target]>,
) -> Result<Vec<PlanOutcome>> {
    validate_catalog_targets(catalog, configured_targets.unwrap_or(targets))?;
    let mut local_filenames: FxHashMap<String, &ArtifactRecord> = FxHashMap::default();
    let mut published_records: Vec<_> = published.iter().collect();
    published_records.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let mut pending_records: Vec<_> = pending.iter().collect();
    pending_records.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (_, record) in published_records.into_iter().chain(pending_records) {
        local_filenames.insert(record.wheel.filename.clone(), record);
    }
    let mut outcomes = Vec::new();
    for e in &catalog.entries {
        let exclusions: FxHashMap<&str, &str> = e
            .excluded_platforms
            .iter()
            .map(|(p, r)| (p.as_str(), r.as_str()))
            .collect();
        for target in targets {
            if let Some(platforms) = e.platforms.as_ref()
                && !platforms.iter().any(|p| p == &target.platform) {
                    continue;
                }
            let key = (e.package.clone(), target.clone());
            if let Some(reason) = exclusions.get(target.platform.as_str()) {
                outcomes.push(PlanOutcome::ExcludedTarget {
                    package: e.package.clone(),
                    target: target.clone(),
                    reason: (*reason).to_string(),
                });
                continue;
            }
            if let Some(current) = published.get(&key) {
                outcomes.push(PlanOutcome::AlreadyPublished {
                    package: e.package.clone(),
                    target: target.clone(),
                    wheel: current.wheel.clone(),
                });
                continue;
            }
            if let Some(p) = pending.get(&key) {
                if let Some(existing) = local_filenames.get(&p.wheel.filename)
                    && existing.sha256 != p.sha256 {
                        outcomes.push(PlanOutcome::ArtifactConflict {
                            package: e.package.clone(),
                            target: target.clone(),
                            filename: p.wheel.filename.clone(),
                            existing_sha256: existing.sha256.clone(),
                            candidate_sha256: p.sha256.clone(),
                        });
                    } else {
                        outcomes.push(PlanOutcome::PendingValidation {
                            package: e.package.clone(),
                            target: target.clone(),
                            wheel: p.wheel.clone(),
                        });
                    }
                continue;
            }
            let candidates = upstream.get(&key);
            let wheels: Vec<&UpstreamWheel> = candidates
                .map(|v| {
                    v.iter()
                        .filter_map(|a| match a {
                            UpstreamArtifact::Wheel(w) => Some(w),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(candidate) = wheels.first() {
                if let Some(existing) = local_filenames.get(&candidate.wheel.filename)
                    && existing.sha256 != candidate.sha256 {
                        outcomes.push(PlanOutcome::ArtifactConflict {
                            package: e.package.clone(),
                            target: target.clone(),
                            filename: candidate.wheel.filename.clone(),
                            existing_sha256: existing.sha256.clone(),
                            candidate_sha256: candidate.sha256.clone(),
                        });
                    } else {
                        outcomes.push(PlanOutcome::DownloadWheel {
                            package: e.package.clone(),
                            target: target.clone(),
                            wheel: candidate.wheel.clone(),
                            url: candidate.url.clone(),
                            sha256: candidate.sha256.clone(),
                        });
                    }
                continue;
            }
            let sdists: Vec<&UpstreamSdist> = candidates
                .map(|v| {
                    v.iter()
                        .filter_map(|a| match a {
                            UpstreamArtifact::Sdist(s) => Some(s),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(candidate) = sdists.first() {
                outcomes.push(PlanOutcome::BuildSdist {
                    package: e.package.clone(),
                    target: target.clone(),
                    url: candidate.url.clone(),
                    sha256: candidate.sha256.clone(),
                });
                continue;
            }
            return Err(missing(&e.package, target));
        }
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::parse_catalog;

    fn target(platform: &str) -> Target {
        Target::new("3.15.0", platform).unwrap()
    }

    #[test]
    fn plans_download_then_published() {
        let catalog =
            parse_catalog("[[packages]]\nname = \"foo\"\nversion = \"1.0\"\n").unwrap();
        let targets = vec![target("linux-x86_64")];
        let package = catalog.entries[0].package.clone();
        let wheel = WheelIdentity::parse("foo-1.0-py3-none-any.whl").unwrap();
        let key = (package.clone(), targets[0].clone());
        let upstream: FxHashMap<(PackageVersion, Target), Vec<UpstreamArtifact>> = [(
            key.clone(),
            vec![UpstreamArtifact::Wheel(UpstreamWheel {
                wheel: wheel.clone(),
                url: "https://example.test/foo.whl".to_string(),
                sha256: "0".repeat(64),
            })],
        )]
        .into_iter()
        .collect();
        let outcomes = plan(
            &catalog,
            &targets,
            &FxHashMap::default(),
            &upstream,
            &FxHashMap::default(),
            None,
        )
        .unwrap();
        assert!(matches!(outcomes[0], PlanOutcome::DownloadWheel { .. }));
        let published: FxHashMap<(PackageVersion, Target), ArtifactRecord> = [(
            key,
            ArtifactRecord {
                wheel,
                sha256: "0".repeat(64),
            },
        )]
        .into_iter()
        .collect();
        let outcomes = plan(
            &catalog,
            &targets,
            &published,
            &FxHashMap::default(),
            &FxHashMap::default(),
            None,
        )
        .unwrap();
        assert!(matches!(
            outcomes[0],
            PlanOutcome::AlreadyPublished { .. }
        ));
    }
}
