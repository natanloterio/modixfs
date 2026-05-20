use crate::manifest::{FileKind, InputKind, Manifest};

/// Generates a machine-readable `schema.json` for a tool manifest.
///
/// Format mirrors MCP's `list_tools` response so that MCP-aware clients and
/// scripts can consume LiveFoldersFS tool schemas without hand-parsing markdown.
pub fn generate_schema_json(manifest: &Manifest) -> String {
    let name = manifest.name.as_deref().unwrap_or("unknown");
    let description = manifest.description.as_deref().unwrap_or("");

    let endpoints: Vec<serde_json::Value> = manifest
        .files
        .iter()
        .filter(|s| matches!(s.kind, FileKind::WriteInvoke | FileKind::ReadInvoke))
        .map(|spec| {
            let mut ep = serde_json::json!({
                "name": spec.name,
                "kind": match spec.kind {
                    FileKind::WriteInvoke => "write_invoke",
                    FileKind::ReadInvoke  => "read_invoke",
                    _                     => "other",
                },
            });
            if let Some(ref schema) = spec.input {
                let type_str = match schema.kind {
                    InputKind::String => "string",
                    InputKind::Json   => "json",
                    InputKind::None   => "none",
                };
                let mut input_obj = serde_json::json!({ "type": type_str });
                if let Some(min) = schema.min_length {
                    input_obj["min_length"] = serde_json::json!(min);
                }
                if let Some(max) = schema.max_length {
                    input_obj["max_length"] = serde_json::json!(max);
                }
                if let Some(ref pat) = schema.pattern {
                    input_obj["pattern"] = serde_json::json!(pat);
                }
                if let Some(ref s) = schema.schema {
                    input_obj["schema"] = s.clone();
                }
                ep["input"] = input_obj;
            }
            if let Some(ref sf) = spec.state_file {
                ep["state_file"] = serde_json::json!(sf);
            }
            if let Some(ref stages) = spec.pipe {
                ep["pipe"] = serde_json::json!(stages);
            }
            ep
        })
        .collect();

    let doc = serde_json::json!({
        "name": name,
        "description": description,
        "endpoints": endpoints,
    });

    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// Generates an array of Anthropic-native tool objects for a tool manifest.
///
/// Each visible endpoint becomes one Anthropic tool with the name format
/// `{tool_name}__{endpoint_name}` (double underscore separator).
/// Hidden endpoints (spec.hidden == true) are excluded.
/// `write_invoke` endpoints get an `input` string parameter; `read_invoke` endpoints
/// have no parameters.
pub fn generate_anthropic_tools_json(tool_name: &str, manifest: &Manifest) -> String {
    let tools: Vec<serde_json::Value> = manifest
        .files
        .iter()
        .filter(|s| !s.hidden && matches!(s.kind, FileKind::WriteInvoke | FileKind::ReadInvoke))
        .map(|spec| {
            let name = format!("{}__{}", tool_name, spec.name);
            let description = spec.description.as_deref().unwrap_or("").to_string();
            let input_schema = match spec.kind {
                FileKind::WriteInvoke => serde_json::json!({
                    "type": "object",
                    "properties": {
                        "input": {"type": "string"}
                    },
                    "required": ["input"]
                }),
                FileKind::ReadInvoke => serde_json::json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                }),
                _ => unreachable!("filtered above"),
            };
            serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            })
        })
        .collect();

    serde_json::to_string_pretty(&tools).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{FileKind, FileSpec, InputKind, InputSchema, Manifest};

    fn spec(name: &str, kind: FileKind, input: Option<InputSchema>) -> FileSpec {
        FileSpec { name: name.into(), kind, handler: Some("cat".into()), input, state_file: None, pipe: None, description: None, hidden: false }
    }

    #[test]
    fn schema_json_contains_tool_name_and_description() {
        let m = Manifest {
            name: Some("search".into()),
            description: Some("Search things.".into()),
            version: None, env: vec![],
            files: vec![],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "search");
        assert_eq!(v["description"], "Search things.");
    }

    #[test]
    fn schema_json_lists_invokable_endpoints() {
        let m = Manifest {
            name: Some("demo".into()), description: None, version: None, env: vec![],
            files: vec![
                spec("query", FileKind::WriteInvoke, None),
                spec("status", FileKind::ReadInvoke, None),
                FileSpec { name: "notes.txt".into(), kind: FileKind::Passthrough, handler: None, input: None, state_file: None, pipe: None, description: None, hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let eps = v["endpoints"].as_array().unwrap();
        assert_eq!(eps.len(), 2, "passthrough should be excluded");
        assert_eq!(eps[0]["name"], "query");
        assert_eq!(eps[1]["name"], "status");
    }

    #[test]
    fn schema_json_includes_input_constraints() {
        let schema = InputSchema {
            kind: InputKind::Json,
            min_length: None,
            max_length: None,
            pattern: None,
            schema: Some(serde_json::json!({"required": ["q"]})),
        };
        let m = Manifest {
            name: Some("s".into()), description: None, version: None, env: vec![],
            files: vec![spec("search", FileKind::WriteInvoke, Some(schema))],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let input = &v["endpoints"][0]["input"];
        assert_eq!(input["type"], "json");
        assert_eq!(input["schema"]["required"][0], "q");
    }

    #[test]
    fn schema_json_includes_string_constraints() {
        let schema = InputSchema {
            kind: InputKind::String,
            min_length: Some(1),
            max_length: Some(100),
            pattern: Some(r"^\w+$".into()),
            schema: None,
        };
        let m = Manifest {
            name: Some("s".into()), description: None, version: None, env: vec![],
            files: vec![spec("greet", FileKind::WriteInvoke, Some(schema))],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let input = &v["endpoints"][0]["input"];
        assert_eq!(input["min_length"], 1);
        assert_eq!(input["max_length"], 100);
        assert_eq!(input["pattern"], r"^\w+$");
    }

    #[test]
    fn schema_json_includes_pipe_stages() {
        let m = Manifest {
            name: Some("t".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "fetch".into(), kind: FileKind::WriteInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: None, hidden: false },
                FileSpec { name: "format".into(), kind: FileKind::WriteInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: None, hidden: false },
                FileSpec { name: "pipeline".into(), kind: FileKind::WriteInvoke, handler: None, input: None, state_file: None,
                    pipe: Some(vec!["fetch".into(), "format".into()]), description: None, hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let eps = v["endpoints"].as_array().unwrap();
        let pipe_ep = eps.iter().find(|e| e["name"] == "pipeline").unwrap();
        assert_eq!(pipe_ep["pipe"][0], "fetch");
        assert_eq!(pipe_ep["pipe"][1], "format");
    }

    #[test]
    fn schema_json_excludes_passthrough_and_readonly() {
        let m = Manifest {
            name: Some("t".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "config.json".into(), kind: FileKind::Passthrough, handler: None, input: None, state_file: None, pipe: None, description: None, hidden: false },
                FileSpec { name: "readme.md".into(), kind: FileKind::Readonly, handler: None, input: None, state_file: None, pipe: None, description: None, hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_schema_json(&m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["endpoints"].as_array().unwrap().len(), 0);
    }

    // ── generate_anthropic_tools_json tests ────────────────────────────────────

    #[test]
    fn anthropic_tools_hidden_endpoints_are_excluded() {
        let m = Manifest {
            name: Some("users".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "count".into(), kind: FileKind::ReadInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: None, hidden: false },
                FileSpec { name: "internal".into(), kind: FileKind::ReadInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: None, hidden: true },
            ],
            ..Default::default()
        };
        let out = generate_anthropic_tools_json("users", &m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let tools = v.as_array().unwrap();
        assert_eq!(tools.len(), 1, "hidden endpoint should be excluded");
        assert_eq!(tools[0]["name"], "users__count");
    }

    #[test]
    fn anthropic_tools_read_invoke_has_no_parameters() {
        let m = Manifest {
            name: Some("users".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "count".into(), kind: FileKind::ReadInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: Some("integer count of all users".into()), hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_anthropic_tools_json("users", &m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let tool = &v[0];
        assert_eq!(tool["name"], "users__count");
        assert_eq!(tool["description"], "integer count of all users");
        let props = &tool["input_schema"]["properties"];
        assert!(props.as_object().unwrap().is_empty(), "read_invoke should have no properties");
        let required = &tool["input_schema"]["required"];
        assert!(required.as_array().unwrap().is_empty(), "read_invoke should have no required fields");
    }

    #[test]
    fn anthropic_tools_write_invoke_has_input_string_parameter() {
        let m = Manifest {
            name: Some("users".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "search".into(), kind: FileKind::WriteInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: Some("find user by name".into()), hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_anthropic_tools_json("users", &m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let tool = &v[0];
        assert_eq!(tool["name"], "users__search");
        assert_eq!(tool["input_schema"]["properties"]["input"]["type"], "string");
        assert_eq!(tool["input_schema"]["required"][0], "input");
    }

    #[test]
    fn anthropic_tools_name_format_uses_double_underscore() {
        let m = Manifest {
            name: Some("myservice".into()), description: None, version: None, env: vec![],
            files: vec![
                FileSpec { name: "ep".into(), kind: FileKind::ReadInvoke, handler: Some("cat".into()), input: None, state_file: None, pipe: None, description: None, hidden: false },
            ],
            ..Default::default()
        };
        let out = generate_anthropic_tools_json("myservice", &m);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v[0]["name"], "myservice__ep");
    }
}
