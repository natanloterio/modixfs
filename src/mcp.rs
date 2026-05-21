//! MCP (Model Context Protocol) server over stdio.
//!
//! Exposes LiveFolders tools as MCP tools. Reads `<mount>/tools/` to discover
//! available tools and their endpoints via the FUSE-synthesized `schema.json`.
//!
//! JSON-RPC 2.0 — newline-delimited, no Content-Length framing.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

/// Entry point called from `main.rs`.
pub fn run(mount: PathBuf) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in BufReader::new(stdin.lock()).lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("mcp: stdin read error: {}", e);
                break;
            }
        };

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let resp = json_rpc_error(Value::Null, -32700, &format!("Parse error: {}", e));
                writeln!(out, "{}", resp)?;
                out.flush()?;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req["method"].as_str().unwrap_or("");
        let is_notification = req.get("id").is_none();

        let response = match method {
            "initialize" => Some(handle_initialize(&id)),
            "notifications/initialized" | "notifications/cancelled" => None,
            "tools/list" => Some(handle_tools_list(&id, &mount)),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                Some(handle_tools_call(&id, &mount, &params))
            }
            other if is_notification => {
                tracing::debug!("mcp: ignoring notification: {}", other);
                None
            }
            other => {
                tracing::warn!("mcp: unknown method: {}", other);
                Some(json_rpc_error(id, -32601, &format!("Method not found: {}", other)))
            }
        };

        if let Some(resp) = response {
            writeln!(out, "{}", resp)?;
            out.flush()?;
        }
    }

    Ok(())
}

// ── Protocol handlers ──────────────────────────────────────────────────────────

fn handle_initialize(id: &Value) -> Value {
    json_rpc_ok(id.clone(), json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": {
            "name": "livefolders",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "capabilities": {
            "tools": {}
        }
    }))
}

fn handle_tools_list(id: &Value, mount: &Path) -> Value {
    match list_tools(mount) {
        Ok(tools) => json_rpc_ok(id.clone(), json!({ "tools": tools })),
        Err(e) => json_rpc_error(id.clone(), -32603, &format!("Internal error: {}", e)),
    }
}

fn handle_tools_call(id: &Value, mount: &Path, params: &Value) -> Value {
    let tool_name = match params["name"].as_str() {
        Some(n) => n.to_string(),
        None => return json_rpc_error(id.clone(), -32602, "Missing 'name' in params"),
    };
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    match call_tool(mount, &tool_name, &arguments) {
        Ok(output) => json_rpc_ok(id.clone(), json!({
            "content": [{"type": "text", "text": output}],
            "isError": false,
        })),
        Err(e) => json_rpc_ok(id.clone(), json!({
            "content": [{"type": "text", "text": format!("[ERROR] {}", e)}],
            "isError": true,
        })),
    }
}

// ── Tool discovery ─────────────────────────────────────────────────────────────

/// Enumerate `<mount>/tools/` and return MCP tool descriptors, one per endpoint.
pub fn list_tools(mount: &Path) -> Result<Vec<Value>> {
    let tools_dir = mount.join("tools");
    if !tools_dir.is_dir() {
        anyhow::bail!(
            "livefolders is not mounted at '{}' — run 'livefolders mount' first",
            mount.display()
        );
    }

    let mut entries: Vec<_> = std::fs::read_dir(&tools_dir)?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut result = Vec::new();

    for entry in entries {
        let tool_dir = entry.path();
        let tool_name = entry.file_name().to_string_lossy().to_string();
        let schema_path = tool_dir.join("schema.json");

        let schema_str = match std::fs::read_to_string(&schema_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("mcp: skipping {}: cannot read schema.json: {}", tool_name, e);
                continue;
            }
        };

        let schema: Value = match serde_json::from_str(&schema_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("mcp: skipping {}: invalid schema.json: {}", tool_name, e);
                continue;
            }
        };

        let endpoints = match schema["endpoints"].as_array() {
            Some(eps) => eps,
            None => continue,
        };

        for ep in endpoints {
            let ep_name = match ep["name"].as_str() {
                Some(n) => n,
                None => continue,
            };
            let ep_kind = ep["kind"].as_str().unwrap_or("");
            if ep_kind != "write_invoke" && ep_kind != "read_invoke" {
                continue;
            }

            let description = ep["description"].as_str()
                .or_else(|| schema["description"].as_str())
                .unwrap_or("")
                .to_string();

            let input_schema = if ep_kind == "write_invoke" {
                json!({
                    "type": "object",
                    "properties": { "input": {"type": "string"} },
                    "required": ["input"]
                })
            } else {
                json!({
                    "type": "object",
                    "properties": {},
                    "required": []
                })
            };

            result.push(json!({
                "name": format!("{}__{}", tool_name, ep_name),
                "description": description,
                "inputSchema": input_schema,
            }));
        }
    }

    Ok(result)
}

