// MCP JSON-RPC stdio client

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub struct McpClient {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pub initialized: bool,
}

impl McpClient {
    pub fn spawn(command: &str, args: &[String], env_pairs: &[(String, String)]) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env_pairs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()
            .with_context(|| format!("SPAWN: failed to start '{}'", command))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(Self { _child: child, stdin, stdout, next_id: 1, initialized: false })
    }

    pub fn initialize(&mut self) -> Result<()> {
        let id = self.next_id();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "livefolders-proxy", "version": env!("CARGO_PKG_VERSION")}
            }
        }))?;
        let resp = self.recv()?;
        if resp.get("error").is_some() {
            anyhow::bail!("MCP initialize error: {}", resp["error"]);
        }
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))?;
        self.initialized = true;
        Ok(())
    }

    pub fn call_tool(&mut self, tool: &str, arguments: Value) -> Result<String> {
        if !self.initialized {
            self.initialize()?;
        }
        let id = self.next_id();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }))?;
        let resp = self.recv()?;
        if let Some(err) = resp.get("error") {
            anyhow::bail!("HANDLER: MCP tool error: {}", err);
        }
        let content = &resp["result"]["content"];
        let text = content.as_array()
            .and_then(|arr| arr.iter().find(|c| c["type"] == "text"))
            .and_then(|c| c["text"].as_str())
            .unwrap_or("")
            .to_string();
        Ok(text)
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)?;
        writeln!(self.stdin, "{}", line)?;
        self.stdin.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Value> {
        loop {
            let mut line = String::new();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("MCP server closed stdout unexpectedly");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // skip non-JSON startup noise from the server
            };
            if v.get("id").is_none() {
                continue; // skip notifications
            }
            return Ok(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_mcp_server_script() -> &'static str {
        r#"#!/usr/bin/env python3
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method", "")
    id_ = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":id_,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0.1"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/call":
        tool = req["params"]["name"]
        args = req["params"].get("arguments", {})
        send({"jsonrpc":"2.0","id":id_,"result":{"content":[{"type":"text","text":f"called:{tool}:{args}"}],"isError":False}})
"#
    }

    fn fake_mcp_server_with_startup_noise_script() -> &'static str {
        r#"#!/usr/bin/env python3
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

# Simulate servers (like npx auto-browser) that print plain text to stdout on startup
sys.stdout.write("Light abstraction onto puppeteer in order to simplify easy browser automation task\n")
sys.stdout.flush()
sys.stdout.write("Starting MCP server...\n")
sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method", "")
    id_ = req.get("id")
    if method == "initialize":
        send({"jsonrpc":"2.0","id":id_,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"noisy","version":"0.1"}}})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/call":
        tool = req["params"]["name"]
        args = req["params"].get("arguments", {})
        send({"jsonrpc":"2.0","id":id_,"result":{"content":[{"type":"text","text":f"called:{tool}:{args}"}],"isError":False}})
"#
    }

    fn write_fake_server(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let path = tmp.path().join("fake_mcp.py");
        std::fs::write(&path, fake_mcp_server_script()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn write_noisy_fake_server(tmp: &tempfile::TempDir) -> std::path::PathBuf {
        let path = tmp.path().join("fake_mcp_noisy.py");
        std::fs::write(&path, fake_mcp_server_with_startup_noise_script()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn client_spawn_and_call_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_fake_server(&tmp);
        let mut client = McpClient::spawn("python3", &[script.to_string_lossy().to_string()], &[]).unwrap();
        client.initialize().unwrap();
        let result = client.call_tool("echo", serde_json::json!({"msg": "hi"})).unwrap();
        assert!(result.contains("called:echo"), "got: {}", result);
    }

    #[test]
    fn client_initialize_sets_initialized_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_fake_server(&tmp);
        let mut client = McpClient::spawn("python3", &[script.to_string_lossy().to_string()], &[]).unwrap();
        assert!(!client.initialized);
        client.initialize().unwrap();
        assert!(client.initialized);
    }

    #[test]
    fn client_call_tool_auto_initializes() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_fake_server(&tmp);
        let mut client = McpClient::spawn("python3", &[script.to_string_lossy().to_string()], &[]).unwrap();
        // call_tool without explicit initialize — should auto-initialize
        let result = client.call_tool("ping", serde_json::json!({})).unwrap();
        assert!(result.contains("called:ping"), "got: {}", result);
    }

    #[test]
    fn client_skips_non_json_startup_noise() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_noisy_fake_server(&tmp);
        let mut client = McpClient::spawn("python3", &[script.to_string_lossy().to_string()], &[]).unwrap();
        client.initialize().unwrap();
        let result = client.call_tool("browser_navigate", serde_json::json!({"url": "https://example.com"})).unwrap();
        assert!(result.contains("called:browser_navigate"), "got: {}", result);
    }
}
