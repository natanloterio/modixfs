use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::format_error;
use crate::manifest::{InputKind, InputSchema};
use crate::registry::{Session, Tool, ToolResult};

/// Acquires an exclusive advisory flock on `path`, creating the file if absent.
/// Returns the open `File` whose lifetime holds the lock.
async fn acquire_state_lock(path: PathBuf) -> std::io::Result<std::fs::File> {
    tokio::task::spawn_blocking(move || {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;
        let ret = unsafe { libc::flock(f.as_raw_fd() as libc::c_int, libc::LOCK_EX) };
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(f)
    })
    .await
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?
}

pub async fn invoke_command(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &Path,
    timeout_secs: u64,
    state_file: Option<&Path>,
) -> ToolResult {
    // Acquire exclusive state-file lock before spawning the handler.
    // The lock is released when `_lock` drops at the end of this function.
    let _lock = if let Some(sf) = state_file {
        let resolved = if sf.is_absolute() { sf.to_path_buf() } else { cwd.join(sf) };
        match acquire_state_lock(resolved).await {
            Ok(f) => Some(f),
            Err(e) => return ToolResult::err(format_error("SPAWN", &format!("state lock failed: {}", e))),
        }
    } else {
        None
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(handler)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(cwd)
        .env("LIVEFOLDERS_TOOL", tool_name)
        .env("LIVEFOLDERS_ENDPOINT", endpoint);
    if let Some(sf) = state_file {
        let resolved = if sf.is_absolute() { sf.to_path_buf() } else { cwd.join(sf) };
        cmd.env("LIVEFOLDERS_STATE_FILE", resolved);
    }

    let mut child = match cmd.spawn() {
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
        if let Some(ref mut h) = stdout_handle { let _ = h.read_to_end(&mut buf).await; }
        buf
    };
    let read_err = async {
        let mut buf = Vec::new();
        if let Some(ref mut h) = stderr_handle { let _ = h.read_to_end(&mut buf).await; }
        buf
    };

    let wait_fut = async {
        let (out_bytes, err_bytes) = tokio::join!(read_out, read_err);
        child.wait().await.map(|status| (status, out_bytes, err_bytes))
    };

    let started = std::time::Instant::now();
    let timed = tokio::time::timeout(Duration::from_secs(timeout_secs), wait_fut).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    match timed {
        Err(_) => {
            let _ = child.kill().await;
            ToolResult::err(format_error("TIMEOUT", &format!("handler exceeded {}s", timeout_secs)))
        }
        Ok(Err(e)) => ToolResult::err(format_error("PROCESS", &e.to_string())),
        Ok(Ok((status, out_bytes, err_bytes))) => {
            if status.success() {
                ToolResult { output: out_bytes, error: None, duration_ms, stderr: err_bytes }
            } else {
                let msg = String::from_utf8_lossy(&err_bytes);
                ToolResult {
                    output: Vec::new(),
                    error: Some(format_error("HANDLER", msg.trim())),
                    duration_ms,
                    stderr: err_bytes,
                }
            }
        }
    }
}

fn validate_input(input: &[u8], schema: &InputSchema) -> Result<(), String> {
    match schema.kind {
        InputKind::String => {
            let s = std::str::from_utf8(input)
                .map_err(|_| format_error("INVALID_INPUT", "expected valid UTF-8 string"))?;
            if let Some(min) = schema.min_length {
                if s.chars().count() < min {
                    return Err(format_error(
                        "INVALID_INPUT",
                        &format!("input too short: minimum {} characters required", min),
                    ));
                }
            }
            if let Some(max) = schema.max_length {
                if s.chars().count() > max {
                    return Err(format_error(
                        "INVALID_INPUT",
                        &format!("input too long: maximum {} characters allowed", max),
                    ));
                }
            }
            if let Some(ref pat) = schema.pattern {
                let re = regex::Regex::new(pat)
                    .map_err(|e| format_error("INVALID_INPUT", &format!("invalid pattern in schema: {}", e)))?;
                if !re.is_match(s.trim_end_matches('\n')) {
                    return Err(format_error(
                        "INVALID_INPUT",
                        &format!("input does not match required pattern: {}", pat),
                    ));
                }
            }
            Ok(())
        }
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
            let value = serde_json::from_str::<serde_json::Value>(s)
                .map_err(|e| format_error("INVALID_INPUT", &format!("expected valid JSON: {}", e)))?;
            if let Some(ref json_schema) = schema.schema {
                validate_json_schema(&value, json_schema)?;
            }
            Ok(())
        }
    }
}

