#![forbid(unsafe_code)]

use crate::error::{Error, Result, Stage};
use crate::models::Target;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ProjectSettings {
    pub python_specifier: pep440_rs::VersionSpecifiers,
    pub python_specifier_text: String,
    pub python_version: pep440_rs::Version,
    pub platforms: Vec<String>,
    pub linux_repair_policy: String,
    pub public_base_url: String,
}

impl ProjectSettings {
    pub fn targets(&self) -> Vec<Target> {
        self.platforms
            .iter()
            .map(|platform| Target {
                python_version: self.python_version.clone(),
                platform: platform.clone(),
            })
            .collect()
    }

    pub fn contains_python(&self, version: &pep440_rs::Version) -> bool {
        self.python_specifier.contains(version)
    }
}

fn fail(detail: impl Into<String>) -> Error {
    Error::new(crate::error::Kind::Configuration, detail, Stage::Configuration)
}

fn map_keys(map: &deser_value::Map) -> Vec<String> {
    let mut out = Vec::with_capacity(map.len());
    for i in 0..map.len() {
        if let Some((k, _)) = map.get_index(i)
            && let Some(s) = k.as_str()
        {
            out.push(s.to_string());
        }
    }
    out
}

pub fn parse_project_settings(source: &str) -> Result<ProjectSettings> {
    let value: deser_value::Value =
        deser_toml::from_str(source).map_err(|e| fail(format!("invalid project target configuration: {e}")))?;
    let doc = value
        .as_map()
        .ok_or_else(|| fail("invalid project target configuration"))?;
    let project = doc
        .get("project")
        .and_then(|v| v.as_map())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    let requires_python = project
        .get("requires-python")
        .and_then(|v| v.as_str())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    let specifier: pep440_rs::VersionSpecifiers = requires_python
        .parse()
        .map_err(|_| fail("invalid project target configuration"))?;
    let python_version = python_minor(&specifier)?;

    let tool = doc
        .get("tool")
        .and_then(|v| v.as_map())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    let mypypi = tool
        .get("mypypi")
        .and_then(|v| v.as_map())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    let targets = mypypi
        .get("targets")
        .and_then(|v| v.as_map())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    for key in map_keys(targets) {
        if key != "platforms" && key != "linux_repair_policy" {
            return Err(fail(format!("unknown target setting: {key}")));
        }
    }
    let raw_platforms = targets.get("platforms").ok_or_else(|| fail("invalid project target configuration"))?;
    let platforms_seq = raw_platforms
        .as_seq()
        .ok_or_else(|| fail("target platforms must be non-empty names"))?;
    if platforms_seq.is_empty() {
        return Err(fail("target platforms must be non-empty names"));
    }
    let mut platforms = Vec::with_capacity(platforms_seq.len());
    for item in platforms_seq.iter() {
        match item.as_str() {
            Some(s) if !s.is_empty() && s.trim() == s => platforms.push(s.to_string()),
            _ => return Err(fail("target platforms must be non-empty names")),
        }
    }
    {
        let mut sorted = platforms.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != platforms.len() {
            return Err(fail("target platforms must be unique"));
        }
    }
    let policy = targets
        .get("linux_repair_policy")
        .and_then(|v| v.as_str())
        .ok_or_else(|| fail("invalid project target configuration"))?;
    if policy.is_empty() || policy.trim() != policy {
        return Err(fail("Linux repair policy must be a non-empty name"));
    }
    let public_base_url = match mypypi.get("public_base_url") {
        Some(value) => {
            let text = value
                .as_str()
                .ok_or_else(|| fail("public_base_url must be a URL"))?;
            parse_public_base_url(text)?
        }
        None => stuff_catalog::DEFAULT_PYPI_ORIGIN.to_string(),
    };
    Ok(ProjectSettings {
        python_specifier: specifier,
        python_specifier_text: requires_python.to_string(),
        python_version,
        platforms,
        linux_repair_policy: policy.to_string(),
        public_base_url,
    })
}

