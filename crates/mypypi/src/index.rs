#![forbid(unsafe_code)]

use crate::error::{Error, Kind, Result, Stage};
use crate::models::{Target, ValidatedArtifact, canonicalize_name};
use crate::platforms::{current_platform, target_python_version};
use sha2::{Digest, Sha256};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone)]
pub struct IndexArtifact {
    pub wheel_filename: String,
    pub package_name: String,
    pub package_version: String,
    pub sha256: String,
    pub requires_python: Option<String>,
    pub metadata: Vec<u8>,
    pub metadata_sha256: String,
    pub yanked_reason: Option<String>,
    pub targets: Vec<Target>,
    pub first_party: bool,
}

impl IndexArtifact {
    pub fn targets_sorted(&self) -> Vec<&Target> {
        let mut v: Vec<&Target> = self.targets.iter().collect();
        v.sort_by(|a, b| a.platform.cmp(&b.platform));
        v
    }
}

fn publication_error(detail: impl Into<String>, artifact: Option<&ValidatedArtifact>) -> Error {
    let mut e = if artifact.is_some() {
        Error::new(Kind::Integrity, detail, Stage::Publication)
    } else {
        Error::new(Kind::InvalidInput, detail, Stage::Publication)
    };
    if let Some(a) = artifact {
        e = e.with_package(&a.wheel.package);
        if let Some(t) = a.targets.iter().min_by(|x, y| x.platform.cmp(&y.platform)) {
            e = e.with_target(&t.python_version.to_string(), &t.platform);
        }
    }
    e
}

fn read_member_bytes(archive: &crate::wheel_zip::Archive, name: &str) -> Result<Vec<u8>> {
    archive.read(name).map_err(|e| publication_error(format!("could not read wheel metadata: {e}"), None))
}

fn parse_headers(bytes: &[u8]) -> FxHashMap<String, Vec<String>> {
    let text = String::from_utf8_lossy(bytes);
    let mut map: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut current: Option<String> = None;
    for line in text.split_inclusive('\n') {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if (line.starts_with(' ') || line.starts_with('\t')) && current.is_some() {
            let key = current.clone().unwrap();
            if let Some(vals) = map.get_mut(&key)
                && let Some(last) = vals.last_mut()
            {
                last.push(' ');
                last.push_str(line.trim());
            }
            continue;
        }
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().to_lowercase();
            let val = line[colon + 1..].trim().to_string();
            map.entry(key.clone()).or_default().push(val);
            current = Some(key);
        }
    }
    map
}

