use crate::error::{Error, Result};
use crate::model::{CrateName, IndexDep, IndexEntry, PublishBody, Version};
use std::collections::BTreeMap;

pub fn shard_for_name(name_lower: &str) -> String {
    let bytes = name_lower.as_bytes();
    if bytes.is_empty() {
        return "1".to_string();
    }
    if name_lower.len() == 1 {
        return format!("1/{}", name_lower);
    }
    if name_lower.len() == 2 {
        return format!("2/{}", name_lower);
    }
    if name_lower.len() == 3 {
        return format!("3/{}/{}", &name_lower[0..1], name_lower);
    }
    format!("{}/{}/{}", &name_lower[0..2], &name_lower[2..4], name_lower)
}

pub fn native_config_json(public_base_url: &str) -> String {
    let dl = format!("{}/blobs/{{sha256-checksum}}.crate", public_base_url.trim_end_matches('/'));
    let api = public_base_url.to_string();
    format!(
        "{{\"dl\":{},\"api\":{},\"auth-required\":false}}",
        json_str(&dl),
        json_str(&api)
    )
}

pub fn mirror_config_json(public_base_url: &str) -> String {
    let dl = format!("{}/blobs/{{sha256-checksum}}.crate", public_base_url.trim_end_matches('/'));
    format!("{{\"dl\":{}}}", json_str(&dl))
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
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

pub fn publish_to_index(body: &PublishBody) -> Result<IndexEntry> {
    publish_to_index_with_manifest(body, None)
}

pub fn publish_to_index_with_manifest(
    body: &PublishBody,
    packaged: Option<&crate::crate_file::PackagedManifest>,
) -> Result<IndexEntry> {
    let _ = CrateName::new(&body.name)?;
    let _ = Version::new(&body.vers)?;

    if let Some(manifest) = packaged {
        reconcile_manifest(body, manifest)?;
    }

    let mut deps_out_fixed: Vec<IndexDep> = Vec::new();
    for d in &body.deps {
        if d.name.as_ref().is_none_or(|s| s.is_empty()) && d.explicit_name_in_toml.is_none() {
            return Err(Error::Input("dependency missing name".to_string()));
        }
        let mut dep_name = d.name.clone().unwrap_or_default();
        let mut dep_package = d.package.clone();
        if let Some(ref explicit) = d.explicit_name_in_toml {
            dep_name = explicit.clone();
            if dep_package.is_none() {
                dep_package = d.name.clone();
            }
        }

        let req = d.version_req.clone();
        if req.is_empty() {
            return Err(Error::Input(format!("dependency '{dep_name}' missing version_req")));
        }
        if crate::version::VersionReq::parse(&req).is_err() {
            return Err(Error::Input(format!("dependency '{dep_name}' has invalid version_req '{req}'")));
        }
        if dep_name.is_empty() {
            return Err(Error::Input("dependency with empty name".to_string()));
        }
        if CrateName::new(&dep_name).is_err() {
            return Err(Error::Input(format!("dependency has invalid name '{dep_name}'")));
        }
        if let Some(ref pkg) = dep_package
            && CrateName::new(pkg).is_err()
        {
            return Err(Error::Input(format!("dependency has invalid package '{pkg}'")));
        }

        let registry_out = if let Some(ref reg) = d.registry {
            if reg == "https://github.com/rust-lang/crates.io-index" {
                Some("https://github.com/rust-lang/crates.io-index".to_string())
            } else if reg.is_empty() {
                None
            } else {
                Some(reg.clone())
            }
        } else {
            None
        };

        deps_out_fixed.push(IndexDep {
            name: dep_name,
            req,
            features: d.features.clone(),
            optional: d.optional,
            default_features: d.default_features,
            target: d.target.clone(),
            kind: d.kind.clone(),
            registry: registry_out,
            package: dep_package,
        });
    }

    let mut has_v2 = false;
    for v in body.features.values() {
        for val in v {
            if val.contains("dep:") || val.contains('?') {
                has_v2 = true;
                break;
            }
        }
        if has_v2 {
            break;
        }
    }
    let features_out: BTreeMap<String, Vec<String>> = body.features.clone();

    let entry = IndexEntry {
        name: body.name.clone(),
        vers: body.vers.clone(),
        deps: deps_out_fixed,
        cksum: body.checksum.clone(),
        features: features_out,
        yanked: body.yanked,
        links: body.links.clone(),
        rust_version: body.rust_version.clone(),
        v: if has_v2 { Some(2) } else { Some(1) },
        extra: BTreeMap::new(),
    };
    Ok(entry)
}

fn reconcile_manifest(body: &PublishBody, manifest: &crate::crate_file::PackagedManifest) -> Result<()> {
    if manifest.name != body.name || manifest.version != body.vers {
        return Err(Error::Integrity("name/version mismatch".to_string()));
    }
    if manifest.links != body.links {
        return Err(Error::Integrity("links mismatch between archive and publish metadata".to_string()));
    }
    if manifest.rust_version != body.rust_version {
        return Err(Error::Integrity(
            "rust_version mismatch between archive and publish metadata".to_string(),
        ));
    }
    if !features_equal(&manifest.features, &body.features) {
        return Err(Error::Integrity("features mismatch between archive and publish metadata".to_string()));
    }
    use std::collections::BTreeMap;
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct DepKey {
        alias: String,
        real: String,
        kind: String,
        target: Option<String>,
    }
    struct NormDep<'a> {
        key: DepKey,
        version_req: &'a str,
        features: &'a [String],
        optional: bool,
        default_features: bool,
        registry: Option<&'a str>,
        package: Option<&'a str>,
    }
    fn publish_alias_real(dep: &crate::model::Dependency) -> Result<(String, String)> {
        let (alias, real) = match (&dep.explicit_name_in_toml, &dep.name, &dep.package) {
            (Some(explicit), name, package) => (
                explicit.as_str(),
                package.as_deref().or(name.as_deref()).unwrap_or(explicit.as_str()),
            ),
            (None, Some(n), pkg) => (n.as_str(), pkg.as_deref().unwrap_or(n.as_str())),
            (None, None, _) => {
                return Err(Error::Integrity("publish dependency missing name".to_string()));
            }
        };
        Ok((alias.to_string(), real.to_string()))
    }
    let mut packaged_norm: Vec<NormDep<'_>> = Vec::new();
    for d in &manifest.dependencies {
        let real = d.package.clone().unwrap_or_else(|| d.alias.clone());
        let kind = d.kind.clone().unwrap_or_else(|| "normal".to_string());
        packaged_norm.push(NormDep {
            key: DepKey {
                alias: d.alias.clone(),
                real,
                kind,
                target: d.target.clone(),
            },
            version_req: d.version_req.as_str(),
            features: d.features.as_slice(),
            optional: d.optional,
            default_features: d.default_features,
            registry: match d.registry.as_deref() {
                Some("") | None => None,
                Some(r) => Some(r),
            },
            package: d.package.as_deref(),
        });
    }
    let mut publish_norm: Vec<(NormDep<'_>, &crate::model::Dependency)> = Vec::new();
    for dep in &body.deps {
        let (alias, real) = publish_alias_real(dep)?;
        let kind = dep.kind.clone().unwrap_or_else(|| "normal".to_string());
        publish_norm.push((
            NormDep {
                key: DepKey {
                    alias,
                    real,
                    kind,
                    target: dep.target.clone(),
                },
                version_req: dep.version_req.as_str(),
                features: dep.features.as_slice(),
                optional: dep.optional,
                default_features: dep.default_features,
                registry: match dep.registry.as_deref() {
                    Some("") | None => None,
                    Some(r) => Some(r),
                },
                package: dep.package.as_deref(),
            },
            dep,
        ));
    }
    if packaged_norm.len() != publish_norm.len() {
        return Err(Error::Integrity(format!(
            "dependency count mismatch: archive has {}, publish metadata has {}",
            packaged_norm.len(),
            publish_norm.len()
        )));
    }
    let mut packaged_by_key: BTreeMap<DepKey, Vec<usize>> = BTreeMap::new();
    for (i, d) in packaged_norm.iter().enumerate() {
        packaged_by_key.entry(d.key.clone()).or_default().push(i);
    }
    let mut publish_by_key: BTreeMap<DepKey, Vec<usize>> = BTreeMap::new();
    for (i, (d, _)) in publish_norm.iter().enumerate() {
        publish_by_key.entry(d.key.clone()).or_default().push(i);
    }
    if packaged_by_key.keys().collect::<Vec<_>>() != publish_by_key.keys().collect::<Vec<_>>() {
        let mut missing = Vec::new();
        for k in packaged_by_key.keys() {
            if !publish_by_key.contains_key(k) {
                missing.push(format!("archive-only '{}' kind={} target={:?}", k.alias, k.kind, k.target));
            }
        }
        for k in publish_by_key.keys() {
            if !packaged_by_key.contains_key(k) {
                missing.push(format!("publish-only '{}' kind={} target={:?}", k.alias, k.kind, k.target));
            }
        }
        return Err(Error::Integrity(format!(
            "dependency set mismatch (alias/package/kind/target): {}",
            missing.join("; ")
        )));
    }
    for (key, p_idxs) in &packaged_by_key {
        let g_idxs = &publish_by_key[key];
        if p_idxs.len() != g_idxs.len() {
            return Err(Error::Integrity(format!(
                "dependency '{}' count mismatch for kind={} target={:?}",
                key.alias, key.kind, key.target
            )));
        }
        for (pi, gi) in p_idxs.iter().zip(g_idxs.iter()) {
            let expected = &packaged_norm[*pi];
            let (got, raw) = &publish_norm[*gi];
            let alias = &key.alias;
            if !version_req_matches(got.version_req, expected.version_req) {
                return Err(Error::Integrity(format!(
                    "dependency '{alias}' version_req mismatch: archive '{}' vs publish '{}'",
                    expected.version_req, got.version_req
                )));
            }
            if !str_sets_equal(got.features, expected.features)
                || got.optional != expected.optional
                || got.default_features != expected.default_features
            {
                return Err(Error::Integrity(format!("dependency '{alias}' flags mismatch")));
            }
            match_registry(got.registry, expected.registry, alias)?;
            if !package_matches(raw, expected.package, alias) {
                return Err(Error::Integrity(format!("dependency '{alias}' package mismatch")));
            }
        }
    }
    Ok(())
}

fn features_equal(a: &std::collections::BTreeMap<String, Vec<String>>, b: &std::collections::BTreeMap<String, Vec<String>>) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (k, av) in a {
        match b.get(k) {
            Some(bv) if str_sets_equal(av, bv) => {}
            _ => return false,
        }
    }
    true
}