fn parse_public_base_url(text: &str) -> Result<String> {
    let parsed: url::Url = text
        .parse()
        .map_err(|_| fail(format!("public_base_url is not a valid URL: {text}")))?;
    if parsed.scheme() != "https" && parsed.scheme() != "http" {
        return Err(fail("public_base_url must use https or http"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(fail("public_base_url must not contain credentials"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(fail("public_base_url must not contain a query or fragment"));
    }
    Ok(text.trim_end_matches('/').to_string())
}

fn python_minor(spec: &pep440_rs::VersionSpecifiers) -> Result<pep440_rs::Version> {
    if spec.len() != 2 {
        return Err(fail(
            "project.requires-python must select exactly one Python minor release",
        ));
    }
    let mut lower: Option<&pep440_rs::Version> = None;
    let mut upper: Option<&pep440_rs::Version> = None;
    for s in spec.iter() {
        match s.operator() {
            pep440_rs::Operator::GreaterThanEqual => lower = Some(s.version()),
            pep440_rs::Operator::LessThan => upper = Some(s.version()),
            _ => {
                return Err(fail(
                    "project.requires-python must use an inclusive lower and exclusive upper bound",
                ));
            }
        }
    }
    let (Some(lo), Some(hi)) = (lower, upper) else {
        return Err(fail(
            "project.requires-python must use an inclusive lower and exclusive upper bound",
        ));
    };
    if lo.release().len() != 2 || hi.release().len() != 2 {
        return Err(fail(
            "project.requires-python bounds must name major and minor releases",
        ));
    }
    if lo.release()[0] != hi.release()[0] || hi.release()[1] != lo.release()[1] + 1 {
        return Err(fail(
            "project.requires-python must cover exactly one Python minor release",
        ));
    }
    Ok(pep440_rs::Version::new([lo.release()[0], lo.release()[1]]))
}

pub fn load_project_settings(path: &Path) -> Result<ProjectSettings> {
    let source = std::fs::read_to_string(path).map_err(|e| {
        Error::io(format!("could not read project configuration at {}: {e}", path.display()), Stage::Configuration)
    })?;
    parse_project_settings(&source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(requires_python: &str) -> String {
        format!(
            "[project]\nrequires-python = \"{requires_python}\"\n\n[tool.mypypi.targets]\nplatforms = [\"linux-x86_64\", \"linux-aarch64\", \"macos-x86_64\", \"macos-arm64\"]\nlinux_repair_policy = \"manylinux_2_28\"\n"
        )
    }

    #[test]
    fn derives_python_from_project_metadata() {
        let settings = parse_project_settings(&project(">=3.15,<3.16")).unwrap();
        assert_eq!(settings.python_version.to_string(), "3.15");
        let v315: pep440_rs::Version = "3.15.0".parse().unwrap();
        let v314: pep440_rs::Version = "3.14.0".parse().unwrap();
        let v316: pep440_rs::Version = "3.16.0".parse().unwrap();
        assert!(settings.contains_python(&v315));
        assert!(!settings.contains_python(&v314));
        assert!(!settings.contains_python(&v316));
        let platforms: Vec<&str> = settings.platforms.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            platforms,
            vec!["linux-x86_64", "linux-aarch64", "macos-x86_64", "macos-arm64"]
        );
    }

    #[test]
    fn rejects_non_single_minor() {
        for req in [">=3.14,<3.16", ">=3.15,<3.17", "==3.15.*"] {
            assert!(parse_project_settings(&project(req)).is_err(), "{req}");
        }
    }

    #[test]
    fn rejects_duplicate_platforms() {
        let src = project(">=3.15,<3.16").replace(
            "\"macos-arm64\"]",
            "\"macos-arm64\", \"macos-arm64\"]",
        );
        let err = parse_project_settings(&src).unwrap_err();
        assert!(err.detail.contains("must be unique"));
    }

    #[test]
    fn rejects_unknown_target_setting() {
        let src2 = "[project]\nrequires-python = \">=3.15,<3.16\"\n\n[tool.mypypi.targets]\nplatforms = [\"linux-x86_64\"]\nlinux_repair_policy = \"manylinux_2_28\"\nextra = \"x\"\n";
        let err = parse_project_settings(src2).unwrap_err();
        assert!(err.detail.contains("unknown target setting"));
    }

    #[test]
    fn public_base_url_defaults_to_the_published_origin() {
        let settings = parse_project_settings(&project(">=3.15,<3.16")).unwrap();
        assert_eq!(settings.public_base_url, stuff_catalog::DEFAULT_PYPI_ORIGIN);
    }

    #[test]
    fn public_base_url_is_configurable_and_normalized() {
        let src = project(">=3.15,<3.16").replace(
            "[tool.mypypi.targets]",
            "[tool.mypypi]\npublic_base_url = \"https://pypi.example.test/mirror/\"\n\n[tool.mypypi.targets]",
        );
        let settings = parse_project_settings(&src).unwrap();
        assert_eq!(settings.public_base_url, "https://pypi.example.test/mirror");
    }

    #[test]
    fn rejects_invalid_public_base_url() {
        for url in ["not a url", "ftp://pypi.example.test", "https://user:pass@pypi.example.test"] {
            let src = project(">=3.15,<3.16").replace(
                "[tool.mypypi.targets]",
                &format!("[tool.mypypi]\npublic_base_url = \"{url}\"\n\n[tool.mypypi.targets]"),
            );
            assert!(parse_project_settings(&src).is_err(), "{url}");
        }
    }
}