pub fn wheel_metadata(artifact: &ValidatedArtifact) -> Result<(Option<String>, Vec<u8>)> {
    let fail = |d: String| publication_error(d, Some(artifact));
    let wheel = crate::wheel_zip::Archive::open(&artifact.path).map_err(|_| fail("could not read wheel metadata".to_string()))?;
    let members: Vec<String> = wheel.names().map(str::to_string).collect();
    let res = {
        let mut seen = rustc_hash::FxHashSet::default();
        members.iter().any(|n| !seen.insert(n.clone()))
    }; if res {
        return Err(fail("wheel contains duplicate archive paths".to_string()));
    }
    for member in &members {
        let parts: Vec<&str> = member.strip_suffix('/').unwrap_or(member).split('/').collect();
        let bad = member.is_empty()
            || member.starts_with('/')
            || parts.is_empty()
            || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..")
            || parts[0].contains(':')
            || member.contains('\\')
            || member.contains('\0');
        if bad {
            return Err(fail("wheel contains an archive path that escapes its root".to_string()));
        }
        let file_type = wheel.mode(member).map_err(|_| fail("could not read wheel metadata".to_string()))? & 0o170000;
        if file_type != 0 && file_type != 0o100000 && file_type != 0o040000 {
            return Err(fail("wheel contains a non-regular archive member".to_string()));
        }
    }
    let metadata_members: Vec<String> =
        members.iter().filter(|m| m.ends_with(".dist-info/METADATA")).cloned().collect();
    if metadata_members.len() != 1 {
        return Err(fail("wheel must contain exactly one dist-info/METADATA".to_string()));
    }
    let metadata = read_member_bytes(&wheel, &metadata_members[0]).map_err(|_| fail("could not read wheel metadata".to_string()))?;
    let parsed = parse_headers(&metadata);
    let names = parsed.get("name").cloned().unwrap_or_default();
    let versions = parsed.get("version").cloned().unwrap_or_default();
    let rp_values = parsed.get("requires-python").cloned().unwrap_or_default();
    let requires_python = rp_values.first().cloned();
    if names.len() != 1 || canonicalize_name(&names[0]) != artifact.wheel.package.name {
        return Err(fail("wheel metadata name does not match its filename".to_string()));
    }
    let version_matches = versions.len() == 1 && versions[0] == artifact.wheel.package.version.to_string();
    if !version_matches {
        // distinguish invalid version vs mismatch like Python does
        let parses = versions.len() == 1 && versions[0].parse::<pep440_rs::Version>().is_ok();
        if !parses {
            return Err(fail("wheel metadata contains an invalid version".to_string()));
        }
        return Err(fail("wheel metadata version does not match its filename".to_string()));
    }
    if rp_values.len() > 1 {
        return Err(fail("wheel metadata has duplicate Requires-Python fields".to_string()));
    }
    if let Some(ref rp) = requires_python {
        let spec: pep440_rs::VersionSpecifiers = rp
            .parse()
            .map_err(|_| fail("wheel metadata has invalid Requires-Python".to_string()))?;
        let mut excluded: Vec<String> = Vec::new();
        for target in &artifact.targets {
            if !supports_target_python(&spec, target) {
                excluded.push(target.python_version.to_string());
            }
        }
        excluded.sort();
        excluded.dedup();
        if !excluded.is_empty() {
            return Err(fail(format!("wheel Requires-Python metadata excludes CPython {}", excluded.join(", "))));
        }
    }
    Ok((requires_python, metadata))
}

