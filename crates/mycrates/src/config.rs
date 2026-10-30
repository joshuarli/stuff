use crate::error::{Error, Result};
use deser::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[deser(default)]
pub struct UpstreamConfig {
    pub index_url: String,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        Self {
            index_url: "https://index.crates.io/".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[deser(default)]
pub struct LimitsConfig {
    pub max_compressed: u64,
    pub max_expanded: u64,
    pub max_entries: usize,
    pub max_ratio: u64,
    pub concurrent_requests: usize,
    pub fetch_bytes: u64,
    pub fetch_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub redirects: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_compressed: 1024 * 1024 * 1024,
            max_expanded: 4 * 1024 * 1024 * 1024,
            max_entries: 200_000,
            max_ratio: 1000,
            concurrent_requests: 4,
            fetch_bytes: 10 * 1024 * 1024 * 1024,
            fetch_timeout_secs: 3600,
            request_timeout_secs: 300,
            redirects: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[deser(default)]
pub struct Config {
    pub version: u32,
    pub public_base_url: String,
    pub upstream: UpstreamConfig,
    pub limits: LimitsConfig,
    pub auth_required: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            public_base_url: "http://localhost:8080".to_string(),
            upstream: UpstreamConfig::default(),
            limits: LimitsConfig::default(),
            auth_required: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(path)?;
            return Ok(cfg);
        }
        let s = fs::read_to_string(path).map_err(|e| Error::Operation(e.to_string()))?;
        let cfg: Config = crate::json::from_str(&s).map_err(|e| Error::Input(e.to_string()))?;
        if cfg.version != 1 {
            return Err(Error::Input(format!("unsupported config version {}", cfg.version)));
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).map_err(|e| Error::Operation(e.to_string()))?;
        }
        let s = crate::json::to_string_pretty(self).map_err(|e| Error::Operation(e.to_string()))?;
        fs::write(path, s).map_err(|e| Error::Operation(e.to_string()))?;
        Ok(())
    }
}
