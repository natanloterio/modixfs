use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Default mountpoint; overridden by the CLI argument if provided.
    pub mount: Option<PathBuf>,

    #[serde(default)]
    pub tools: Vec<ToolConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ToolConfig {
    pub name: String,

    /// Name of the environment variable holding the API token.
    /// Defaults to "<NAME>_TOKEN" (uppercased tool name).
    pub token_env: Option<String>,
}

impl ToolConfig {
    pub fn resolve_token(&self) -> Option<String> {
        let env_var = self
            .token_env
            .clone()
            .unwrap_or_else(|| format!("{}_TOKEN", self.name.to_uppercase()));
        std::env::var(&env_var).ok()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        serde_yaml::from_str(&content)
            .with_context(|| format!("parsing config file {}", path.display()))
    }

    /// Returns a minimal default config (echo only, no mount override).
    pub fn default_config() -> Self {
        Self {
            mount: None,
            tools: vec![ToolConfig { name: "echo".to_string(), token_env: None }],
        }
    }
}
