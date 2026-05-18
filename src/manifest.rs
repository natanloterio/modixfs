use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvDecl>,
}

#[derive(Debug, Deserialize)]
pub struct EnvDecl {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
}

impl Manifest {
    pub fn load(tool_dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = tool_dir.join("modix.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let manifest = serde_yaml::from_str(&content)?;
        Ok(Some(manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_full_manifest() {
        let yaml = r#"
name: mytool
description: Does something useful
version: 0.2.0
env:
  - name: MYTOOL_KEY
    description: API key
    required: true
  - name: MYTOOL_TIMEOUT
    description: Timeout
    required: false
    default: "30"
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name.as_deref(), Some("mytool"));
        assert_eq!(m.description.as_deref(), Some("Does something useful"));
        assert_eq!(m.env.len(), 2);
        assert!(m.env[0].required);
        assert!(!m.env[1].required);
        assert_eq!(m.env[1].default.as_deref(), Some("30"));
    }

    #[test]
    fn parse_minimal_manifest() {
        let yaml = "name: minimal\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.name.as_deref(), Some("minimal"));
        assert!(m.env.is_empty());
    }

    #[test]
    fn load_from_dir_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = Manifest::load(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn load_from_dir_reads_modix_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("modix.yaml")).unwrap();
        writeln!(f, "name: testpkg\nversion: 1.0.0").unwrap();
        let m = Manifest::load(tmp.path()).unwrap().unwrap();
        assert_eq!(m.name.as_deref(), Some("testpkg"));
    }
}