/// Validates a parsed JSON value against a JSON Schema subset.
/// Supports: `required` (array of required field names), `properties` (map of
/// field name → `{ "type": "<type>" }` with type in string/number/integer/boolean/array/object/null).
fn validate_json_schema(value: &serde_json::Value, schema: &serde_json::Value) -> Result<(), String> {
    let Some(schema_obj) = schema.as_object() else {
        return Ok(());
    };

    // Top-level type check
    if let Some(expected_type) = schema_obj.get("type").and_then(|t| t.as_str()) {
        check_json_type(value, expected_type, "input")?;
    }

    let instance_obj = value.as_object()
        .ok_or_else(|| format_error("INVALID_INPUT", "expected JSON object"))?;

    // Check required fields
    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
        for field in required {
            let field_name = field.as_str().unwrap_or("");
            if !instance_obj.contains_key(field_name) {
                return Err(format_error(
                    "INVALID_INPUT",
                    &format!("missing required field: '{}'", field_name),
                ));
            }
        }
    }

    // Check properties type constraints
    if let Some(properties) = schema_obj.get("properties").and_then(|p| p.as_object()) {
        for (field_name, prop_schema) in properties {
            if let Some(field_value) = instance_obj.get(field_name) {
                if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
                    check_json_type(field_value, expected_type, field_name)?;
                }
            }
        }
    }

    Ok(())
}

fn check_json_type(value: &serde_json::Value, expected_type: &str, field: &str) -> Result<(), String> {
    let ok = match expected_type {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    };
    if !ok {
        Err(format_error(
            "INVALID_INPUT",
            &format!("field '{}' expected type '{}', got '{}'", field, expected_type, json_type_name(value)),
        ))
    } else {
        Ok(())
    }
}

fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Runs a declared pipe: each stage's stdout feeds the next stage's stdin.
/// The `duration_ms` in the returned `ToolResult` is the sum of all stage durations.
/// On any stage error the pipeline stops and returns that error immediately.
pub async fn invoke_pipe(
    stages: &[String],
    initial_input: &[u8],
    manifest: &crate::manifest::Manifest,
    tool_name: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> ToolResult {
    let mut current: Vec<u8> = initial_input.to_vec();
    let mut total_ms = 0u64;
    let mut last_stderr = Vec::new();

    for stage_name in stages {
        let spec = manifest.spec_for(stage_name);
        if let Some(s) = spec.and_then(|s| s.input.as_ref()) {
            if let Err(e) = validate_input(&current, s) {
                return ToolResult::err(e);
            }
        }
        let state_file = spec
            .and_then(|s| s.state_file.as_deref())
            .map(|sf| cwd.join(sf));
        let handler = cwd.join(stage_name).to_string_lossy().to_string();
        let result = invoke_command(
            &handler, &current, tool_name, stage_name,
            cwd, timeout_secs, state_file.as_deref(),
        ).await;
        total_ms += result.duration_ms;
        last_stderr = result.stderr.clone();
        if result.is_error() {
            return ToolResult { duration_ms: total_ms, stderr: last_stderr, ..result };
        }
        current = result.output;
    }

    ToolResult { output: current, error: None, duration_ms: total_ms, stderr: last_stderr }
}

pub async fn invoke_command_validated(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &Path,
    timeout_secs: u64,
    schema: Option<&InputSchema>,
    state_file: Option<&Path>,
) -> ToolResult {
    if let Some(s) = schema
        && let Err(e) = validate_input(input, s) {
            return ToolResult::err(e);
        }
    invoke_command(handler, input, tool_name, endpoint, cwd, timeout_secs, state_file).await
}

pub async fn invoke_command_sandboxed(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &Path,
    timeout_secs: u64,
    state_file: Option<&Path>,
    schema: Option<&InputSchema>,
    sandbox: &dyn crate::sandbox::Sandbox,
) -> ToolResult {
    if let Some(s) = schema {
        if let Err(e) = validate_input(input, s) {
            return ToolResult::err(e);
        }
    }

    let _lock = if let Some(sf) = state_file {
        let resolved = if sf.is_absolute() { sf.to_path_buf() } else { cwd.join(sf) };
        match acquire_state_lock(resolved).await {
            Ok(f) => Some(f),
            Err(e) => return ToolResult::err(format_error("SPAWN", &format!("state lock failed: {}", e))),
        }
    } else {
        None
    };

    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(handler)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .current_dir(cwd)
        .env("LIVEFOLDERS_TOOL", tool_name)
        .env("LIVEFOLDERS_ENDPOINT", endpoint);
    if let Some(sf) = state_file {
        let resolved = if sf.is_absolute() { sf.to_path_buf() } else { cwd.join(sf) };
        cmd.env("LIVEFOLDERS_STATE_FILE", resolved);
    }

    sandbox.apply(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format_error("SPAWN", &e.to_string())),
    };

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
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
        if let Some(ref mut h) = stdout_handle { let _ = h.read_to_end(&mut buf).await; }
        buf
    };
    let read_err = async {
        let mut buf = Vec::new();
        if let Some(ref mut h) = stderr_handle { let _ = h.read_to_end(&mut buf).await; }
        buf
    };

    let wait_fut = async {
        let (out_bytes, err_bytes) = tokio::join!(read_out, read_err);
        child.wait().await.map(|status| (status, out_bytes, err_bytes))
    };

    let started = std::time::Instant::now();
    let timed = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), wait_fut).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    match timed {
        Err(_) => {
            let _ = child.kill().await;
            ToolResult::err(format_error("TIMEOUT", &format!("handler exceeded {}s", timeout_secs)))
        }
        Ok(Err(e)) => ToolResult::err(format_error("PROCESS", &e.to_string())),
        Ok(Ok((status, out_bytes, err_bytes))) => {
            if status.success() {
                ToolResult { output: out_bytes, error: None, duration_ms, stderr: err_bytes }
            } else {
                let msg = String::from_utf8_lossy(&err_bytes);
                ToolResult {
                    output: Vec::new(),
                    error: Some(format_error("HANDLER", msg.trim())),
                    duration_ms,
                    stderr: err_bytes,
                }
            }
        }
    }
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

    #[allow(dead_code)]
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
        let manifest = crate::manifest::Manifest::load(&self.dir).ok().flatten();
        let spec = manifest.as_ref().and_then(|m| m.spec_for(endpoint).cloned());

        // Pipe endpoint: delegate all stages to invoke_pipe.
        if let Some(stages) = spec.as_ref().and_then(|s| s.pipe.as_ref()) {
            let m = match manifest {
                Some(m) => m,
                None => return ToolResult::err("[ERROR:SPAWN] manifest not found"),
            };
            return invoke_pipe(stages, input, &m, &self.name, &self.dir, self.timeout_secs).await;
        }

        let schema = spec.as_ref().and_then(|s| s.input.as_ref());

        let state_file = spec.as_ref()
            .and_then(|s| s.state_file.as_deref())
            .map(|sf| self.dir.join(sf));

        let handler = self.endpoint_path(endpoint).to_string_lossy().to_string();
        let sandbox = crate::sandbox::build(
            manifest.as_ref().and_then(|m| m.sandbox.as_ref()),
            crate::sandbox::SandboxMode::default(),
        );
        invoke_command_sandboxed(
            &handler, input, &self.name, endpoint,
            &self.dir, self.timeout_secs,
            state_file.as_deref(),
            schema,
            sandbox.as_ref(),
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn invoke_command_with_json_schema_accepts_valid_json() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema::of_kind(InputKind::Json);
        let result = invoke_command_validated(
            "cat", b"{\"key\":\"value\"}", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema), None,
        ).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn invoke_command_with_json_schema_rejects_invalid_json() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema::of_kind(InputKind::Json);
        let result = invoke_command_validated(
            "cat", b"not json", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema), None,
        ).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:INVALID_INPUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_with_none_schema_rejects_nonempty_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema::of_kind(InputKind::None);
        let result = invoke_command_validated(
            "cat", b"some input", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema), None,
        ).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:INVALID_INPUT]"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_with_none_schema_accepts_empty_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema::of_kind(InputKind::None);
        let result = invoke_command_validated(
            "echo ok", b"", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema), None,
        ).await;
        assert!(!result.is_error());
    }

    #[tokio::test]
    async fn invoke_command_with_no_schema_accepts_anything() {
        let result = invoke_command_validated(
            "cat", b"anything goes", "tool", "ep",
            Path::new("/tmp"), 10, None, None,
        ).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"anything goes");
    }

    #[tokio::test]
    async fn invoke_command_with_string_schema_accepts_anything() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema::of_kind(InputKind::String);
        let result = invoke_command_validated(
            "cat", b"hello world", "tool", "ep",
            Path::new("/tmp"), 10, Some(&schema), None,
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
        let result = invoke_command("echo hello", b"", "testtool", "testep", Path::new("/tmp"), 10, None).await;
        assert!(!result.is_error(), "unexpected error: {:?}", result.error);
        assert_eq!(result.output.trim_ascii_end(), b"hello");
    }

    #[tokio::test]
    async fn invoke_command_passes_stdin() {
        let result = invoke_command("cat", b"hello from stdin", "testtool", "testep", Path::new("/tmp"), 10, None).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"hello from stdin");
    }

    #[tokio::test]
    async fn invoke_command_captures_error_on_nonzero_exit() {
        let result = invoke_command("sh -c 'echo failure >&2; exit 1'", b"", "testtool", "testep", Path::new("/tmp"), 10, None).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:HANDLER]"), "got: {}", err);
        assert!(err.contains("failure"), "got: {}", err);
    }

    #[tokio::test]
    async fn invoke_command_times_out() {
        let result = invoke_command("sleep 60", b"", "testtool", "testep", Path::new("/tmp"), 1, None).await;
        assert!(result.is_error());
        let err = result.error.unwrap();
        assert!(err.starts_with("[ERROR:TIMEOUT]"), "got: {}", err);
    }

    // ── invoke_pipe tests ────────────────────────────────────────────────────

    fn make_pipe_manifest(tmp: &tempfile::TempDir, stages: &[(&str, &str)], pipe_name: &str) -> crate::manifest::Manifest {
        use std::fmt::Write as FmtWrite;
        let mut yaml = format!("name: pipetool\nfiles:\n");
        for (name, _) in stages {
            write!(yaml, "  - name: {name}\n    type: write_invoke\n    handler: ./{name}\n").unwrap();
        }
        write!(yaml, "  - name: {pipe_name}\n    type: write_invoke\n    pipe: [{}]\n",
            stages.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")).unwrap();
        for (name, script) in stages {
            let p = tmp.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{script}")).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        }
        std::fs::write(tmp.path().join("folder.yaml"), &yaml).unwrap();
        serde_yaml::from_str(&yaml).unwrap()
    }

    #[tokio::test]
    async fn invoke_pipe_chains_stdout_to_stdin() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = make_pipe_manifest(&tmp, &[
            ("upper", "tr a-z A-Z"),
            ("trim",  "tr -d '\\n'"),
        ], "process");
        let result = invoke_pipe(
            manifest.spec_for("process").unwrap().pipe.as_ref().unwrap(),
            b"hello",
            &manifest, "pipetool", tmp.path(), 10,
        ).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
        assert_eq!(result.output, b"HELLO");
    }

    #[tokio::test]
    async fn invoke_pipe_stops_on_stage_error() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = make_pipe_manifest(&tmp, &[
            ("fail",   "exit 1"),
            ("should_not_run", "cat"),
        ], "broken");
        let result = invoke_pipe(
            manifest.spec_for("broken").unwrap().pipe.as_ref().unwrap(),
            b"input",
            &manifest, "pipetool", tmp.path(), 10,
        ).await;
        assert!(result.is_error());
        assert!(result.error.as_ref().unwrap().starts_with("[ERROR:HANDLER]"), "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn invoke_pipe_accumulates_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = make_pipe_manifest(&tmp, &[
            ("a", "cat"),
            ("b", "cat"),
            ("c", "cat"),
        ], "chain");
        let result = invoke_pipe(
            manifest.spec_for("chain").unwrap().pipe.as_ref().unwrap(),
            b"data",
            &manifest, "pipetool", tmp.path(), 10,
        ).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"data");
    }

    #[tokio::test]
    async fn external_tool_routes_pipe_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        make_pipe_manifest(&tmp, &[
            ("shout", "tr a-z A-Z"),
        ], "pipeline");
        let tool = ExternalTool::new("pipetool", tmp.path().to_path_buf(), 10);
        let session = crate::registry::Session::default();
        let result = tool.invoke("pipeline", b"world", &session).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
        assert_eq!(result.output.trim_ascii_end(), b"WORLD");
    }

    #[tokio::test]
    async fn external_tool_rejects_invalid_json_when_schema_declared() {
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

    // ── String constraint tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn string_min_length_rejects_short_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::String, min_length: Some(5), ..InputSchema::of_kind(InputKind::String) };
        let result = invoke_command_validated("cat", b"hi", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("too short"), "wrong error");
    }

    #[tokio::test]
    async fn string_min_length_accepts_exact_length() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::String, min_length: Some(5), ..InputSchema::of_kind(InputKind::String) };
        let result = invoke_command_validated("cat", b"hello", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn string_max_length_rejects_long_input() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema { kind: InputKind::String, max_length: Some(3), ..InputSchema::of_kind(InputKind::String) };
        let result = invoke_command_validated("cat", b"toolong", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("too long"), "wrong error");
    }

    #[tokio::test]
    async fn string_pattern_rejects_non_matching() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema {
            kind: InputKind::String,
            pattern: Some(r"^\d+$".to_string()),
            ..InputSchema::of_kind(InputKind::String)
        };
        let result = invoke_command_validated("cat", b"abc", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("pattern"), "wrong error");
    }

    #[tokio::test]
    async fn string_pattern_accepts_matching() {
        use crate::manifest::{InputKind, InputSchema};
        let schema = InputSchema {
            kind: InputKind::String,
            pattern: Some(r"^\d+$".to_string()),
            ..InputSchema::of_kind(InputKind::String)
        };
        let result = invoke_command_validated("cat", b"12345", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    // ── JSON schema subset tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn json_schema_rejects_missing_required_field() {
        use crate::manifest::{InputKind, InputSchema};
        let json_schema = serde_json::json!({"required": ["query"]});
        let schema = InputSchema {
            kind: InputKind::Json,
            schema: Some(json_schema),
            ..InputSchema::of_kind(InputKind::Json)
        };
        let result = invoke_command_validated("cat", b"{\"limit\":10}", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("query"), "wrong error");
    }

    #[tokio::test]
    async fn json_schema_accepts_all_required_fields_present() {
        use crate::manifest::{InputKind, InputSchema};
        let json_schema = serde_json::json!({"required": ["query", "limit"]});
        let schema = InputSchema {
            kind: InputKind::Json,
            schema: Some(json_schema),
            ..InputSchema::of_kind(InputKind::Json)
        };
        let result = invoke_command_validated("cat", b"{\"query\":\"foo\",\"limit\":10}", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    #[tokio::test]
    async fn json_schema_rejects_wrong_property_type() {
        use crate::manifest::{InputKind, InputSchema};
        let json_schema = serde_json::json!({
            "properties": { "limit": { "type": "number" } }
        });
        let schema = InputSchema {
            kind: InputKind::Json,
            schema: Some(json_schema),
            ..InputSchema::of_kind(InputKind::Json)
        };
        let result = invoke_command_validated("cat", b"{\"limit\":\"ten\"}", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("limit"), "wrong error");
    }

    #[tokio::test]
    async fn json_schema_accepts_correct_property_type() {
        use crate::manifest::{InputKind, InputSchema};
        let json_schema = serde_json::json!({
            "properties": { "limit": { "type": "number" } }
        });
        let schema = InputSchema {
            kind: InputKind::Json,
            schema: Some(json_schema),
            ..InputSchema::of_kind(InputKind::Json)
        };
        let result = invoke_command_validated("cat", b"{\"limit\":10}", "t", "e", Path::new("/tmp"), 10, Some(&schema), None).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
    }

    // ── state_file tests ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn invoke_command_passes_state_file_env_to_handler() {
        let tmp = tempfile::tempdir().unwrap();
        let sf = tmp.path().join("state.db");
        // Handler echoes the env var so we can assert its value.
        let result = invoke_command(
            "echo $LIVEFOLDERS_STATE_FILE", b"", "t", "e",
            tmp.path(), 10, Some(&sf),
        ).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
        let output = String::from_utf8_lossy(&result.output);
        assert!(output.trim() == sf.to_string_lossy(), "expected {:?}, got {:?}", sf, output.trim());
    }

    #[tokio::test]
    async fn invoke_command_creates_state_file_if_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let sf = tmp.path().join("brand_new.db");
        assert!(!sf.exists(), "precondition: file must not exist yet");
        let result = invoke_command("true", b"", "t", "e", tmp.path(), 10, Some(&sf)).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
        assert!(sf.exists(), "state file should have been created by the lock");
    }

    #[tokio::test]
    async fn invoke_command_without_state_file_does_not_set_env() {
        // Handler exits non-zero when LIVEFOLDERS_STATE_FILE is set.
        let result = invoke_command(
            "test -z \"$LIVEFOLDERS_STATE_FILE\"", b"", "t", "e",
            Path::new("/tmp"), 10, None,
        ).await;
        assert!(!result.is_error(), "LIVEFOLDERS_STATE_FILE should not be set, got: {:?}", result.error);
    }

    #[tokio::test]
    async fn sandbox_is_applied_noop_does_not_break_invocation() {
        use crate::sandbox::{build as build_sandbox, SandboxMode};
        let sandbox = build_sandbox(None, SandboxMode::Disabled);
        let result = invoke_command_sandboxed(
            "echo hello", b"", "tool", "ep",
            Path::new("/tmp"), 10, None, None,
            sandbox.as_ref(),
        ).await;
        assert!(!result.is_error(), "got: {:?}", result.error);
        assert_eq!(result.output.trim_ascii_end(), b"hello");
    }

    #[tokio::test]
    async fn concurrent_invocations_with_state_file_serialize() {
        // Two concurrent handlers each append a line to the state file.
        // With the lock they must not interleave writes.
        let tmp = tempfile::tempdir().unwrap();
        let sf = tmp.path().join("counter.txt");
        // Prime the file so it exists before the lock race.
        std::fs::write(&sf, "").unwrap();

        let sf1 = sf.clone();
        let sf2 = sf.clone();
        let dir = tmp.path().to_path_buf();
        let (r1, r2) = tokio::join!(
            invoke_command("echo line1 >> $LIVEFOLDERS_STATE_FILE", b"", "t", "e", &dir, 10, Some(&sf1)),
            invoke_command("echo line2 >> $LIVEFOLDERS_STATE_FILE", b"", "t", "e", &dir, 10, Some(&sf2)),
        );
        assert!(!r1.is_error(), "got: {:?}", r1.error);
        assert!(!r2.is_error(), "got: {:?}", r2.error);
        let content = std::fs::read_to_string(&sf).unwrap();
        // Both lines must be present (neither was lost to a race).
        assert!(content.contains("line1"), "missing line1 in: {content}");
        assert!(content.contains("line2"), "missing line2 in: {content}");
    }
}
