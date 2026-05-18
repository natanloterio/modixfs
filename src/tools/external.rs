use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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
        Err(e) => return ToolResult::err(format!("failed to spawn handler: {}", e)),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(input).await {
            return ToolResult::err(format!("failed to write stdin: {}", e));
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
            ToolResult::err("timeout")
        }
        Ok(Err(e)) => ToolResult::err(format!("process error: {}", e)),
        Ok(Ok((status, out_bytes, err_bytes))) => {
            if status.success() {
                ToolResult::ok(out_bytes)
            } else {
                let stderr = String::from_utf8_lossy(&err_bytes);
                ToolResult::err(stderr.trim().to_string())
            }
        }
    }
}

pub struct ExternalTool {
    name: String,
    dir: PathBuf,
    timeout_secs: u64,
}

impl ExternalTool {
    pub fn new(name: impl Into<String>, dir: PathBuf, timeout_secs: u64) -> Self {
        Self { name: name.into(), dir, timeout_secs }
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
        "External tool"
    }

    fn how_to(&self) -> &str {
        ""
    }

    fn endpoints(&self) -> Vec<&str> {
        vec![]
    }

    async fn invoke(&self, endpoint: &str, input: &[u8], _session: &Session) -> ToolResult {
        let script = self.endpoint_path(endpoint);
        invoke_command(
            script.to_str().unwrap_or(""),
            input,
            &self.name,
            endpoint,
            &self.dir,
            self.timeout_secs,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        assert!(result.error.unwrap().contains("failure"));
    }

    #[tokio::test]
    async fn invoke_command_times_out() {
        let result = invoke_command("sleep 60", b"", "testtool", "testep", Path::new("/tmp"), 1).await;
        assert!(result.is_error());
        assert_eq!(result.error.as_deref(), Some("timeout"));
    }
}
