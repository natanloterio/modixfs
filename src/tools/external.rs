use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::format_error;
use crate::manifest::{InputKind, InputSchema};
use crate::registry::{Session, Tool, ToolResult};

pub async fn invoke_command(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
) -> ToolResult {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(handler)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(cwd)
        .env("LIVEFOLDERS_TOOL", tool_name)
        .env("LIVEFOLDERS_ENDPOINT", endpoint)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format_error("SPAWN", &e.to_string())),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(input).await {
            return ToolResult::err(format_error("SPAWN", &format!("failed to write stdin: {}", e)));
        }
        let _ = stdin.flush().await;
    }

    use tokio::io::AsyncReadExt;
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let read_out = async {
        let mut buf = Vec::new();
        if let Some(ref mut h) = stdout_handle {
            let _ = h.read_to_end(&mut buf).await;
        }
        buf
    };
    let read_err = async {
        let mut buf = Vec::new();
        if let Some(ref mut h) = stderr_handle {
            let _ = h.read_to_end(&mut buf).await;
        }
        buf
    };

    let wait_fut = async {
        let (out_bytes, err_bytes) = tokio::join!(read_out, read_err);
        child.wait().await.map(|status| (status, out_bytes, err_bytes))
    };

    match tokio::time::timeout(Duration::from_secs(timeout_secs), wait_fut).await {
        Err(_) => {
            let _ = child.kill().await;
            ToolResult::err(format_error("TIMEOUT", &format!("handler exceeded {}s", timeout_secs)))
        }
        Ok(Err(e)) => ToolResult::err(format_error("PROCESS", &e.to_string())),
        Ok(Ok((status, out_bytes, err_bytes))) => {
            if status.success() {
                ToolResult::ok(out_bytes)
            } else {
                let stderr = String::from_utf8_lossy(&err_bytes);
                ToolResult::err(format_error("HANDLER", stderr.trim()))
            }
        }
    }
}

fn validate_input(input: &[u8], schema: &InputSchema) -> Result<(), String> {
    match schema.kind {
        InputKind::String => Ok(()),
        InputKind::None => {
            if input.is_empty() {
                Ok(())
            } else {
                Err(format_error("INVALID_INPUT", "endpoint takes no input"))
            }
        }
        InputKind::Json => {
            let s = std::str::from_utf8(input)
                .map_err(|_| format_error("INVALID_INPUT", "expected valid UTF-8 JSON"))?;
            serde_json::from_str::<serde_json::Value>(s)
                .map(|_| ())
                .map_err(|e| format_error("INVALID_INPUT", &format!("expected valid JSON: {}", e)))
        }
    }
}

pub async fn invoke_command_validated(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
    schema: Option<&InputSchema>,
) -> ToolResult {
    if let Some(s) = schema {
        if let Err(e) = validate_input(input, s) {
            return ToolResult::err(e);
        }
    }
    invoke_command(handler, input, tool_name, endpoint, cwd, timeout_secs).await
}

pub struct ExternalTool {
    name: String,
    dir: PathBuf,
    timeout_secs: u64,
    description_cache: std::sync::OnceLock<String>,
}

impl ExternalTool {
    pub fn new(name: impl Into<String>, dir: PathBuf, timeout_secs: u64) -> Self {
        Self { name: name.into(), dir, timeout_secs, description_cache: std::sync::OnceLock::new() }
    }

    fn endpoint_path(&self, endpoint: &str) -> PathBuf {
        self.dir.join(endpoint)
    }

    pub fn description_from_how_to(&self) -> String {
        let how_to = self.dir.join("how_to.md");
        std::fs::read_to_string(&how_to)
            .ok()
            .and_then(|s| s.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim_start_matches('#').trim().to_string()))
            .unwrap_or_else(|| format!("External tool at {}", self.dir.display()))
    }

    fn description_from_manifest(&self) -> Option<String> {
        let path = self.dir.join("folder.yaml");
        let content = std::fs::read_to_string(path).ok()?;
        let manifest: crate::manifest::Manifest = serde_yaml::from_str(&content).ok()?;
        manifest.description
    }

    pub fn endpoints_from_disk(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return vec![] };
        let mut eps = vec![];
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "how_to.md" { continue; }
            if path.is_dir() { continue; }
            let Ok(meta) = path.metadata() else { continue };
            if meta.permissions().mode() & 0o111 != 0 {
                eps.push(name);
            }
        }
        eps.sort();
        eps
    }
}

