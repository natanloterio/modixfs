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

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    String,
    Json,
    None,
}

#[derive(Debug, Deserialize, Clone)]
pub struct InputSchema {
    #[serde(rename = "type")]
    pub kind: InputKind,
    /// Minimum character count (string inputs only).
    #[serde(default)]
    pub min_length: Option<usize>,
    /// Maximum character count (string inputs only).
    #[serde(default)]
    pub max_length: Option<usize>,
    /// Regex pattern the full input must match (string inputs only).
    #[serde(default)]
    pub pattern: Option<String>,
    /// JSON Schema subset (json inputs only): supports `required` and `properties[*].type`.
    #[serde(default)]
    pub schema: Option<serde_json::Value>,
}

impl InputSchema {
    pub fn of_kind(kind: InputKind) -> Self {
        Self { kind, min_length: None, max_length: None, pattern: None, schema: None }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct FileSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FileKind,
    pub handler: Option<String>,
    #[serde(default)]
    pub input: Option<InputSchema>,
    /// Path to a state file (relative to the tool directory).
    /// The runtime holds an exclusive advisory lock on this file for the entire
    /// duration of each handler invocation and passes its resolved path as the
    /// `LIVEFOLDERS_STATE_FILE` environment variable.  Concurrent invocations
    /// of the same endpoint are serialised automatically.
    #[serde(default)]
    pub state_file: Option<String>,
    /// Ordered list of endpoint names to chain.  The stdout of each stage
    /// becomes the stdin of the next.  When set, `handler` must be absent.
    #[serde(default)]
    pub pipe: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxFsSpec {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxResourceSpec {
    #[serde(default)]
    pub max_procs: Option<u64>,
    #[serde(default)]
    pub max_memory_mb: Option<u64>,
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
pub struct SandboxSpec {
    #[serde(default)]
    pub fs: SandboxFsSpec,
    /// None = use global default (deny network). Some(true) = allow network.
    #[serde(default)]
    pub network: Option<bool>,
    #[serde(default)]
    pub resources: SandboxResourceSpec,
}

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    pub name: Option<String>,
    pub description: Option<String>,
    #[allow(dead_code)]
    pub version: Option<String>,
    #[serde(default)]
    pub env: Vec<EnvDecl>,
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub sandbox: Option<SandboxSpec>,
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
                    match (&spec.handler, &spec.pipe) {
                        (None, None) => anyhow::bail!(
                            "file '{}' has kind {:?} but neither handler nor pipe specified",
                            spec.name, spec.kind
                        ),
                        (Some(_), Some(_)) => anyhow::bail!(
                            "file '{}' specifies both handler and pipe; use one or the other",
                            spec.name
                        ),
                        _ => {}
                    }
                    if let Some(stages) = &spec.pipe {
                        if stages.is_empty() {
                            anyhow::bail!("file '{}' pipe must contain at least one stage", spec.name);
                        }
                        for stage in stages {
                            if self.spec_for(stage).is_none() {
                                anyhow::bail!(
                                    "file '{}' pipe references unknown endpoint '{}'",
                                    spec.name, stage
                                );
                            }
                        }
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

    #[test]
    fn parse_file_spec_with_json_input_schema() {
        let yaml = r#"
files:
  - name: search
    type: write_invoke
    handler: ./search.sh
    input:
      type: json
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("search").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(matches!(schema.kind, InputKind::Json));
    }

    #[test]
    fn parse_file_spec_with_none_input_schema() {
        let yaml = r#"
files:
  - name: status
    type: read_invoke
    handler: ./status.sh
    input:
      type: none
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("status").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(matches!(schema.kind, InputKind::None));
    }

    #[test]
    fn parse_file_spec_with_string_input_schema() {
        let yaml = r#"
files:
  - name: echo
    type: write_invoke
    handler: cat
    input:
      type: string
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("echo").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(matches!(schema.kind, InputKind::String));
    }

    #[test]
    fn parse_file_spec_without_input_schema_is_none() {
        let yaml = "files:\n  - name: search\n    type: write_invoke\n    handler: ./search.sh\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("search").unwrap();
        assert!(spec.input.is_none());
    }

    #[test]
    fn parse_string_constraints_from_yaml() {
        let yaml = r#"
files:
  - name: greet
    type: write_invoke
    handler: cat
    input:
      type: string
      min_length: 2
      max_length: 50
      pattern: "^[a-z]+$"
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("greet").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(matches!(schema.kind, InputKind::String));
        assert_eq!(schema.min_length, Some(2));
        assert_eq!(schema.max_length, Some(50));
        assert_eq!(schema.pattern.as_deref(), Some("^[a-z]+$"));
    }

    #[test]
    fn parse_json_schema_constraint_from_yaml() {
        let yaml = r#"
files:
  - name: search
    type: write_invoke
    handler: cat
    input:
      type: json
      schema:
        required: [query]
        properties:
          query:
            type: string
          limit:
            type: number
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("search").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(matches!(schema.kind, InputKind::Json));
        let json_schema = schema.schema.as_ref().unwrap();
        let required = json_schema["required"].as_array().unwrap();
        assert_eq!(required[0].as_str(), Some("query"));
        assert_eq!(json_schema["properties"]["limit"]["type"].as_str(), Some("number"));
    }

    #[test]
    fn parse_pipe_field_from_yaml() {
        let yaml = r#"
files:
  - name: fetch
    type: write_invoke
    handler: ./fetch.sh
  - name: format
    type: write_invoke
    handler: ./format.sh
  - name: weather_report
    type: write_invoke
    pipe: [fetch, format]
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("weather_report").unwrap();
        assert!(spec.handler.is_none());
        let stages = spec.pipe.as_ref().unwrap();
        assert_eq!(stages, &["fetch", "format"]);
    }

    #[test]
    fn validate_accepts_pipe_endpoint_without_handler() {
        let yaml = r#"
files:
  - name: fetch
    type: write_invoke
    handler: ./fetch.sh
  - name: pipeline
    type: write_invoke
    pipe: [fetch]
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn validate_rejects_pipe_and_handler_together() {
        let yaml = r#"
files:
  - name: a
    type: write_invoke
    handler: ./a.sh
    pipe: [a]
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_rejects_pipe_referencing_unknown_stage() {
        let yaml = r#"
files:
  - name: pipeline
    type: write_invoke
    pipe: [nonexistent]
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let err = m.validate().unwrap_err().to_string();
        assert!(err.contains("nonexistent"), "got: {}", err);
    }

    #[test]
    fn validate_rejects_empty_pipe() {
        let yaml = "files:\n  - name: pipeline\n    type: write_invoke\n    pipe: []\n";
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn parse_input_schema_defaults_constraints_to_none() {
        let yaml = r#"
files:
  - name: status
    type: read_invoke
    handler: date
    input:
      type: string
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        let spec = m.spec_for("status").unwrap();
        let schema = spec.input.as_ref().unwrap();
        assert!(schema.min_length.is_none());
        assert!(schema.max_length.is_none());
        assert!(schema.pattern.is_none());
        assert!(schema.schema.is_none());
    }
}

#[cfg(test)]
mod sandbox_tests {
    use super::*;

    #[test]
    fn sandbox_spec_deserializes_from_yaml() {
        let yaml = r#"
name: mytool
sandbox:
  fs:
    read: ["/usr", "/lib"]
    write: ["./cache"]
  network: false
  resources:
    max_procs: 4
    max_memory_mb: 128
"#;
        let manifest: Manifest = serde_yaml::from_str(yaml).unwrap();
        let sandbox = manifest.sandbox.unwrap();
        assert_eq!(sandbox.fs.read, vec!["/usr", "/lib"]);
        assert_eq!(sandbox.fs.write, vec!["./cache"]);
        assert_eq!(sandbox.network, Some(false));
        assert_eq!(sandbox.resources.max_procs, Some(4));
        assert_eq!(sandbox.resources.max_memory_mb, Some(128));
    }

    #[test]
    fn sandbox_absent_yields_none() {
        let yaml = "name: mytool\n";
        let manifest: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(manifest.sandbox.is_none());
    }

    #[test]
    fn sandbox_network_defaults_to_none_when_omitted() {
        let yaml = r#"name: mytool
sandbox:
  fs:
    read: []
"#;
        let manifest: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(manifest.sandbox.unwrap().network, None);
    }
}
