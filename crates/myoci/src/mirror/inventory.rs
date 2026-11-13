#![forbid(unsafe_code)]

use crate::error::{Error, Result};
use crate::mirror::reference::RegistryReference;
use stuff_oci::{Digest, Tag};
use std::path::Path;

pub const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
pub const SCHEMA_VERSION: u32 = 1;
pub const POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PlatformRequirement {
    pub os: String,
    pub architecture: String,
    pub variant: Option<String>,
}

impl PlatformRequirement {
    pub fn parse(text: &str, context: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split('/').collect();
        match parts.as_slice() {
            [os, architecture] => Ok(Self {
                os: validate_platform_component(os, context)?,
                architecture: validate_platform_component(architecture, context)?,
                variant: None,
            }),
            [os, architecture, variant] => Ok(Self {
                os: validate_platform_component(os, context)?,
                architecture: validate_platform_component(architecture, context)?,
                variant: Some(validate_platform_component(variant, context)?),
            }),
            _ => Err(Error::Input(format!(
                "{context}: platform requirement {text:?} must be os/architecture or os/architecture/variant"
            ))),
        }
    }

    pub fn matches(&self, platform: &stuff_oci::Platform) -> bool {
        self.os == platform.os
            && self.architecture == platform.architecture
            && match &self.variant {
                Some(variant) => platform.variant.as_deref() == Some(variant.as_str()),
                None => true,
            }
    }

    pub fn render(&self) -> String {
        match &self.variant {
            Some(variant) => format!("{}/{}/{}", self.os, self.architecture, variant),
            None => format!("{}/{}", self.os, self.architecture),
        }
    }
}