#[async_trait]
impl Tool for ExternalTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        self.description_cache.get_or_init(|| {
            self.description_from_manifest()
                .unwrap_or_else(|| self.description_from_how_to())
        })
    }

    fn how_to(&self) -> &str {
        ""
    }

    fn endpoints(&self) -> Vec<&str> {
        vec![]
    }

    async fn invoke(&self, endpoint: &str, input: &[u8], _session: &Session) -> ToolResult {
        // Look up input schema from folder.yaml if present
        let schema = crate::manifest::Manifest::load(&self.dir)
            .ok()
            .flatten()
            .and_then(|m| m.spec_for(endpoint).and_then(|s| s.input.clone()));

        if let Some(ref s) = schema {
            if let Err(e) = validate_input(input, s) {
                return ToolResult::err(e);
            }
        }

        let script = self.endpoint_path(endpoint);

        let mut child = match Command::new(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&self.dir)
            .env("LIVEFOLDERS_TOOL", &self.name)
            .env("LIVEFOLDERS_ENDPOINT", endpoint)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format_error("SPAWN", &format!("failed to spawn {}: {}", script.display(), e))),
        };

        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(input).await {
                return ToolResult::err(format_error("SPAWN", &format!("failed to write stdin: {}", e)));
            }
            let _ = stdin.flush().await;
        }

        use tokio::io::AsyncReadExt;
        let mut stdout_handle = child.stdout.take();
        let mut stderr_handle = child.stderr.take();

        let read_out = async {
            let mut buf = Vec::new();
            if let Some(ref mut h) = stdout_handle {
                let _ = h.read_to_end(&mut buf).await;
            }
            buf
        };
        let read_err = async {
            let mut buf = Vec::new();
            if let Some(ref mut h) = stderr_handle {
                let _ = h.read_to_end(&mut buf).await;
            }
            buf
        };

        let wait_fut = async {
            let (out_bytes, err_bytes) = tokio::join!(read_out, read_err);
            child.wait().await.map(|status| (status, out_bytes, err_bytes))
        };

        match tokio::time::timeout(Duration::from_secs(self.timeout_secs), wait_fut).await {
            Err(_) => {
                let _ = child.kill().await;
                ToolResult::err(format_error("TIMEOUT", &format!("handler exceeded {}s", self.timeout_secs)))
            }
            Ok(Err(e)) => ToolResult::err(format_error("PROCESS", &e.to_string())),
            Ok(Ok((status, out_bytes, err_bytes))) => {
                if status.success() {
                    ToolResult::ok(out_bytes)
                } else {
                    let stderr = String::from_utf8_lossy(&err_bytes);
                    ToolResult::err(format_error("HANDLER", stderr.trim()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn invoke_command_with_json_schema_accepts_valid_json() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::Json };
        let result = invoke_command_validated(
            "cat", b"{\"key\":\"value\"}", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema),
        ).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn invoke_command_with_json_schema_rejects_invalid_json() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::Json };
        let result = invoke_command_validated(
            "cat", b"not json", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema),
        ).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:INVALID_INPUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_with_none_schema_rejects_nonempty_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::None };
        let result = invoke_command_validated(
            "cat", b"some input", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema),
        ).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:INVALID_INPUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_with_none_schema_accepts_empty_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::None };
        let result = invoke_command_validated(
            "echo ok", b"", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema),
        ).await;
        assert!(!result.is_error());
    }

    #[tokio::test]
    async fn invoke_command_with_no_schema_accepts_anything() {
        let result = invoke_command_validated(
            "cat", b"anything goes", "tool", "ep",
            Path::new("/tmp"), 10, None,
        ).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"anything goes");
    }

    #[tokio::test]
    async fn invoke_command_with_string_schema_accepts_anything() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::String };
        let result = invoke_command_validated(
            "cat", b"hello world", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema),
        ).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"hello world");
    }

    #[test]
    fn description_reads_from_folder_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("folder.yaml"),
            "name: mytool\ndescription: Fetches data from the API\n",
        ).unwrap();
        let tool = ExternalTool::new("mytool", tmp.path().to_path_buf(), 30);
        assert_eq!(tool.description(), "Fetches data from the API");
    }

    #[test]
    fn description_falls_back_to_how_to_when_no_folder_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("how_to.md"), "# My tool\nDoes stuff.\n").unwrap();
        let tool = ExternalTool::new("mytool", tmp.path().to_path_buf(), 30);
        assert_eq!(tool.description(), "My tool");
    }

    #[tokio::test]
    async fn invoke_command_echo() {
        let result = invoke_command("echo hello", b"", "testtool", "testep", Path::new("/tmp"), 10).await;
        assert!(!result.is_error(), "unexpected error: {:?}", result.error);
        assert_eq!(result.output.trim_ascii_end(), b"hello");
    }

    #[tokio::test]
    async fn invoke_command_passes_stdin() {
        let result = invoke_command("cat", b"hello from stdin", "testtool", "testep", Path::new("/tmp"), 10).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"hello from stdin");
    }

    #[tokio::test]
    async fn invoke_command_captures_error_on_nonzero_exit() {
        let result = invoke_command("sh -c 'echo failure >&2; exit 1'", b"", "testtool", "testep", Path::new("/tmp"), 10).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:HANDLER]"), "got: {}", err);
        assert!(err.contains("failure"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_times_out() {
        let result = invoke_command("sleep 60", b"", "testtool", "testep", Path::new("/tmp"), 1).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:TIMEOUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn external_tool_rejects_invalid_json_when_schema_declared() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("folder.yaml"), r#"
name: testtool
files:
  - name: search
    type: write_invoke
    handler: "cat"
    input:
      type: json
"#).unwrap();
        let ep = tmp.path().join("search");
        std::fs::write(&ep, "#!/bin/sh\ncat").unwrap();
        std::fs::set_permissions(&ep, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let tool = ExternalTool::new("testtool", tmp.path().to_path_buf(), 10);
        let session = crate::registry::Session::default();
        let result = tool.invoke("search", b"not json", &session).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:INVALID_INPUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn external_tool_accepts_valid_json_when_schema_declared() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("folder.yaml"), r#"
name: testtool
files:
  - name: search
    type: write_invoke
    handler: "cat"
    input:
      type: json
"#).unwrap();
        let ep = tmp.path().join("search");
        std::fs::write(&ep, "#!/bin/sh\ncat").unwrap();
        std::fs::set_permissions(&ep, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

        let tool = ExternalTool::new("testtool", tmp.path().to_path_buf(), 10);
        let session = crate::registry::Session::default();
        let result = tool.invoke("search", b"{\"q\":\"hello\"}", &session).await;
        assert!(!result.is_error(), "unexpected error: {:?}", result.error);
    }
}
