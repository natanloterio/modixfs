# YAML File Behaviors Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow tool authors to declare in `livefolders.yaml` how each virtual file responds to FUSE write/read calls, replacing the current executable-bit heuristic.

**Architecture:** Four tasks in strict order — types first, then the invocation helper, then discovery (readdir/lookup), then dispatch (write/release/read). Each task is independently testable. The FUSE layer loads the manifest on each routing decision; no caching is needed at this stage.

**Tech Stack:** Rust, serde_yaml (already in Cargo.toml), tokio, libc. No new dependencies — handler commands are run via `sh -c`.

---

### Task 1: Add FileKind and FileSpec to manifest.rs

**Files:**
- Modify: `src/manifest.rs`

**Context:**
`Manifest` currently has `name`, `description`, `version`, `env`. We add a `files` section that declares how virtual files behave. `FileKind` maps to the four types from the design: `write_invoke`, `read_invoke`, `passthrough`, `readonly`. `FileSpec` is one entry in the `files` list.

**Step 1: Write the failing tests**

Add to the `#[cfg(test)]` block at the bottom of `src/manifest.rs`:

```rust
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
    let yaml = r#"
files:
  - name: config.json
    type: passthrough
    handler: ./something
"#;
    let m: Manifest = serde_yaml::from_str(yaml).unwrap();
    assert!(m.validate().is_err());
}

#[test]
fn validate_rejects_missing_handler_on_write_invoke() {
    let yaml = r#"
files:
  - name: search
    type: write_invoke
"#;
    let m: Manifest = serde_yaml::from_str(yaml).unwrap();
    assert!(m.validate().is_err());
}

#[test]
fn validate_accepts_valid_manifest() {
    let yaml = r#"
files:
  - name: search
    type: write_invoke
    handler: ./bin/search
  - name: config.json
    type: passthrough
"#;
    let m: Manifest = serde_yaml::from_str(yaml).unwrap();
    assert!(m.validate().is_ok());
}
```

**Step 2: Run tests to verify they fail**

```bash
cd /media/loterio/workspace/workspace/research/ModixFS
cargo test -p livefolders manifest 2>&1 | tail -20
```

Expected: FAIL — `FileKind`, `FileSpec`, `spec_for`, `validate` do not exist yet.

**Step 3: Implement the types**

Replace the contents of `src/manifest.rs` with:

```rust
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
        let path = tool_dir.join("livefolders.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let manifest = serde_yaml::from_str(&content)?;
        Ok(Some(manifest))
    }

    pub fn spec_for(&self, name: &str) -> Option<&FileSpec> {
        self.files.iter().find(|f| f.name == name)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        for spec in &self.files {
            match spec.kind {
                FileKind::WriteInvoke | FileKind::ReadInvoke => {
                    if spec.handler.is_none() {
                        anyhow::bail!(
                            "file '{}' has type '{}' but no handler",
                            spec.name,
                            match spec.kind {
                                FileKind::WriteInvoke => "write_invoke",
                                FileKind::ReadInvoke => "read_invoke",
                                _ => unreachable!(),
                            }
                        );
                    }
                }
                FileKind::Passthrough | FileKind::Readonly => {
                    if spec.handler.is_some() {
                        anyhow::bail!(
                            "file '{}' has type '{}' but specifies a handler (handlers are only for write_invoke and read_invoke)",
                            spec.name,
                            match spec.kind {
                                FileKind::Passthrough => "passthrough",
                                FileKind::Readonly => "readonly",
                                _ => unreachable!(),
                            }
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
        assert!(m.files.is_empty());
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
        let mut f = std::fs::File::create(tmp.path().join("livefolders.yaml")).unwrap();
        writeln!(f, "name: testpkg\nversion: 1.0.0").unwrap();
        let m = Manifest::load(tmp.path()).unwrap().unwrap();
        assert_eq!(m.name.as_deref(), Some("testpkg"));
    }

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
        let yaml = r#"
files:
  - name: config.json
    type: passthrough
    handler: ./something
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_rejects_missing_handler_on_write_invoke() {
        let yaml = r#"
files:
  - name: search
    type: write_invoke
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_err());
    }

    #[test]
    fn validate_accepts_valid_manifest() {
        let yaml = r#"
files:
  - name: search
    type: write_invoke
    handler: ./bin/search
  - name: config.json
    type: passthrough
"#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert!(m.validate().is_ok());
    }
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test -p livefolders manifest 2>&1 | tail -20
```

