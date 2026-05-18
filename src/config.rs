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
            tools: vec![ToolConfig { name: "echo".to_string() }],
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn resolved_tools_dir_none_when_unset() {
        let cfg = Config { mount: None, tools_dir: None, timeout_secs: 30, tools: vec![] };
        assert!(cfg.resolved_tools_dir().unwrap().is_none());
    }

    #[test]
    fn resolved_tools_dir_absolute_path_unchanged() {
        let cfg = Config {
            mount: None,
            tools_dir: Some(PathBuf::from("/opt/tools")),
            timeout_secs: 30,
            tools: vec![],
        };
        assert_eq!(cfg.resolved_tools_dir().unwrap(), Some(PathBuf::from("/opt/tools")));
    }

    #[test]
    fn resolved_tools_dir_expands_tilde() {
        let home = std::env::var("HOME").unwrap();
        let cfg = Config {
            mount: None,
            tools_dir: Some(PathBuf::from("~/.config/livefolders/tools")),
            timeout_secs: 30,
            tools: vec![],
        };
        let expected = PathBuf::from(&home).join(".config/livefolders/tools");
        assert_eq!(cfg.resolved_tools_dir().unwrap(), Some(expected));
    }

    #[test]
    fn load_parses_timeout_and_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tools.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "timeout: 60\ntools:\n  - name: echo\n  - name: custom\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.timeout_secs, 60);
        assert_eq!(cfg.tools.len(), 2);
        assert_eq!(cfg.tools[0].name, "echo");
        assert_eq!(cfg.tools[1].name, "custom");
    }

    #[test]
    fn load_uses_default_timeout_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tools.yaml");
        std::fs::write(&path, "mount: /tmp/lf\n").unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.timeout_secs, 30);
    }

    #[test]
    fn load_returns_error_for_missing_file() {
        let result = Config::load(Path::new("/nonexistent/tools.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn default_config_has_echo_tool() {
        let cfg = Config::default_config();
        assert_eq!(cfg.tools.len(), 1);
        assert_eq!(cfg.tools[0].name, "echo");
        assert_eq!(cfg.timeout_secs, 30);
    }
}
