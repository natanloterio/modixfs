use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    WriteInvoke,
    ReadInvoke,
    Passthrough,
    Readonly,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FileKind,
    pub handler: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvDecl>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
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
        let path = tool_dir.join("folder.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let manifest = serde_yaml::from_str(&content)?;
        Ok(Some(manifest))
    }

    pub fn spec_for(&self, name: &str) -> Option<&FileSpec> {
        self.files.iter().find(|spec| spec.name == name)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for spec in &self.files {
            match spec.kind {
                FileKind::WriteInvoke | FileKind::ReadInvoke => {
                    if spec.handler.is_none() {
                        anyhow::bail!(
                            "file '{}' has kind {:?} but no handler specified",
                            spec.name,
                            spec.kind
                        );
                    }
                }
                FileKind::Passthrough | FileKind::Readonly => {
                    if spec.handler.is_some() {
                        anyhow::bail!(
                            "file '{}' has kind {:?} but specifies a handler (not allowed)",
                            spec.name,
                            spec.kind
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_files_section() {
        let yaml = r#"
name: weather
files:
  - name: forecast
    type: read_invoke
    handler: ./bin/forecast
  - name: search
    type: write_invoke
    handler: "curl -s -X POST -d @- https://api.example.com/search"
  - name: config.json
    type: passthrough
  - name: how_to.md
    type: readonly
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.files.len(), 4);
        let forecast = m.spec_for("forecast").unwrap();
        assert_eq!(forecast.kind, FileKind::ReadInvoke);
        assert_eq!(forecast.handler.as_deref(), Some("./bin/forecast"));
        let config = m.spec_for("config.json").unwrap();
        assert_eq!(config.kind, FileKind::Passthrough);
        assert!(config.handler.is_none());
        assert!(m.spec_for("nonexistent").is_none());
    }

    #[test]
    fn validate_rejects_handler_on_passthrough() {
        let yaml = "files:\n  - name: config.json\n    type: passthrough\n    handler: ./something\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_handler_on_write_invoke() {
        let yaml = "files:\n  - name: search\n    type: write_invoke\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_manifest() {
        let yaml = "files:\n  - name: search\n    type: write_invoke\n    handler: ./bin/search\n  - name: config.json\n    type: passthrough\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_ok());
    }

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
    fn load_from_dir_reads_livefolders_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(tmp.path().join("folder.yaml")).unwrap();
        writeln!(f, "name: testpkg\nversion: 1.0.0").unwrap();
        let m = Manifest::load(tmp.path()).unwrap().unwrap();
        assert_eq!(m.name.as_deref(), Some("testpkg"));
    }

    #[test]
    fn validate_rejects_missing_handler_on_read_invoke() {
        let yaml = "files:\n  - name: status\n    type: read_invoke\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_rejects_handler_on_readonly() {
        let yaml = "files:\n  - name: readme.md\n    type: readonly\n    handler: ./something\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_accepts_read_invoke_with_handler() {
        let yaml = "files:\n  - name: status\n    type: read_invoke\n    handler: date\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn validate_accepts_readonly_without_handler() {
        let yaml = "files:\n  - name: readme.md\n    type: readonly\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn spec_for_returns_none_on_empty_files() {
        let m: Manifest = serde_yaml::from_str("name: empty\n").unwrap();
        assert!(m.spec_for("anything").is_none());
    }
}
