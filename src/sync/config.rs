use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted configuration for a sync watch folder.
/// Stored as `.backpack-sync.toml` in the root of the watched directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub watch_dir: String,
    pub server_url: String,
    #[serde(default)]
    pub space_token: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
}

fn default_poll_interval() -> u64 { 30 }
fn default_debounce_ms() -> u64 { 500 }
fn default_max_concurrency() -> usize { 4 }

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            watch_dir: String::new(),
            server_url: String::new(),
            space_token: None,
            poll_interval_secs: default_poll_interval(),
            ignore_patterns: Vec::new(),
            debounce_ms: default_debounce_ms(),
            max_concurrency: default_max_concurrency(),
        }
    }
}

impl SyncConfig {
    pub const CONFIG_FILE_NAME: &'static str = ".backpack-sync.toml";

    pub fn load_from_dir(dir: &str) -> Result<Self> {
        let path = PathBuf::from(dir).join(Self::CONFIG_FILE_NAME);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config at {}", path.display()))?;
        let mut config: SyncConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config at {}", path.display()))?;
        if !std::path::Path::new(&config.watch_dir).is_absolute() {
            config.watch_dir = std::fs::canonicalize(dir)
                .unwrap_or_else(|_| PathBuf::from(dir))
                .to_string_lossy()
                .to_string();
        }
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = PathBuf::from(&self.watch_dir).join(Self::CONFIG_FILE_NAME);
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write config to {}", path.display()))?;
        Ok(())
    }
}