fn str_sets_equal(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut sa: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    let mut sb: Vec<&str> = b.iter().map(|s| s.as_str()).collect();
    sa.sort_unstable();
    sb.sort_unstable();
    sa == sb
}

fn is_crates_io_registry(v: &str) -> bool {
    v == "https://github.com/rust-lang/crates.io-index"
}

fn match_registry(got: Option<&str>, expected: Option<&str>, alias: &str) -> Result<()> {
    match (got, expected) {
        (None, None) => Ok(()),
        (Some(g), None) if is_crates_io_registry(g) => Ok(()),
        (None, Some(e)) if !is_crates_io_registry(e) => Ok(()),
        (Some(g), Some(e)) if g == e && is_crates_io_registry(g) => Ok(()),
        (Some(g), Some(e)) if g == e => Err(Error::Integrity(format!(
            "dependency '{alias}' uses unsupported registry '{g}'"
        ))),
        _ => Err(Error::Integrity(format!("dependency '{alias}' registry mismatch"))),
    }
}

fn package_matches(raw: &crate::model::Dependency, expected_package: Option<&str>, alias: &str) -> bool {
    if raw.package.as_deref() == expected_package {
        return true;
    }
    if raw.package.is_none() && expected_package == Some(alias) {
        return true;
    }
    if expected_package.is_none()
        && raw.package.as_deref() == Some(alias)
        && raw.explicit_name_in_toml.as_deref() == Some(alias)
    {
        return true;
    }
    let real = raw
        .package
        .as_deref()
        .or(raw.name.as_deref())
        .or(raw.explicit_name_in_toml.as_deref());
    raw.package.is_none() && expected_package.is_some() && real == expected_package
}