// ── Tool invocation ────────────────────────────────────────────────────────────

/// Invoke a LiveFolders tool by its MCP name (`toolname__endpoint`).
///
/// For `write_invoke`: writes `arguments["input"]` to the endpoint file then reads the result.
/// For `read_invoke`: reads the endpoint file directly.
pub fn call_tool(mount: &Path, mcp_name: &str, arguments: &Value) -> Result<String> {
    let (tool_name, ep_name) = split_tool_name(mcp_name)?;

    let tools_dir = mount.join("tools");
    if !tools_dir.is_dir() {
        anyhow::bail!(
            "livefolders is not mounted at '{}' — run 'livefolders mount' first",
            mount.display()
        );
    }

    let schema_path = tools_dir.join(tool_name).join("schema.json");
    let schema_str = std::fs::read_to_string(&schema_path)
        .with_context(|| format!("reading schema.json for '{}'", tool_name))?;
    let schema: Value = serde_json::from_str(&schema_str)?;

    let ep_kind = schema["endpoints"].as_array()
        .and_then(|eps| eps.iter().find(|e| e["name"].as_str() == Some(ep_name)))
        .and_then(|ep| ep["kind"].as_str())
        .unwrap_or("read_invoke");

    let ep_path = tools_dir.join(tool_name).join(ep_name);

    if ep_kind == "write_invoke" {
        let input = arguments["input"].as_str().unwrap_or("");
        std::fs::write(&ep_path, input.as_bytes())
            .with_context(|| format!("writing to {}", ep_path.display()))?;
    }

    std::fs::read_to_string(&ep_path)
        .with_context(|| format!("reading from {}", ep_path.display()))
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Split `"toolname__endpoint"` on the first `__`, returning `(tool, endpoint)`.
fn split_tool_name(mcp_name: &str) -> Result<(&str, &str)> {
    mcp_name.find("__")
        .map(|i| (&mcp_name[..i], &mcp_name[i + 2..]))
        .ok_or_else(|| anyhow::anyhow!(
            "invalid tool name '{}': expected 'toolname__endpoint'", mcp_name
        ))
}

fn json_rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message} })
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_schema(dir: &Path, schema: &Value) {
        fs::write(dir.join("schema.json"), serde_json::to_string(schema).unwrap()).unwrap();
    }

    // ── split_tool_name ──────────────────────────────────────────────────────────

    #[test]
    fn split_on_double_underscore() {
        let (t, e) = split_tool_name("hackernews__top_stories").unwrap();
        assert_eq!(t, "hackernews");
        assert_eq!(e, "top_stories");
    }

    #[test]
    fn split_uses_first_double_underscore() {
        let (t, e) = split_tool_name("svc__ep__extra").unwrap();
        assert_eq!(t, "svc");
        assert_eq!(e, "ep__extra");
    }

    #[test]
    fn split_errors_without_separator() {
        assert!(split_tool_name("no_separator").is_err());
    }

    // ── json_rpc helpers ─────────────────────────────────────────────────────────

    #[test]
    fn json_rpc_ok_shape() {
        let resp = json_rpc_ok(json!(1), json!({"x": 42}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["x"], 42);
        assert!(resp.get("error").is_none());
    }

    #[test]
    fn json_rpc_error_shape() {
        let resp = json_rpc_error(json!(null), -32601, "Method not found");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found");
        assert!(resp.get("result").is_none());
    }

    // ── handle_initialize ────────────────────────────────────────────────────────

    #[test]
    fn initialize_returns_capabilities() {
        let resp = handle_initialize(&json!(1));
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        assert_eq!(resp["result"]["serverInfo"]["name"], "livefolders");
    }

    // ── list_tools ───────────────────────────────────────────────────────────────

    #[test]
    fn list_tools_one_entry_per_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let hn = tools.join("hackernews");
        fs::create_dir_all(&hn).unwrap();
        write_schema(&hn, &json!({
            "name": "hackernews",
            "description": "HN tool",
            "endpoints": [
                {"name": "top_stories", "kind": "read_invoke"},
                {"name": "item", "kind": "write_invoke"},
            ]
        }));

        let result = list_tools(tmp.path()).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["name"], "hackernews__top_stories");
        assert_eq!(result[1]["name"], "hackernews__item");
    }

    #[test]
    fn list_tools_prefers_endpoint_description() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc",
            "description": "tool-level",
            "endpoints": [{"name": "ep", "kind": "read_invoke", "description": "ep-level"}]
        }));

        let result = list_tools(tmp.path()).unwrap();
        assert_eq!(result[0]["description"], "ep-level");
    }

    #[test]
    fn list_tools_falls_back_to_tool_description() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc",
            "description": "tool-level",
            "endpoints": [{"name": "ep", "kind": "read_invoke"}]
        }));

        let result = list_tools(tmp.path()).unwrap();
        assert_eq!(result[0]["description"], "tool-level");
    }

    #[test]
    fn list_tools_read_invoke_input_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc",
            "description": "",
            "endpoints": [{"name": "status", "kind": "read_invoke"}]
        }));

        let result = list_tools(tmp.path()).unwrap();
        let schema = &result[0]["inputSchema"];
        assert!(schema["required"].as_array().unwrap().is_empty());
        assert!(schema["properties"].as_object().unwrap().is_empty());
    }

    #[test]
    fn list_tools_write_invoke_input_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc",
            "description": "",
            "endpoints": [{"name": "search", "kind": "write_invoke"}]
        }));

        let result = list_tools(tmp.path()).unwrap();
        let schema = &result[0]["inputSchema"];
        assert_eq!(schema["properties"]["input"]["type"], "string");
        assert_eq!(schema["required"][0], "input");
    }

    #[test]
    fn list_tools_skips_invalid_schema_json() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let bad = tools.join("bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("schema.json"), b"not valid json").unwrap();

        let result = list_tools(tmp.path()).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn list_tools_error_when_tools_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_tools(tmp.path()).is_err());
    }

    #[test]
    fn list_tools_sorted_alphabetically() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        for name in &["zebra", "alpha", "mango"] {
            let d = tools.join(name);
            fs::create_dir_all(&d).unwrap();
            write_schema(&d, &json!({
                "name": name, "description": "",
                "endpoints": [{"name": "ep", "kind": "read_invoke"}]
            }));
        }
        let result = list_tools(tmp.path()).unwrap();
        let names: Vec<&str> = result.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["alpha__ep", "mango__ep", "zebra__ep"]);
    }

    // ── call_tool ────────────────────────────────────────────────────────────────

    #[test]
    fn call_tool_read_invoke_reads_file() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc", "description": "",
            "endpoints": [{"name": "status", "kind": "read_invoke"}]
        }));
        fs::write(svc.join("status"), b"all systems go").unwrap();

        let out = call_tool(tmp.path(), "svc__status", &json!({})).unwrap();
        assert_eq!(out, "all systems go");
    }

    #[test]
    fn call_tool_write_invoke_writes_then_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools");
        let svc = tools.join("svc");
        fs::create_dir_all(&svc).unwrap();
        write_schema(&svc, &json!({
            "name": "svc", "description": "",
            "endpoints": [{"name": "echo", "kind": "write_invoke"}]
        }));
        fs::write(svc.join("echo"), b"").unwrap();

        let out = call_tool(tmp.path(), "svc__echo", &json!({"input": "hello world"})).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn call_tool_error_on_bad_name() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(call_tool(tmp.path(), "no_separator", &json!({})).is_err());
    }

    #[test]
    fn call_tool_error_when_schema_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let tools = tmp.path().join("tools").join("svc");
        fs::create_dir_all(&tools).unwrap();
        assert!(call_tool(tmp.path(), "svc__status", &json!({})).is_err());
    }
}
