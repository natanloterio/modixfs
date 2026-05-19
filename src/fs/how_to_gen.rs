use crate::manifest::{FileKind, InputKind, Manifest};

pub fn generate_how_to(manifest: &Manifest) -> String {
    let mut out = String::new();
    let name = manifest.name.as_deref().unwrap_or("this tool");
    out.push_str(&format!("# {}\n\n", name));
    if let Some(desc) = &manifest.description {
        out.push_str(&format!("{}\n\n", desc));
    }
    if !manifest.files.is_empty() {
        out.push_str("## Files\n\n");
        for spec in &manifest.files {
            let kind_str = match spec.kind {
                FileKind::WriteInvoke => "write_invoke",
                FileKind::ReadInvoke => "read_invoke",
                FileKind::Passthrough => "passthrough",
                FileKind::Readonly => "readonly",
            };
            out.push_str(&format!("- **{}** (`{}`)", spec.name, kind_str));
            if let Some(h) = &spec.handler {
                out.push_str(&format!(" — handler: `{}`", h));
            }
            if let Some(ref schema) = spec.input {
                let type_str = match schema.kind {
                    InputKind::String => "plain text",
                    InputKind::Json => "JSON",
                    InputKind::None => "no payload",
                };
                out.push_str(&format!(", input: {}", type_str));
                if let Some(min) = schema.min_length {
                    out.push_str(&format!(", min_length: {}", min));
                }
                if let Some(max) = schema.max_length {
                    out.push_str(&format!(", max_length: {}", max));
                }
                if let Some(ref pat) = schema.pattern {
                    out.push_str(&format!(", pattern: `{}`", pat));
                }
                if let Some(ref json_schema) = schema.schema {
                    out.push_str(&format!(", schema: `{}`", json_schema));
                }
            }
            if let Some(ref sf) = spec.state_file {
                out.push_str(&format!(", state_file: `{}`", sf));
            }
            if let Some(ref stages) = spec.pipe {
                out.push_str(&format!(", pipe: [{}]", stages.join(", ")));
            }
            // Invokable endpoints expose a companion .log file after first call.
            if matches!(spec.kind, FileKind::WriteInvoke | FileKind::ReadInvoke) {
                out.push_str(&format!(" → read `{}.log` for last invocation timing and stderr", spec.name));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if !manifest.env.is_empty() {
        out.push_str("## Required secrets\n\n");
        for e in &manifest.env {
            out.push_str(&format!("- `{}`", e.name));
            if let Some(desc) = &e.description {
                out.push_str(&format!(" — {}", desc));
            }
            if e.required {
                out.push_str(" *(required)*");
            } else if let Some(default) = &e.default {
                out.push_str(&format!(" (default: `{}`)", default));
            }
            out.push('\n');
        }
    }
    out.push_str("\nRead `schema.json` for a machine-readable version of the endpoint schemas.\n");
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{EnvDecl, FileSpec, Manifest};

    fn make_manifest(name: &str, desc: Option<&str>, files: Vec<FileSpec>, env: Vec<EnvDecl>) -> Manifest {
        Manifest {
            name: Some(name.into()),
            description: desc.map(|s| s.into()),
            version: None,
            env,
            files,
            ..Default::default()
        }
    }

    #[test]
    fn generate_includes_name_and_description() {
        let m = make_manifest("weather", Some("Get the weather forecast."), vec![], vec![]);
        let out = generate_how_to(&m);
        assert!(out.contains("weather"));
        assert!(out.contains("Get the weather forecast."));
    }

    #[test]
    fn generate_includes_env_defaults() {
        use crate::manifest::EnvDecl;
        let m = Manifest {
            name: Some("mytool".into()),
            description: None,
            version: None,
            env: vec![EnvDecl {
                name: "TIMEOUT".into(),
                description: Some("Seconds".into()),
                required: false,
                default: Some("30".into()),
            }],
            files: vec![],
            ..Default::default()
        };
        let out = generate_how_to(&m);
        assert!(out.contains("TIMEOUT"));
        assert!(out.contains("30"));
    }

    #[test]
    fn generate_lists_file_specs() {
        let m = make_manifest("demo", None, vec![
            FileSpec { name: "forecast".into(), kind: FileKind::ReadInvoke, handler: Some("date".into()), input: None, state_file: None, pipe: None },
            FileSpec { name: "notes.txt".into(), kind: FileKind::Passthrough, handler: None, input: None, state_file: None, pipe: None },
        ], vec![]);
        let out = generate_how_to(&m);
        assert!(out.contains("forecast"));
        assert!(out.contains("read_invoke"));
        assert!(out.contains("notes.txt"));
        assert!(out.contains("passthrough"));
    }

    #[test]
    fn how_to_mentions_json_input_schema() {
        use crate::manifest::{FileKind, FileSpec, InputKind, InputSchema, Manifest};
        let manifest = Manifest {
            name: Some("search".to_string()),
            description: Some("Search the web.".to_string()),
            version: None,
            env: vec![],
            files: vec![FileSpec {
                name: "query".to_string(),
                kind: FileKind::WriteInvoke,
                handler: Some("./search.sh".to_string()),
                input: Some(InputSchema::of_kind(InputKind::Json)),
                state_file: None,
                pipe: None,
            }],
            ..Default::default()
        };
        let output = generate_how_to(&manifest);
        assert!(output.contains("JSON"), "expected JSON mention, got:\n{}", output);
    }

    #[test]
    fn how_to_surfaces_string_constraints() {
        use crate::manifest::{FileKind, FileSpec, InputKind, InputSchema, Manifest};
        let manifest = Manifest {
            name: Some("greet".to_string()),
            description: None,
            version: None,
            env: vec![],
            files: vec![FileSpec {
                name: "say".to_string(),
                kind: FileKind::WriteInvoke,
                handler: Some("cat".to_string()),
                input: Some(InputSchema {
                    kind: InputKind::String,
                    min_length: Some(1),
                    max_length: Some(80),
                    pattern: Some(r"^[a-z]+$".to_string()),
                    schema: None,
                }),
                state_file: None,
                pipe: None,
            }],
            ..Default::default()
        };
        let output = generate_how_to(&manifest);
        assert!(output.contains("min_length: 1"), "got:\n{}", output);
        assert!(output.contains("max_length: 80"), "got:\n{}", output);
        assert!(output.contains("^[a-z]+$"), "got:\n{}", output);
    }

    #[test]
    fn how_to_surfaces_json_schema() {
        use crate::manifest::{FileKind, FileSpec, InputKind, InputSchema, Manifest};
        let manifest = Manifest {
            name: Some("search".to_string()),
            description: None,
            version: None,
            env: vec![],
            files: vec![FileSpec {
                name: "query".to_string(),
                kind: FileKind::WriteInvoke,
                handler: Some("cat".to_string()),
                input: Some(InputSchema {
                    kind: InputKind::Json,
                    min_length: None,
                    max_length: None,
                    pattern: None,
                    schema: Some(serde_json::json!({"required": ["q"]})),
                }),
                state_file: None,
                pipe: None,
            }],
            ..Default::default()
        };
        let output = generate_how_to(&manifest);
        assert!(output.contains("schema:"), "got:\n{}", output);
        assert!(output.contains("required"), "got:\n{}", output);
    }
}
