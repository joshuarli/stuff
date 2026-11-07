use deser_value::{Kind, Value};

use super::wirejson::{compact, json_string};
use super::{input, Rejection};

pub const CRATES_IO_INDEX: &str = "https://github.com/rust-lang/crates.io-index";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Features(Vec<(String, Vec<String>)>);

impl Features {
    pub fn empty() -> Self {
        Self(Vec::new())
    }

    pub(crate) fn from_entries(entries: Vec<(String, Vec<String>)>) -> Self {
        Self(entries)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0
            .iter()
            .map(|(key, values)| (key.as_str(), values.as_slice()))
    }

    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, values)| values.as_slice())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn sorted(&self) -> Self {
        let mut entries = self.0.clone();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        Self(entries)
    }

    pub fn has_v2_syntax(&self) -> bool {
        self.0
            .iter()
            .flat_map(|(_, values)| values.iter())
            .any(|value| value.contains("dep:") || value.contains('?'))
    }

    fn from_json(value: &Value, context: &str) -> Result<Self, Rejection> {
        let map = value
            .as_map()
            .ok_or_else(|| input(format!("{context} must be an object")))?;
        let mut entries = Vec::new();
        for (key, values) in map.iter() {
            let key = key.as_str().unwrap_or_default();
            let seq = values
                .as_seq()
                .ok_or_else(|| input(format!("{context}.{key} must be a list of strings")))?;
            let mut items = Vec::new();
            for value in seq.iter() {
                let item = value
                    .as_str()
                    .ok_or_else(|| input(format!("{context}.{key} must be a list of strings")))?;
                items.push(item.to_string());
            }
            entries.push((key.to_string(), items));
        }
        Ok(Self(entries))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishDependency {
    pub name: Option<String>,
    pub version_req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<String>,
    pub registry: Option<String>,
    pub package: Option<String>,
    pub explicit_name_in_toml: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishRequest {
    pub name: String,
    pub vers: String,
    pub deps: Vec<PublishDependency>,
    pub features: Features,
    pub links: Option<String>,
    pub rust_version: Option<String>,
    pub checksum: String,
    pub yanked: bool,
}

impl PublishRequest {
    pub fn from_json(text: &str) -> Result<Self, Rejection> {
        let value: Value = deser_json::from_str(text).map_err(|error| input(error.to_string()))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, Rejection> {
        let map = value
            .as_map()
            .ok_or_else(|| input("publish metadata must be an object"))?;
        for required in ["name", "vers", "deps"] {
            if map.get(required).is_none() {
                return Err(input(format!("publish metadata is missing {required}")));
            }
        }
        let name = expect_str(value, "name", "", "publish")?;
        let vers = expect_str(value, "vers", "", "publish")?;
        let deps_value = map_value(value, "deps").unwrap_or(value);
        let deps_seq = deps_value
            .as_seq()
            .ok_or_else(|| input("publish.deps has the wrong type"))?;
        let mut deps = Vec::new();
        for dependency in deps_seq.iter() {
            deps.push(PublishDependency::from_value(dependency)?);
        }
        let features = match map.get("features") {
            Some(value) => Features::from_json(value, "publish.features")?,
            None => Features::empty(),
        };
        Ok(Self {
            name,
            vers,
            deps,
            features,
            links: optional_str(value, "links", "publish")?,
            rust_version: optional_str(value, "rust_version", "publish")?,
            checksum: expect_str(value, "checksum", "", "publish")?,
            yanked: expect_bool(value, "yanked", false, "publish")?,
        })
    }
}

impl PublishDependency {
    fn from_value(value: &Value) -> Result<Self, Rejection> {
        if value.as_map().is_none() {
            return Err(input("publish dependency must be an object"));
        }
        Ok(Self {
            name: optional_str(value, "name", "dep")?,
            version_req: expect_str(value, "version_req", "", "dep")?,
            features: string_list(value, "features", "dep")?,
            optional: expect_bool(value, "optional", false, "dep")?,
            default_features: expect_bool(value, "default_features", false, "dep")?,
            target: optional_str(value, "target", "dep")?,
            kind: optional_str(value, "kind", "dep")?,
            registry: optional_str(value, "registry", "dep")?,
            package: optional_str(value, "package", "dep")?,
            explicit_name_in_toml: optional_str(value, "explicit_name_in_toml", "dep")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DependencyKind {
    Normal,
    Dev,
    Build,
    Other(String),
}

impl DependencyKind {
    pub fn parse(text: &str) -> Self {
        match text {
            "normal" => Self::Normal,
            "dev" => Self::Dev,
            "build" => Self::Build,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Normal => "normal",
            Self::Dev => "dev",
            Self::Build => "build",
            Self::Other(text) => text,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexDep {
    pub name: String,
    pub req: String,
    pub features: Vec<String>,
    pub optional: bool,
    pub default_features: bool,
    pub target: Option<String>,
    pub kind: Option<DependencyKind>,
    pub registry: Option<String>,
    pub package: Option<String>,
}

impl IndexDep {
    pub fn from_value(value: &Value) -> Result<Self, Rejection> {
        if value.as_map().is_none() {
            return Err(input("index dependency must be an object"));
        }
        let kind = optional_str(value, "kind", "dep")?.map(|text| DependencyKind::parse(&text));
        Ok(Self {
            name: expect_str(value, "name", "", "dep")?,
            req: expect_str(value, "req", "", "dep")?,
            features: string_list(value, "features", "dep")?,
            optional: expect_bool(value, "optional", false, "dep")?,
            default_features: expect_bool(value, "default_features", false, "dep")?,
            target: optional_str(value, "target", "dep")?,
            kind,
            registry: optional_str(value, "registry", "dep")?,
            package: optional_str(value, "package", "dep")?,
        })
    }

    pub fn line(&self) -> String {
        let mut out = String::from("{\"name\":");
        out.push_str(&json_string(&self.name));
        out.push_str(",\"req\":");
        out.push_str(&json_string(&self.req));
        let features: Vec<String> = self.features.iter().map(|value| json_string(value)).collect();
        out.push_str(&format!(",\"features\":[{}]", features.join(",")));
        out.push_str(&format!(",\"optional\":{}", bool_text(self.optional)));
        out.push_str(&format!(
            ",\"default_features\":{}",
            bool_text(self.default_features)
        ));
        if let Some(target) = &self.target {
            out.push_str(",\"target\":");
            out.push_str(&json_string(target));
        }
        if let Some(kind) = &self.kind {
            out.push_str(",\"kind\":");
            out.push_str(&json_string(kind.as_str()));
        }
        out.push_str(",\"registry\":");
        match &self.registry {
            Some(registry) => out.push_str(&json_string(registry)),
            None => out.push_str("null"),
        }
        if let Some(package) = &self.package {
            out.push_str(",\"package\":");
            out.push_str(&json_string(package));
        }
        out.push('}');
        out
    }
}

const KNOWN_ENTRY_KEYS: [&str; 9] = [
    "name",
    "vers",
    "deps",
    "cksum",
    "features",
    "yanked",
    "links",
    "rust_version",
    "v",
];

#[derive(Clone, Debug, PartialEq)]
pub struct IndexEntry {
    pub name: String,
    pub vers: String,
    pub deps: Vec<IndexDep>,
    pub cksum: String,
    pub features: Features,
    pub yanked: bool,
    pub links: Option<String>,
    pub rust_version: Option<String>,
    pub v: Option<u32>,
    pub extra: Vec<(String, Value)>,
}

impl IndexEntry {
    pub fn from_json_line(line: &str) -> Result<Self, Rejection> {
        let value: Value = deser_json::from_str(line).map_err(|error| input(error.to_string()))?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, Rejection> {
        let map = value
            .as_map()
            .ok_or_else(|| input("index entry must be an object"))?;
        let deps = match map.get("deps") {
            None => Vec::new(),
            Some(value) => {
                let seq = value
                    .as_seq()
                    .ok_or_else(|| input("entry.deps has the wrong type"))?;
                let mut deps = Vec::new();
                for dependency in seq.iter() {
                    deps.push(IndexDep::from_value(dependency)?);
                }
                deps
            }
        };
        let features = match map.get("features") {
            Some(value) => Features::from_json(value, "entry.features")?,
            None => Features::empty(),
        };
        let mut extra = Vec::new();
        for (key, value) in map.iter() {
            let key = key.as_str().unwrap_or_default();
            if !KNOWN_ENTRY_KEYS.contains(&key) {
                extra.push((key.to_string(), value.clone()));
            }
        }
        Ok(Self {
            name: expect_str(value, "name", "", "entry")?,
            vers: expect_str(value, "vers", "", "entry")?,
            deps,
            cksum: expect_str(value, "cksum", "", "entry")?,
            features,
            yanked: expect_bool(value, "yanked", false, "entry")?,
            links: optional_str(value, "links", "entry")?,
            rust_version: optional_str(value, "rust_version", "entry")?,
            v: optional_u32(value, "v", "entry")?,
            extra,
        })
    }

    pub fn index_line(&self) -> String {
        let mut out = String::from("{\"name\":");
        out.push_str(&json_string(&self.name));
        out.push_str(",\"vers\":");
        out.push_str(&json_string(&self.vers));
        let deps: Vec<String> = self.deps.iter().map(IndexDep::line).collect();
        out.push_str(&format!(",\"deps\":[{}]", deps.join(",")));
        out.push_str(",\"cksum\":");
        out.push_str(&json_string(&self.cksum));
        out.push_str(",\"features\":");
        out.push_str(&features_line(&self.features));
        out.push_str(&format!(",\"yanked\":{}", bool_text(self.yanked)));
        if let Some(links) = &self.links {
            out.push_str(",\"links\":");
            out.push_str(&json_string(links));
        }
        if let Some(rust_version) = &self.rust_version {
            out.push_str(",\"rust_version\":");
            out.push_str(&json_string(rust_version));
        }
        if let Some(v) = self.v {
            out.push_str(&format!(",\"v\":{v}"));
        }
        let mut extra: Vec<&(String, Value)> = self.extra.iter().collect();
        extra.sort_by(|left, right| left.0.cmp(&right.0));
        for (key, value) in extra {
            out.push(',');
            out.push_str(&json_string(key));
            out.push(':');
            out.push_str(&compact(value));
        }
        out.push('}');
        out
    }
}

fn features_line(features: &Features) -> String {
    let mut out = String::from("{");
    for (index, (key, values)) in features.sorted().iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&json_string(key));
        out.push(':');
        let items: Vec<String> = values.iter().map(|value| json_string(value)).collect();
        out.push_str(&format!("[{}]", items.join(",")));
    }
    out.push('}');
    out
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn map_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_map().and_then(|map| map.get(key))
}

fn expect_str(
    value: &Value,
    key: &str,
    default: &str,
    context: &str,
) -> Result<String, Rejection> {
    match map_value(value, key) {
        None => Ok(default.to_string()),
        Some(field) => field
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| input(format!("{context}.{key} has the wrong type"))),
    }
}

fn optional_str(value: &Value, key: &str, context: &str) -> Result<Option<String>, Rejection> {
    match map_value(value, key) {
        None => Ok(None),
        Some(field) if field.is_null() => Ok(None),
        Some(field) => field
            .as_str()
            .map(|text| Some(text.to_string()))
            .ok_or_else(|| input(format!("{context}.{key} has the wrong type"))),
    }
}

fn expect_bool(
    value: &Value,
    key: &str,
    default: bool,
    context: &str,
) -> Result<bool, Rejection> {
    match map_value(value, key) {
        None => Ok(default),
        Some(field) => field
            .as_bool()
            .ok_or_else(|| input(format!("{context}.{key} has the wrong type"))),
    }
}

fn string_list(value: &Value, key: &str, context: &str) -> Result<Vec<String>, Rejection> {
    match map_value(value, key) {
        None => Ok(Vec::new()),
        Some(field) => {
            let seq = field
                .as_seq()
                .ok_or_else(|| input(format!("{context}.{key} has the wrong type")))?;
            let mut out = Vec::new();
            for item in seq.iter() {
                let text = item
                    .as_str()
                    .ok_or_else(|| input(format!("{context}.{key} must be a list of strings")))?;
                out.push(text.to_string());
            }
            Ok(out)
        }
    }
}

fn optional_u32(value: &Value, key: &str, context: &str) -> Result<Option<u32>, Rejection> {
    match map_value(value, key) {
        None => Ok(None),
        Some(field) if field.is_null() => Ok(None),
        Some(field) => match field.kind() {
            Kind::U64(number) if *number < u64::from(u32::MAX) + 1 => Ok(Some(*number as u32)),
            Kind::I64(number) if *number >= 0 && *number < i64::from(u32::MAX) + 1 => {
                Ok(Some(*number as u32))
            }
            _ => Err(input(format!("{context}.{key} has the wrong type"))),
        },
    }
}
