use crate::error::Result;
use deser::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEntry {
    pub name: String,
    pub version: String,
    pub checksum: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pins {
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub project: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub lock_path: Option<String>,
    #[deser(default, skip_serializing_if = Option::is_none)]
    pub lock_content: Option<String>,
    #[deser(default)]
    pub registry: Vec<PinEntry>,
    #[deser(default)]
    pub unsupported: Vec<String>,
}

pub fn write_pins(pins_path: &Path, pins: &Pins) -> Result<()> {
    if let Some(p) = pins_path.parent() {
        fs::create_dir_all(p)?;
    }
    let s = crate::toml::to_string(pins).map_err(|e| crate::error::Error::Operation(e.to_string()))?;
    fs::write(pins_path, s)?;
    Ok(())
}
