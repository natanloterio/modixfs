// MCP server process pool

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use super::client::McpClient;

#[derive(Debug, Deserialize)]
struct McpServersConfig {
    #[serde(default)]
    servers: HashMap<String, ServerEntry>,
}

#[derive(Debug, Deserialize)]
struct ServerEntry {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
}

pub struct ServerPool {
    config_path: PathBuf,
    clients: Mutex<HashMap<String, Arc<Mutex<McpClient>>>>,
}

impl ServerPool {
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            config_path,
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub fn call(&self, server: &str, tool: &str, args: Value) -> Result<String> {
        let client = self.get_or_spawn(server)?;
        let mut c = client.lock().unwrap();
        c.call_tool(tool, args)
    }

    pub fn running_servers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.clients.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    }

    fn get_or_spawn(&self, server: &str) -> Result<Arc<Mutex<McpClient>>> {
        let mut clients = self.clients.lock().unwrap();
        if let Some(c) = clients.get(server) {
            return Ok(Arc::clone(c));
        }
        let entry = self.load_entry(server)?;
        let env_pairs: Vec<(String, String)> = entry.env.into_iter().collect();
        let client = McpClient::spawn(&entry.command, &entry.args, &env_pairs)
            .with_context(|| format!("failed to spawn MCP server '{}'", server))?;
        let arc = Arc::new(Mutex::new(client));
        clients.insert(server.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    fn load_entry(&self, server: &str) -> Result<ServerEntry> {
        let content = std::fs::read_to_string(&self.config_path)
            .with_context(|| format!("cannot read {}", self.config_path.display()))?;
        let mut cfg: McpServersConfig = serde_yaml::from_str(&content)?;
        cfg.servers.remove(server)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not registered in mcp-servers.yaml", server))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_mcp_script() -> &'static str {
        r#"#!/usr/bin/env python3
import sys, json
def s(o): sys.stdout.write(json.dumps(o)+"\n"); sys.stdout.flush()
for line in sys.stdin:
    line=line.strip()
    if not line: continue
    req=json.loads(line); method=req.get("method",""); id_=req.get("id")
    if method=="initialize": s({"jsonrpc":"2.0","id":id_,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.1"}}})
    elif method=="notifications/initialized": pass
    elif method=="tools/call":
        args=req["params"].get("arguments",{})
        tool=req["params"]["name"]
        s({"jsonrpc":"2.0","id":id_,"result":{"content":[{"type":"text","text":f"ok:{tool}:{args}"}],"isError":False}})
"#
    }

    fn make_config(tmp: &tempfile::TempDir, script: &std::path::Path) -> std::path::PathBuf {
        let cfg = tmp.path().join("mcp-servers.yaml");
        let content = format!(
            "servers:\n  fake-tool:\n    command: python3\n    args: [\"{}\"]\n",
            script.display()
        );
        std::fs::write(&cfg, content).unwrap();
        cfg
    }

    #[test]
    fn pool_call_spawns_on_demand() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake.py");
        std::fs::write(&script, fake_mcp_script()).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = make_config(&tmp, &script);
        let pool = ServerPool::new(cfg);
        let result = pool.call("fake-tool", "any_tool", serde_json::json!({"x": 1})).unwrap();
        assert!(result.contains("ok:"), "got: {}", result);
    }

    #[test]
    fn pool_unknown_server_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("mcp-servers.yaml");
        std::fs::write(&cfg, "servers: {}\n").unwrap();
        let pool = ServerPool::new(cfg);
        let err = pool.call("nonexistent", "tool", serde_json::json!({})).unwrap_err();
        assert!(err.to_string().contains("nonexistent"), "got: {}", err);
    }

    #[test]
    fn pool_reuses_existing_client() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake.py");
        std::fs::write(&script, fake_mcp_script()).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = make_config(&tmp, &script);
        let pool = ServerPool::new(cfg);
        // Call twice — should reuse the same McpClient (no re-spawn)
        let r1 = pool.call("fake-tool", "tool_a", serde_json::json!({})).unwrap();
        let r2 = pool.call("fake-tool", "tool_b", serde_json::json!({})).unwrap();
        assert!(r1.contains("tool_a"), "got: {}", r1);
        assert!(r2.contains("tool_b"), "got: {}", r2);
    }
}