fn validate_platform_component(component: &str, context: &str) -> Result<String> {
    let bytes = component.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return Err(Error::Input(format!(
            "{context}: platform component {component:?} is empty or too long"
        )));
    }
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return Err(Error::Input(format!(
            "{context}: platform component {component:?} must start with a lowercase letter or digit"
        )));
    }
    if !bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')) {
        return Err(Error::Input(format!(
            "{context}: platform component {component:?} must be lowercase alphanumerics with '.', '_', '-'"
        )));
    }
    Ok(component.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferrersMode {
    Snapshot,
    None,
}

impl ReferrersMode {
    pub fn parse(text: &str, context: &str) -> Result<Self> {
        match text {
            "snapshot" => Ok(Self::Snapshot),
            "none" => Ok(Self::None),
            other => Err(Error::Input(format!(
                "{context}: referrers must be \"snapshot\" or \"none\", not {other:?}"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InventoryEntry {
    pub id: String,
    pub source: RegistryReference,
    pub destination: RegistryReference,
    pub aliases: Vec<Tag>,
    pub platforms: Vec<PlatformRequirement>,
    pub referrers: ReferrersMode,
}

#[derive(Debug, Clone)]
pub struct Inventory {
    pub entries: Vec<InventoryEntry>,
}

pub fn read_capped(path: &Path, context: &str) -> Result<String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        Error::Operation(format!("could not stat {context} {}: {error}", path.display()))
    })?;
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(Error::Limit(format!(
            "{context} {} exceeds the {MAX_CONFIG_BYTES}-byte limit",
            path.display()
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|error| Error::Operation(format!("could not read {context} {}: {error}", path.display())))
}

impl Inventory {
    pub fn load(path: &Path) -> Result<Self> {
        let text = read_capped(path, "inventory")?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let document: toml_edit::DocumentMut = text
            .parse()
            .map_err(|error| Error::Input(format!("inventory is not valid TOML: {error}")))?;
        let root = document.as_table();
        for (key, _) in root.iter() {
            match key {
                "schema_version" | "policy_version" | "entries" => {}
                other => {
                    return Err(Error::Input(format!("inventory: unknown field {other:?}")));
                }
            }
        }
        let schema_version = integer_field(root, "schema_version", "inventory")?;
        let policy_version = integer_field(root, "policy_version", "inventory")?;
        if schema_version != u64::from(SCHEMA_VERSION) {
            return Err(Error::Input(format!(
                "inventory: unsupported schema_version {schema_version}; expected {SCHEMA_VERSION}"
            )));
        }
        if policy_version != u64::from(POLICY_VERSION) {
            return Err(Error::Input(format!(
                "inventory: unsupported policy_version {policy_version}; expected {POLICY_VERSION}"
            )));
        }
        let entries_value = root
            .get("entries")
            .and_then(|item| item.as_array_of_tables())
            .ok_or_else(|| Error::Input("inventory: entries must be an array of tables".to_string()))?;
        let mut entries = Vec::with_capacity(entries_value.len());
        for (index, table) in entries_value.iter().enumerate() {
            entries.push(parse_entry(table, index)?);
        }
        let inventory = Self { entries };
        inventory.validate()?;
        Ok(inventory)
    }

    fn validate(&self) -> Result<()> {
        let mut ids = rustc_hash::FxHashSet::default();
        let mut tag_owners: rustc_hash::FxHashMap<(String, String), String> = rustc_hash::FxHashMap::default();
        for entry in &self.entries {
            if !ids.insert(entry.id.clone()) {
                return Err(Error::Input(format!("inventory: duplicate entry id {:?}", entry.id)));
            }
            let destination = entry.destination.repository_id();
            for alias in &entry.aliases {
                if let Some(owner) = tag_owners.insert((destination.clone(), alias.as_str().to_string()), entry.id.clone()) {
                    return Err(Error::Input(format!(
                        "inventory: alias {:?} at destination {destination} is assigned to both {owner:?} and {:?}",
                        alias.as_str(),
                        entry.id
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> Digest {
        let mut payload = String::from("oci-mirror-inventory-v1\0");
        payload.push_str(&format!("schema_version={SCHEMA_VERSION}\0policy_version={POLICY_VERSION}\0"));
        let mut entries: Vec<&InventoryEntry> = self.entries.iter().collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        for entry in entries {
            payload.push_str(&format!("id={}\0", entry.id));
            payload.push_str(&format!("source={}\0", entry.source.repository_id()));
            if let Some(reference) = &entry.source.reference {
                payload.push_str(&format!("source_reference={}\0", reference.as_str()));
            }
            payload.push_str(&format!("destination={}\0", entry.destination.repository_id()));
            let mut aliases: Vec<&Tag> = entry.aliases.iter().collect();
            aliases.sort();
            let aliases: Vec<&str> = aliases.iter().map(|alias| alias.as_str()).collect();
            payload.push_str(&format!("aliases={}\0", aliases.join(",")));
            let mut platforms: Vec<String> = entry.platforms.iter().map(PlatformRequirement::render).collect();
            platforms.sort();
            payload.push_str(&format!("platforms={}\0", platforms.join(",")));
            payload.push_str(&format!("referrers={}\0", entry.referrers.as_str()));
        }
        Digest::from_bytes(payload.as_bytes())
    }

    pub fn entry(&self, id: &str) -> Option<&InventoryEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

fn parse_entry(table: &toml_edit::Table, index: usize) -> Result<InventoryEntry> {
    let context = format!("inventory entries[{index}]");
    for (key, _) in table.iter() {
        match key {
            "id" | "source" | "destination" | "aliases" | "platforms" | "referrers" => {}
            other => {
                return Err(Error::Input(format!("{context}: unknown field {other:?}")));
            }
        }
    }
    let id = string_field(table, "id", &context)?;
    validate_id(&id, &context)?;
    let source_text = string_field(table, "source", &context)?;
    let source = RegistryReference::parse(&source_text, &format!("{context} source"))?;
    if source.reference.is_none() {
        return Err(Error::Input(format!(
            "{context} source {source_text:?} must name a tag or digest"
        )));
    }
    let destination_text = string_field(table, "destination", &context)?;
    let destination = RegistryReference::parse(&destination_text, &format!("{context} destination"))?;
    if destination.reference.is_some() {
        return Err(Error::Input(format!(
            "{context} destination {destination_text:?} must be a repository without a tag or digest"
        )));
    }
    let aliases = string_array(table, "aliases", &context)?
        .iter()
        .map(|alias| {
            Tag::parse(alias).map_err(|error| Error::Input(format!("{context} aliases: {error}")))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut seen = rustc_hash::FxHashSet::default();
    for alias in &aliases {
        if !seen.insert(alias.as_str().to_string()) {
            return Err(Error::Input(format!("{context}: duplicate alias {:?}", alias.as_str())));
        }
    }
    let mut aliases = aliases;
    aliases.sort();
    let platforms = string_array(table, "platforms", &context)?
        .iter()
        .map(|platform| PlatformRequirement::parse(platform, &format!("{context} platforms")))
        .collect::<Result<Vec<_>>>()?;
    let mut seen = rustc_hash::FxHashSet::default();
    for platform in &platforms {
        if !seen.insert(platform.render()) {
            return Err(Error::Input(format!("{context}: duplicate platform {:?}", platform.render())));
        }
    }
    let mut platforms = platforms;
    platforms.sort();
    let referrers = match table.get("referrers") {
        Some(item) => ReferrersMode::parse(
            item.as_str()
                .ok_or_else(|| Error::Input(format!("{context} referrers must be a string")))?,
            &format!("{context} referrers"),
        )?,
        None => ReferrersMode::None,
    };
    Ok(InventoryEntry {
        id,
        source,
        destination,
        aliases,
        platforms,
        referrers,
    })
}

fn validate_id(id: &str, context: &str) -> Result<()> {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return Err(Error::Input(format!("{context}: entry id {id:?} is empty or too long")));
    }
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return Err(Error::Input(format!("{context}: entry id {id:?} must start with a lowercase letter or digit")));
    }
    if !bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_')) {
        return Err(Error::Input(format!("{context}: entry id {id:?} must be lowercase alphanumerics with '-', '_'")));
    }
    Ok(())
}

pub(crate) fn integer_field(table: &toml_edit::Table, key: &str, context: &str) -> Result<u64> {
    table
        .get(key)
        .and_then(|item| item.as_integer())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| Error::Input(format!("{context}: {key} must be a non-negative integer")))
}

pub(crate) fn string_field(table: &toml_edit::Table, key: &str, context: &str) -> Result<String> {
    table
        .get(key)
        .and_then(|item| item.as_str())
        .map(str::to_string)
        .ok_or_else(|| Error::Input(format!("{context}: {key} must be a string")))
}

pub(crate) fn string_array(table: &toml_edit::Table, key: &str, context: &str) -> Result<Vec<String>> {
    let Some(item) = table.get(key) else {
        return Ok(Vec::new());
    };
    let array = item.as_array().ok_or_else(|| {
        Error::Input(format!("{context}: {key} must be an array of strings"))
    })?;
    let mut out = Vec::with_capacity(array.len());
    for value in array.iter() {
        out.push(
            value.as_str()
                .map(str::to_string)
                .ok_or_else(|| Error::Input(format!("{context}: {key} must be an array of strings")))?,
        );
    }
    Ok(out)
}
