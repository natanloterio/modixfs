use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mount: Option<PathBuf>,

    pub tools_dir: Option<PathBuf>,

    #[serde(default = "default_timeout", rename = "timeout")]
    pub timeout_secs: u64,

    #[serde(default)]
    pub tools: Vec<ToolConfig>,
}

fn default_timeout() -> u64 { 30 }

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

    pub fn default_config() -> Self {
        Self {
            mount: None,
            tools_dir: None,
            timeout_secs: default_timeout(),
            tools: vec![ToolConfig { name: "echo".to_string(), token_env: None }],
        }
    }

    pub fn resolved_tools_dir(&self) -> anyhow::Result<Option<PathBuf>> {
        match &self.tools_dir {
            None => Ok(None),
            Some(p) => {
                let s = p.to_string_lossy();
                if s.starts_with("~/") {
                    let home = std::env::var("HOME")
                        .context("tools_dir starts with '~/' but $HOME is not set")?;
                    Ok(Some(PathBuf::from(home).join(&s[2..])))
                } else {
                    Ok(Some(p.clone()))
                }
            }
        }
    }
}