fn version_req_matches(publish_req: &str, packaged_req: &str) -> bool {
    if publish_req == packaged_req {
        return true;
    }
    if normalize_req(publish_req) == normalize_req(packaged_req) {
        return true;
    }
    let Ok(want) = crate::version::VersionReq::parse(publish_req) else {
        return false;
    };
    let Ok(have) = crate::version::VersionReq::parse(packaged_req) else {
        return false;
    };
    if want == have {
        return true;
    }
    let probes = ["1.0.0", "0.1.0", "0.0.1", "2.0.0", "0.2.0"];
    probes.iter().all(|v| {
        let ver = crate::version::Version::parse(v).unwrap();
        want.matches(&ver) == have.matches(&ver)
    }) && probes.iter().any(|v| {
        let ver = crate::version::Version::parse(v).unwrap();
        want.matches(&ver)
    })
}

fn normalize_req(req: &str) -> String {
    let trimmed = req.trim();
    if let Some(stripped) = trimmed.strip_prefix('^') {
        return stripped.to_string();
    }
    if trimmed == "*" {
        return String::new();
    }
    trimmed.to_string()
}

pub fn serialize_index_line(entry: &IndexEntry) -> Result<String> {
    let mut out = String::new();
    out.push('{');
    out.push_str(&format!("\"name\":{},", json_str(&entry.name)));
    out.push_str(&format!("\"vers\":{},", json_str(&entry.vers)));
    out.push_str("\"deps\":[");
    for (i, dep) in entry.deps.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        out.push_str(&format!("\"name\":{},", json_str(&dep.name)));
        out.push_str(&format!("\"req\":{},", json_str(&dep.req)));
        out.push_str(&format!("\"features\":{},", str_list_json(&dep.features)));
        out.push_str(&format!("\"optional\":{},", dep.optional));
        out.push_str(&format!("\"default_features\":{},", dep.default_features));
        if let Some(ref t) = dep.target {
            out.push_str(&format!("\"target\":{},", json_str(t)));
        }
        if let Some(ref k) = dep.kind {
            out.push_str(&format!("\"kind\":{},", json_str(k)));
        }
        match dep.registry.as_ref() {
            None => out.push_str("\"registry\":null,"),
            Some(r) => out.push_str(&format!("\"registry\":{},", json_str(r))),
        }
        if let Some(ref p) = dep.package {
            out.push_str(&format!("\"package\":{},", json_str(p)));
        }
        if out.ends_with(',') {
            out.pop();
        }
        out.push('}');
    }
    out.push_str("],");
    out.push_str(&format!("\"cksum\":{},", json_str(&entry.cksum)));
    out.push_str(&format!("\"features\":{},", features_json(&entry.features)));
    out.push_str(&format!("\"yanked\":{}", entry.yanked));
    if let Some(ref links) = entry.links {
        out.push_str(&format!(",\"links\":{}", json_str(links)));
    }
    if let Some(ref rv) = entry.rust_version {
        out.push_str(&format!(",\"rust_version\":{}", json_str(rv)));
    }
    if let Some(v) = entry.v {
        out.push_str(&format!(",\"v\":{v}"));
    }
    for (k, v) in &entry.extra {
        let vs = crate::json::to_string(v).map_err(|e| Error::Operation(e.to_string()))?;
        out.push_str(&format!(",{}:{}", json_str(k), vs));
    }
    out.push('}');
    Ok(out)
}

fn str_list_json(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(s));
    }
    out.push(']');
    out
}

fn features_json(features: &BTreeMap<String, Vec<String>>) -> String {
    let mut out = String::from("{");
    for (i, (k, v)) in features.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_str(k));
        out.push(':');
        out.push_str(&str_list_json(v));
    }
    out.push('}');
    out
}