Expected: all manifest tests PASS.

**Step 5: Commit**

```bash
git add src/manifest.rs
git commit -m "feat: add FileKind and FileSpec to manifest for YAML file behavior declarations"
```

---

### Task 2: Add invoke_command helper to external.rs

**Files:**
- Modify: `src/tools/external.rs`

**Context:**
`ExternalTool::invoke()` currently always runs the file at `self.dir.join(endpoint)`. We need a standalone async function `invoke_command` that runs any arbitrary shell command string (like `"curl -s -X POST -d @- https://api.example.com"` or `"python3 ./script.py"`). It uses `sh -c` for shell parsing — no new crates needed. This function is used in vfs.rs Task 4.

**Step 1: Write the failing test**

Add to `src/tools/external.rs` at the end of the file, inside a new `#[cfg(test)]` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[tokio::test]
    async fn invoke_command_echo() {
        let result = invoke_command(
            "echo hello",
            b"",
            "testtool",
            "testep",
            Path::new("/tmp"),
            10,
        )
        .await;
        assert!(!result.is_error(), "unexpected error: {:?}", result.error);
        assert_eq!(result.output.trim_ascii_end(), b"hello");
    }

    #[tokio::test]
    async fn invoke_command_passes_stdin() {
        let result = invoke_command(
            "cat",
            b"hello from stdin",
            "testtool",
            "testep",
            Path::new("/tmp"),
            10,
        )
        .await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"hello from stdin");
    }

    #[tokio::test]
    async fn invoke_command_captures_error_on_nonzero_exit() {
        let result = invoke_command(
            "sh -c 'echo failure >&2; exit 1'",
            b"",
            "testtool",
            "testep",
            Path::new("/tmp"),
            10,
        )
        .await;
        assert!(result.is_error());
        assert!(result.error.unwrap().contains("failure"));
    }

    #[tokio::test]
    async fn invoke_command_times_out() {
        let result = invoke_command(
            "sleep 60",
            b"",
            "testtool",
            "testep",
            Path::new("/tmp"),
            1,
        )
        .await;
        assert!(result.is_error());
        assert_eq!(result.error.as_deref(), Some("timeout"));
    }
}
```

**Step 2: Run tests to verify they fail**

```bash
cargo test -p livefolders invoke_command 2>&1 | tail -20
```

Expected: FAIL — `invoke_command` not defined.

**Step 3: Implement invoke_command**

Add the following function to `src/tools/external.rs` before the `ExternalTool` struct (after the imports):

```rust
pub async fn invoke_command(
    handler: &str,
    input: &[u8],
    tool_name: &str,
    endpoint: &str,
    cwd: &std::path::Path,
    timeout_secs: u64,
) -> crate::registry::ToolResult {
    use tokio::io::AsyncWriteExt;

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
        Err(e) => return crate::registry::ToolResult::err(format!("failed to spawn handler: {}", e)),
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(input).await {
            return crate::registry::ToolResult::err(format!("failed to write stdin: {}", e));
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
            crate::registry::ToolResult::err("timeout")
        }
        Ok(Err(e)) => crate::registry::ToolResult::err(format!("process error: {}", e)),
        Ok(Ok((status, out_bytes, err_bytes))) => {
            if status.success() {
                crate::registry::ToolResult::ok(out_bytes)
            } else {
                let stderr = String::from_utf8_lossy(&err_bytes);
                crate::registry::ToolResult::err(stderr.trim().to_string())
            }
        }
    }
}
```

**Step 4: Run tests to verify they pass**

```bash
cargo test -p livefolders invoke_command 2>&1 | tail -20
```

Expected: all 4 invoke_command tests PASS.

**Step 5: Commit**

```bash
git add src/tools/external.rs
git commit -m "feat: add invoke_command helper for running arbitrary handler commands"
```

---

### Task 3: Expose manifest-declared files in readdir and lookup

**Files:**
- Modify: `src/fs/vfs.rs`

**Context:**
Currently, `readdir()` and `lookup_external_file()` only see files that physically exist on disk. Manifest-declared virtual files (e.g., a `search` endpoint backed by a `curl` handler with no disk file) won't appear in `ls` and can't be opened. This task fixes that by merging manifest-declared names into the directory listing and synthesizing `FileAttr` for virtual files.

`LiveFolders` also gains a `timeout_secs: u64` field so vfs.rs can pass the timeout to `invoke_command` in Task 4.

**Step 1: Add timeout_secs to LiveFolders**

In `src/fs/vfs.rs`, find the `LiveFolders` struct and add the field:

```rust
pub struct LiveFolders {
    registry: Arc<RwLock<ToolRegistry>>,
    tools_dir: Option<PathBuf>,
    session: Session,
    write_buf: WriteBuf,
    result_buf: ResultBuf,
    rt: Handle,
    inode_table: InodeTable,
    path_table: PathTable,
    next_ino: Arc<Mutex<u64>>,
    timeout_secs: u64,      // ← add this
}
```

Update `LiveFolders::new()` to accept and store `timeout_secs`:

```rust
pub fn new(
    registry: Arc<RwLock<ToolRegistry>>,
    tools_dir: Option<PathBuf>,
    session: Session,
    rt: Handle,
    timeout_secs: u64,     // ← add this parameter
) -> Self {
    Self {
        registry,
        tools_dir,
        session,
        write_buf: Arc::new(Mutex::new(HashMap::new())),
        result_buf: Arc::new(Mutex::new(HashMap::new())),
        rt,
        inode_table: Arc::new(Mutex::new(HashMap::new())),
        path_table: Arc::new(Mutex::new(HashMap::new())),
        next_ino: Arc::new(Mutex::new(100_000)),
        timeout_secs,      // ← add this
    }
}
```

**Step 2: Find the LiveFolders::new call in main.rs and add the timeout argument**

Open `src/main.rs`. Find the call to `LiveFolders::new(...)` and add the timeout from config. The config struct (from `tools.yaml`) has a `timeout` field in seconds. Add it as the last argument:

```rust
// Find this call and add the timeout:
let fs = LiveFolders::new(registry, tools_dir, session, rt.handle().clone(), config.timeout);
```

If `config.timeout` is of type `u32` or similar, cast it: `config.timeout as u64`.

**Step 3: Add manifest_for_tool and file_spec_for_ino helpers**

Add these two methods to `impl LiveFolders` in `src/fs/vfs.rs`, just before the `Filesystem` impl block:

```rust
fn manifest_for_tool(&self, tool_name: &str) -> Option<crate::manifest::Manifest> {
    let tools_dir = self.tools_dir.as_ref()?;
    crate::manifest::Manifest::load(&tools_dir.join(tool_name)).ok().flatten()
}