pub fn supports_target_python(spec: &pep440_rs::VersionSpecifiers, target: &Target) -> bool {
    if current_platform().as_deref() == Some(target.platform.as_str()) {
        return spec.contains(&target_python_version(target));
    }
    let release = target.python_version.release();
    if release.len() < 2 {
        return false;
    }
    let (major, minor) = (release[0], release[1]);
    let parse = |s: String| s.parse::<pep440_rs::Version>().ok();
    let mut candidates = vec![
        parse(format!("{major}.{minor}.0")),
        parse(format!("{major}.{minor}.1")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    for requirement in spec.iter() {
        let text = requirement.version().to_string();
        let base = text.trim_end_matches(".*");
        let Ok(boundary) = base.parse::<pep440_rs::Version>() else {
            continue;
        };
        let rel = boundary.release();
        if rel.len() < 2 || rel[0] != major || rel[1] != minor {
            continue;
        }
        let patch = if rel.len() > 2 { rel[2] as i64 } else { 0 };
        for value in [patch - 1, patch, patch + 1] {
            if value >= 0
                && let Some(v) = parse(format!("{major}.{minor}.{value}"))
            {
                candidates.push(v);
            }
        }
    }
    candidates.iter().any(|c| spec.contains(c))
}

pub fn prepare_index_artifacts(
    artifacts: &[ValidatedArtifact],
    yanked: &FxHashMap<String, String>,
) -> Result<Vec<IndexArtifact>> {
    let mut yanked_entries: Vec<_> = yanked.iter().collect();
    yanked_entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
    for (filename, reason) in yanked_entries {
        if filename.is_empty() {
            return Err(publication_error("yanked metadata filenames must be non-empty strings", None));
        }
        if reason.trim().is_empty() {
            return Err(publication_error("yanked reason must be a non-empty string", None));
        }
    }
    let known: rustc_hash::FxHashSet<&str> =
        artifacts.iter().map(|a| a.wheel.filename.as_str()).collect();
    let unknown: Vec<&String> = yanked.keys().filter(|k| !known.contains(k.as_str())).collect();
    if !unknown.is_empty() {
        let mut sorted = unknown.clone();
        sorted.sort();
        let list: Vec<&str> = sorted.iter().map(|s| s.as_str()).collect();
        return Err(publication_error(format!("yanked metadata names unknown wheels: {}", list.join(", ")), None));
    }
    let mut prepared = Vec::new();
    let mut seen = rustc_hash::FxHashSet::default();
    for artifact in artifacts {
        if !seen.insert(artifact.wheel.filename.clone()) {
            return Err(publication_error(
                format!("duplicate wheel filename: {}", artifact.wheel.filename),
                Some(artifact),
            ));
        }
        let reason = yanked.get(&artifact.wheel.filename).cloned();
        let bytes = std::fs::read(&artifact.path)
            .map_err(|_| publication_error("could not read validated wheel", Some(artifact)))?;
        let mut h = Sha256::new();
        h.update(&bytes);
        let actual = crate::lower_hex(h.finalize());
        if actual != artifact.sha256 {
            return Err(publication_error("wheel bytes changed after validation", Some(artifact)));
        }
        let (requires_python, metadata) = wheel_metadata(artifact)?;
        let mut mh = Sha256::new();
        mh.update(&metadata);
        let metadata_sha256 = crate::lower_hex(mh.finalize());
        prepared.push(IndexArtifact {
            wheel_filename: artifact.wheel.filename.clone(),
            package_name: artifact.wheel.package.name.clone(),
            package_version: artifact.wheel.package.version.to_string(),
            sha256: artifact.sha256.clone(),
            requires_python,
            metadata,
            metadata_sha256,
            yanked_reason: reason,
            targets: artifact.targets.clone(),
            first_party: artifact.first_party,
        });
    }
    prepared.sort_by(|a, b| {
        (a.package_name.clone(), a.wheel_filename.clone())
            .cmp(&(b.package_name.clone(), b.wheel_filename.clone()))
    });
    Ok(prepared)
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

fn quote_path_segment(text: &str) -> String {
    // urllib.parse.quote(safe="/-._~"): keep unreserved + / - . _ ~
    // percent-encode UTF-8 bytes otherwise (uppercase hex)
    let mut out = String::new();
    for b in text.as_bytes() {
        if matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn project_name(artifact: &IndexArtifact) -> String {
    project_name_for(&artifact.package_name)
}

pub fn project_name_for(package_name: &str) -> String {
    // re.sub(r"[-_.]+", "-", canonicalize_name(name))
    let canonical = canonicalize_name(package_name);
    let mut out = String::new();
    let mut in_sep = false;
    for c in canonical.chars() {
        if matches!(c, '-' | '_' | '.') {
            if !in_sep {
                out.push('-');
                in_sep = true;
            }
        } else {
            out.push(c);
            in_sep = false;
        }
    }
    out
}

pub fn render_root_page(artifacts: &[IndexArtifact]) -> Vec<u8> {
    let mut names: Vec<String> = artifacts.iter().map(project_name).collect();
    names.sort();
    names.dedup();
    let mut out = String::from("<!DOCTYPE html>\n<html>\n  <body>\n");
    for name in &names {
        let href = quote_path_segment(&format!("{name}/"));
        out.push_str(&format!("    <a href=\"{}\">{}</a>\n", escape_html(&href), escape_html(name)));
    }
    out.push_str("  </body>\n</html>\n");
    out.into_bytes()
}

pub fn render_project_page(artifacts: &[IndexArtifact]) -> Vec<u8> {
    let mut sorted = artifacts.to_vec();
    sorted.sort_by(|a, b| a.wheel_filename.cmp(&b.wheel_filename));
    let mut out = String::from("<!DOCTYPE html>\n<html>\n  <body>\n");
    for item in &sorted {
        let filename = &item.wheel_filename;
        let mut href = quote_path_segment(&format!("../../wheels/{filename}"));
        href.push_str(&format!("#sha256={}", item.sha256));
        let mut attributes = vec![format!("href=\"{}\"", escape_html(&href))];
        if let Some(ref rp) = item.requires_python {
            attributes.push(format!("data-requires-python=\"{}\"", escape_html(rp)));
        }
        if let Some(ref y) = item.yanked_reason {
            attributes.push(format!("data-yanked=\"{}\"", escape_html(y)));
        }
        attributes.push(format!("data-dist-info-metadata=\"sha256={}\"", item.metadata_sha256));
        attributes.push(format!("data-core-metadata=\"sha256={}\"", item.metadata_sha256));
        out.push_str(&format!("    <a {}>{}</a>\n", attributes.join(" "), escape_html(filename)));
    }
    out.push_str("  </body>\n</html>\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WheelIdentity;
    use std::path::Path;

    fn make_artifact(dir: &Path, filename: &str, metadata: &[u8]) -> ValidatedArtifact {
        let identity = WheelIdentity::parse(filename).unwrap();
        let path = dir.join(filename);
        let dist_info = format!(
            "{}-{}.dist-info/METADATA",
            identity.package.name.replace('-', "_"),
            identity.package.version
        );
        crate::wheel_zip::testing::write_files(&path, &[(dist_info.as_str(), metadata)]);
        let bytes = std::fs::read(&path).unwrap();
        let mut h = Sha256::new();
        h.update(&bytes);
        let sha256 = crate::lower_hex(h.finalize());
        ValidatedArtifact::new(
            identity,
            path,
            sha256,
            vec![Target::new("3.15", "macos-arm64").unwrap()],
        )
        .unwrap()
    }

    #[test]
    fn project_page_links_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let one = make_artifact(
            dir.path(),
            "sample_pkg-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.0\nRequires-Python: >=3.15,<3.16\n\n",
        );
        let two = make_artifact(
            dir.path(),
            "sample_pkg-1.1-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.1\nRequires-Python: >=3.15\n\n",
        );
        let mut yanked = FxHashMap::default();
        yanked.insert(one.wheel.filename.clone(), "maintainer <team@example.test> & \"held\"".to_string());
        let prepared = prepare_index_artifacts(&[two.clone(), one.clone()], &yanked).unwrap();
        let first = render_project_page(&prepared);
        let reversed: Vec<IndexArtifact> = prepared.iter().rev().cloned().collect();
        assert_eq!(first, render_project_page(&reversed));
        let text = String::from_utf8(first).unwrap();
        assert!(text.find("sample_pkg-1.0").unwrap() < text.find("sample_pkg-1.1").unwrap());
        assert!(text.contains("../../wheels/sample_pkg-1.0-py3-none-any.whl#sha256="));
        assert!(!text.contains("../../../../wheels/"));
        assert!(text.contains("data-requires-python=\"&gt;=3.15,&lt;3.16\""));
        assert!(text.contains("data-yanked=\"maintainer &lt;team@example.test&gt; &amp; &quot;held&quot;\""));
        let mut h = Sha256::new();
        h.update(b"Metadata-Version: 2.4\nName: sample-pkg\nVersion: 1.0\nRequires-Python: >=3.15,<3.16\n\n");
        let mh = crate::lower_hex(h.finalize());
        assert!(text.contains(&format!("data-dist-info-metadata=\"sha256={mh}\"")));
        assert!(text.contains(&format!("data-core-metadata=\"sha256={mh}\"")));
    }

    #[test]
    fn root_page_lists_sorted_projects_once() {
        let dir = tempfile::tempdir().unwrap();
        let a = make_artifact(
            dir.path(),
            "zeta-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: zeta\nVersion: 1.0\n\n",
        );
        let b = make_artifact(
            dir.path(),
            "alpha-1.0-py3-none-any.whl",
            b"Metadata-Version: 2.4\nName: alpha\nVersion: 1.0\n\n",
        );
        let prepared = prepare_index_artifacts(&[a, b], &FxHashMap::default()).unwrap();
        let text = String::from_utf8(render_root_page(&prepared)).unwrap();
        assert!(text.find("alpha").unwrap() < text.find("zeta").unwrap());
    }
}
