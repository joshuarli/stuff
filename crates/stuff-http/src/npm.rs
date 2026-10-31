#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmTarballPath {
    pub package: String,
    pub version: String,
}

impl NpmTarballPath {
    pub fn packument_segments(&self) -> Vec<&str> {
        self.package.split('/').collect()
    }
}

pub fn parse_npm_tarball_path(segments: &[String]) -> Option<NpmTarballPath> {
    let (package, file) = match segments {
        [name, dash, file] if dash == "-" && !name.starts_with('@') => (name.clone(), file),
        [scope, name, dash, file] if dash == "-" && scope.starts_with('@') => {
            (format!("{scope}/{name}"), file)
        }
        _ => return None,
    };
    let basename = package.rsplit('/').next()?;
    let version = file
        .strip_prefix(basename)?
        .strip_prefix('-')?
        .strip_suffix(".tgz")?;
    if version.is_empty() {
        return None;
    }
    Some(NpmTarballPath {
        package,
        version: version.to_string(),
    })
}

pub fn packument_tarball_digest(packument_json: &str, version: &str) -> Option<String> {
    let packument: deser_value::Value = deser_json::from_str(packument_json).ok()?;
    let tarball = packument
        .as_map()?
        .get("versions")?
        .as_map()?
        .get(version)?
        .as_map()?
        .get("dist")?
        .as_map()?
        .get("tarball")?
        .as_str()?;
    let digest = tarball.rsplit('/').next()?.strip_suffix(".tgz")?;
    let valid = digest.len() == 128
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    valid.then(|| digest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(path: &str) -> Vec<String> {
        path.split('/').map(str::to_string).collect()
    }

    #[test]
    fn parses_flat_and_scoped_tarball_paths() {
        let flat = parse_npm_tarball_path(&segs("acorn/-/acorn-8.1.0.tgz")).unwrap();
        assert_eq!(flat.package, "acorn");
        assert_eq!(flat.version, "8.1.0");
        let scoped = parse_npm_tarball_path(&segs("@types/node/-/node-22.0.0-beta.1.tgz")).unwrap();
        assert_eq!(scoped.package, "@types/node");
        assert_eq!(scoped.version, "22.0.0-beta.1");
        assert_eq!(scoped.packument_segments(), ["@types", "node"]);
    }

    #[test]
    fn rejects_other_shapes() {
        for path in [
            "-/tarballs/abc.tgz",
            "acorn/-/other-8.1.0.tgz",
            "acorn/-/acorn-.tgz",
            "acorn/-/acorn-8.1.0.zip",
            "acorn/tarballs/acorn-8.1.0.tgz",
            "@types/-/node-1.0.0.tgz",
            "@types/node/-/other-1.0.0.tgz",
            "acorn/-/acorn-1.0.0.tgz/extra",
        ] {
            assert_eq!(parse_npm_tarball_path(&segs(path)), None, "{path}");
        }
    }

    #[test]
    fn finds_digest_for_exact_version_only() {
        let digest = "ab".repeat(64);
        let packument = format!(
            r#"{{"name":"acorn","versions":{{"8.1.0":{{"dist":{{"tarball":"https://h/current/-/tarballs/{digest}.tgz"}}}},"8.2.0":{{"dist":{{"tarball":"https://h/x/not-a-digest.tgz"}}}}}}}}"#
        );
        assert_eq!(packument_tarball_digest(&packument, "8.1.0"), Some(digest));
        assert_eq!(packument_tarball_digest(&packument, "8.2.0"), None);
        assert_eq!(packument_tarball_digest(&packument, "8.3.0"), None);
        assert_eq!(packument_tarball_digest("not json", "8.1.0"), None);
    }
}