/// Given an external inode (>= 100_000), return (tool_name, file_name, FileSpec)
/// if the file is declared in the tool's livefolders.yaml.
fn file_spec_for_ino(&self, ino: u64) -> Option<(String, String, crate::manifest::FileSpec)> {
    let tools_dir = self.tools_dir.as_ref()?;
    let disk_path = self.path_for_ino(ino)?;
    let rel = disk_path.strip_prefix(tools_dir).ok()?;
    let mut parts = rel.components();
    let tool_name = parts.next()?.as_os_str().to_str()?.to_string();
    let file_name = parts.next()?.as_os_str().to_str()?.to_string();
    let manifest = self.manifest_for_tool(&tool_name)?;
    let spec = manifest.spec_for(&file_name)?.clone();
    Some((tool_name, file_name, spec))
}
```

**Step 4: Update lookup_external_file to handle virtual manifest files**

Find `fn lookup_external_file` in `src/fs/vfs.rs` (currently at line 270) and replace it entirely with:

```rust
fn lookup_external_file(&self, tool_name: &str, name: &str) -> Option<FileAttr> {
    use crate::manifest::FileKind;
    let tools_dir = self.tools_dir.as_ref()?;
    let disk_path = tools_dir.join(tool_name).join(name);

    // Check manifest for declared virtual files first.
    if let Some(manifest) = self.manifest_for_tool(tool_name) {
        if let Some(spec) = manifest.spec_for(name) {
            match spec.kind {
                FileKind::WriteInvoke | FileKind::ReadInvoke => {
                    // Virtual file — synthesize attr without requiring a disk file.
                    let ino = self.ino_for_path(&disk_path);
                    let result_size = self.result_buf.lock().unwrap()
                        .get(&ino).map(|r| r.len()).unwrap_or(0) as u64;
                    return Some(Self::file_attr(ino, result_size, 0o644));
                }
                FileKind::Passthrough | FileKind::Readonly => {
                    // Fall through to disk.
                }
            }
        }
    }

    // Fallback: stat the disk file.
    let meta = std::fs::metadata(&disk_path).ok()?;
    let ino = self.ino_for_path(&disk_path);
    use std::os::unix::fs::PermissionsExt;
    let perm = (meta.permissions().mode() as u16) & 0o777;
    if meta.is_dir() {
        Some(Self::dir_attr(ino))
    } else {
        Some(Self::file_attr(ino, meta.len(), perm))
    }
}
```

**Step 5: Update readdir to include manifest-declared virtual files**

Find the `readdir` method's block that handles an external tool directory (around line 716-729 in vfs.rs, the block that does `std::fs::read_dir(&tool_path)`). After the existing `for entry in dir_entries.flatten()` loop that reads disk entries, add a merge pass for manifest-declared names:

```rust
// After the disk listing loop, add manifest-declared virtual files.
if let Some(manifest) = self.manifest_for_tool(&tool_name) {
    for spec in &manifest.files {
        // Skip if a disk file with this name was already listed.
        if entries.iter().any(|(_, _, n)| n == &spec.name) {
            continue;
        }
        let virtual_path = tools_dir.join(&tool_name).join(&spec.name);
        let child_ino = self.ino_for_path(&virtual_path);
        entries.push((child_ino, fuser::FileType::RegularFile, spec.name.clone()));
    }
}
```

The full updated block (replacing the old one starting at `if let Some(tools_dir) = &self.tools_dir {`) becomes:

```rust
if let Some(tools_dir) = &self.tools_dir {
    let tool_path = tools_dir.join(&tool_name);
    if tool_path.is_dir() {
        if let Ok(dir_entries) = std::fs::read_dir(&tool_path) {
            for entry in dir_entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                let fpath = entry.path();
                let child_ino = self.ino_for_path(&fpath);
                let kind = if fpath.is_dir() {
                    fuser::FileType::Directory
                } else {
                    fuser::FileType::RegularFile
                };
                entries.push((child_ino, kind, fname));
            }
        }
        // Merge manifest-declared virtual files (may not exist on disk).
        if let Some(manifest) = self.manifest_for_tool(&tool_name) {
            for spec in &manifest.files {
                if entries.iter().any(|(_, _, n)| n == &spec.name) {
                    continue;
                }
                let virtual_path = tool_path.join(&spec.name);
                let child_ino = self.ino_for_path(&virtual_path);
                entries.push((child_ino, fuser::FileType::RegularFile, spec.name.clone()));
            }
        }
    } else {
        // Built-in tool: use how_to.md + endpoints logic.
        let reg = self.registry.read().unwrap();
        let tool = match reg.get(&tool_name) {
            Some(t) => t,
            None => { reply.ok(); return; }
        };
        entries.push((how_to_ino(idx), fuser::FileType::RegularFile, "how_to.md".to_string()));
        for (ei, ep) in tool.endpoints().iter().enumerate() {
            entries.push((endpoint_ino(idx, ei), fuser::FileType::RegularFile, ep.to_string()));
        }
    }
} else {
    // No tools_dir: all tools are built-in.
    let reg = self.registry.read().unwrap();
    let tool = match reg.get(&tool_name) {
        Some(t) => t,
        None => { reply.ok(); return; }
    };
    entries.push((how_to_ino(idx), fuser::FileType::RegularFile, "how_to.md".to_string()));
    for (ei, ep) in tool.endpoints().iter().enumerate() {
        entries.push((endpoint_ino(idx, ei), fuser::FileType::RegularFile, ep.to_string()));
    }
}
```

**Step 6: Verify it compiles**

```bash
cargo build 2>&1 | grep -E "^error" | head -20
```

Expected: no errors (warnings are OK). Fix any type errors before proceeding.

**Step 7: Commit**

```bash
git add src/fs/vfs.rs src/main.rs
git commit -m "feat: expose manifest-declared virtual files in readdir and lookup"
```

---

### Task 4: Dispatch write/release/read based on FileSpec

**Files:**
- Modify: `src/fs/vfs.rs`

**Context:**
This is the core of the feature. When a write or read arrives for an external-inode file, the FUSE layer now checks the manifest to decide behavior instead of relying on the executable bit alone:

- `write_invoke`: release() invokes handler with written bytes as stdin, stores result. (Replaces executable-bit detection.)
- `read_invoke`: release() stores params (does nothing else); read() invokes handler with stored params, returns result.
- `passthrough`: release() writes to disk; read() reads from disk. (Unchanged.)
- `readonly`: write returns EACCES; read reads from disk. (Unchanged.)

Files with no manifest entry keep the current heuristic (executable bit → write_invoke behavior, regular file → passthrough).

**Step 1: Add use declaration for FileKind and invoke_command**

At the top of `src/fs/vfs.rs`, add:

```rust
use crate::manifest::FileKind;
use crate::tools::external::invoke_command;
```

**Step 2: Update setattr to handle truncation for manifest virtual files**

Find the `setattr` method's truncation block (around line 366-371):

```rust
if let Some(new_size) = size {
    if self.ep_index_for_ino(ino).is_some() {
        self.write_buf.lock().unwrap().entry(ino).or_default().truncate(new_size as usize);
        self.result_buf.lock().unwrap().remove(&ino);
    }
}
```

Replace with:

```rust
if let Some(new_size) = size {
    let is_virtual_endpoint = self.ep_index_for_ino(ino).is_some()
        || self.file_spec_for_ino(ino).map(|(_, _, s)| matches!(s.kind, FileKind::WriteInvoke | FileKind::ReadInvoke)).unwrap_or(false);
    if is_virtual_endpoint {
        self.write_buf.lock().unwrap().entry(ino).or_default().truncate(new_size as usize);
        self.result_buf.lock().unwrap().remove(&ino);
    } else if let Some(disk_path) = self.path_for_ino(ino) {
        // Passthrough file: truncate on disk.
        if new_size == 0 {
            let _ = std::fs::write(&disk_path, b"");
        }
    }
}
```

**Step 3: Update write() to allow writing to manifest virtual files**

Find the `write` method's guard (around line 472):

```rust
if self.ep_index_for_ino(ino).is_none() && self.path_for_ino(ino).is_none() {
    reply.error(libc::EACCES);
    return;
}
```

Replace with:

```rust
let is_writable = self.ep_index_for_ino(ino).is_some()
    || match self.file_spec_for_ino(ino) {
        Some((_, _, spec)) => !matches!(spec.kind, FileKind::Readonly),
        None => self.path_for_ino(ino).is_some(),
    };
if !is_writable {
    reply.error(libc::EACCES);
    return;
}
```

**Step 4: Update release() to dispatch on FileSpec**

Find the `release` method. Locate the block that begins with:

```rust
if let Some(disk_path) = self.path_for_ino(ino) {
```

Replace the entire external-file block (from that line through `reply.ok(); return;` at line 538) with:

```rust
if let Some(disk_path) = self.path_for_ino(ino) {
    // Manifest-declared file: dispatch on declared type.
    if let Some((tool_name, file_name, spec)) = self.file_spec_for_ino(ino) {
        let cwd = self.tools_dir.as_ref()
            .map(|d| d.join(&tool_name))
            .unwrap_or_else(|| disk_path.parent().unwrap_or(Path::new(".")).to_path_buf());
        match spec.kind {
            FileKind::WriteInvoke => {
                let input = self.write_buf.lock().unwrap().remove(&ino).unwrap_or_default();
                if !input.is_empty() {
                    let handler = spec.handler.clone().unwrap_or_default();
                    let timeout = self.timeout_secs;
                    let output = self.rt.block_on(async move {
                        invoke_command(&handler, &input, &tool_name, &file_name, &cwd, timeout).await
                    });
                    let bytes = if output.is_error() {
                        format!("ERROR: {}\n", output.error.unwrap()).into_bytes()
                    } else {
                        output.output
                    };
                    self.result_buf.lock().unwrap().insert(ino, bytes);
                }
                reply.ok();
                return;
            }
            FileKind::ReadInvoke => {
                // Write stores params; read triggers invocation. Nothing to do here.
                reply.ok();
                return;
            }
            FileKind::Passthrough => {
                if let Some(data) = self.write_buf.lock().unwrap().remove(&ino) {
                    let _ = std::fs::write(&disk_path, data);
                }
                reply.ok();
                return;
            }
            FileKind::Readonly => {
                reply.error(libc::EACCES);
                return;
            }
        }
    }

    // No manifest entry: use heuristic (executable bit).
    use std::os::unix::fs::PermissionsExt;
    let is_exec = std::fs::metadata(&disk_path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);
    if !is_exec {
        if let Some(data) = self.write_buf.lock().unwrap().remove(&ino) {
            let _ = std::fs::write(&disk_path, data);
        }
        reply.ok();
        return;
    }
    // Executable file with no manifest: invoke via ExternalTool.
    let input = self.write_buf.lock().unwrap().remove(&ino).unwrap_or_default();
    if !input.is_empty() {
        if let Some(tools_dir) = self.tools_dir.clone() {
            if let Ok(rel) = disk_path.strip_prefix(&tools_dir) {
                let parts: Vec<_> = rel.components().collect();
                if parts.len() >= 2 {
                    let tool_name = parts[0].as_os_str().to_string_lossy().to_string();
                    let ep_name = parts[1].as_os_str().to_string_lossy().to_string();
                    let tool = self.registry.read().unwrap().get(&tool_name);
                    if let Some(tool) = tool {
                        let session = self.session.clone();
                        let output = self.rt.block_on(async move {
                            let result = tool.invoke(&ep_name, &input, &session).await;
                            if result.is_error() {
                                format!("ERROR: {}\n", result.error.unwrap()).into_bytes()
                            } else {
                                result.output
                            }
                        });
                        self.result_buf.lock().unwrap().insert(ino, output);
                    }
                }
            }
        }
    }
    reply.ok();
    return;
}
```

**Step 5: Update read() to dispatch on FileSpec**

Find the `read` method's external-file block (starting with `if let Some(disk_path) = self.path_for_ino(ino) {`). Replace the entire block (lines 391-422, ending with `return;`) with:

```rust
if let Some(disk_path) = self.path_for_ino(ino) {
    // Manifest-declared file: dispatch on declared type.
    if let Some((tool_name, file_name, spec)) = self.file_spec_for_ino(ino) {
        match spec.kind {
            FileKind::ReadInvoke => {
                let handler = spec.handler.clone().unwrap_or_default();
                let input = self.write_buf.lock().unwrap().remove(&ino).unwrap_or_default();
                let cwd = self.tools_dir.as_ref()
                    .map(|d| d.join(&tool_name))
                    .unwrap_or_else(|| disk_path.parent().unwrap_or(Path::new(".")).to_path_buf());
                let timeout = self.timeout_secs;
                let bytes = self.rt.block_on(async move {
                    let result = invoke_command(&handler, &input, &tool_name, &file_name, &cwd, timeout).await;
                    if result.is_error() {
                        format!("ERROR: {}\n", result.error.unwrap()).into_bytes()
                    } else {
                        result.output
                    }
                });
                let start = offset as usize;
                if start >= bytes.len() {
                    reply.data(&[]);
                } else {
                    let end = (start + size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                }
                return;
            }
            FileKind::WriteInvoke => {
                // Return last invocation result (same as executable heuristic path).
                let result = self.result_buf.lock().unwrap().remove(&ino);
                let bytes = result.unwrap_or_default();
                let start = offset as usize;
                if start >= bytes.len() {
                    reply.data(&[]);
                } else {
                    let end = (start + size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                }
                return;
            }
            FileKind::Passthrough | FileKind::Readonly => {
                // Fall through to disk read below.
            }
        }
    }

    // No manifest entry or passthrough/readonly: read from disk.
    match std::fs::read(&disk_path) {
        Ok(bytes) => {
            let start = offset as usize;
            if start >= bytes.len() {
                reply.data(&[]);
            } else {
                let end = (start + size as usize).min(bytes.len());
                reply.data(&bytes[start..end]);
            }
        }
        Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
    }
    return;
}
```

Note: the old executable-heuristic check in `read()` is now removed — for files with no manifest, disk read is the default. Executable files without a manifest entry still work because their result is stored in `result_buf` via the `release()` path (which still falls through to ExternalTool invocation).

**Step 6: Build and fix any compiler errors**

```bash
cargo build 2>&1 | grep -E "^error" | head -30
```

Fix any errors — common issues:
- Missing `use std::path::Path;` import (already there)
- `fuser::FileType` vs `crate::manifest::FileKind` name collision — both are used, they are different types
- `invoke_command` not in scope — ensure `use crate::tools::external::invoke_command;` is at the top

**Step 7: Run all tests**

```bash
cargo test 2>&1 | tail -30
```

Expected: all existing tests pass, no regressions.

**Step 8: Manual end-to-end test**

Create a test tool directory:

```bash
mkdir -p /tmp/test-tool
cat > /tmp/test-tool/livefolders.yaml << 'EOF'
name: test
files:
  - name: greet
    type: read_invoke
    handler: "echo hello, $(cat)"
  - name: shout
    type: write_invoke
    handler: "tr '[:lower:]' '[:upper:]'"
EOF

# Edit tools.yaml to point tools_dir at /tmp and mount
# tools_dir: /tmp
# Then mount LiveFolders and test:

echo "world" > /tmp/livefolders/tools/test/greet
cat /tmp/livefolders/tools/test/greet
# Expected: hello, world

echo "quiet input" > /tmp/livefolders/tools/test/shout
cat /tmp/livefolders/tools/test/shout
# Expected: QUIET INPUT
```

**Step 9: Commit**

```bash
git add src/fs/vfs.rs
git commit -m "feat: dispatch FUSE write/release/read based on manifest FileSpec"
```

---

### Task 5: Update README with new YAML file behaviors

**Files:**
- Modify: `README.md`

**Step 1: Add file behaviors section to README**

In the "External tools" section of `README.md`, find the **File behavior** table:

```markdown
| File type | Behavior |
|---|---|
| `how_to.md` | Served read-only from disk |
| Executable (`chmod +x`) | Write triggers invocation. Stdout becomes the next read result. |
| Regular file | Passthrough — reads and writes go directly to disk |
```

Replace it with:

```markdown
**Declaring file behavior in `livefolders.yaml`**

Add a `files` section to declare how each virtual file behaves:

```yaml
files:
  - name: forecast
    type: read_invoke
    handler: ./bin/forecast         # read triggers handler; write optionally sets params

  - name: search
    type: write_invoke
    handler: "curl -s -X POST -d @- https://api.example.com/search"

  - name: config.json
    type: passthrough               # reads and writes go directly to disk; no handler

  - name: how_to.md
    type: readonly                  # served from disk; writes return EACCES; no handler
```

| Type | Write | Read |
|---|---|---|
| `write_invoke` | Invokes handler (blocks), stores result | Returns last result |
| `read_invoke` | Stores params (non-blocking) | Invokes handler with stored params (blocks) |
| `passthrough` | Writes to disk | Reads from disk |
| `readonly` | Returns error | Reads from disk |

The `handler` is any shell command. LiveFolders passes input via stdin and reads output from stdout:

```bash
handler: ./bin/forecast                         # local script
handler: python3 ./scripts/search.py            # interpreter (no executable bit needed)
handler: "curl -s -X POST -d @- https://api.example.com/search"  # HTTP via curl
```

**Without a `files` section**, LiveFolders falls back to the current heuristic: executable files behave as `write_invoke` (using the file itself as handler), regular files behave as `passthrough`.
```

**Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document YAML file behaviors in README"
```
