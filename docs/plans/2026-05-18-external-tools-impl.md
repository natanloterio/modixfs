# External Tools & Hot-Reload Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow developers in any language to build ModixFS tools as directories of scripts and files, discovered and hot-reloaded at runtime without recompiling.

**Architecture:** External tools live in a `tools_dir` on disk. Each subdirectory is a tool; executable files are subprocess endpoints; non-executable files passthrough to disk. A `notify` watcher updates the registry live. The FUSE layer gains passthrough read/write/create/mkdir/unlink/rename/chmod for non-executable files.

**Tech Stack:** Rust, fuser, tokio, notify crate (cross-platform inotify/kqueue abstraction)

---

## Task 1: Add `tools_dir` and `timeout` to config

**Files:**
- Modify: `src/config.rs`
- Modify: `tools.yaml`

**Step 1: Add fields to Config struct**

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub mount: Option<PathBuf>,
    pub tools_dir: Option<PathBuf>,   // new
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,            // new
    #[serde(default)]
    pub tools: Vec<ToolConfig>,
}

fn default_timeout() -> u64 { 30 }
```

Also add a helper to expand `~` in `tools_dir`:

```rust
impl Config {
    pub fn resolved_tools_dir(&self) -> Option<PathBuf> {
        self.tools_dir.as_ref().map(|p| {
            let s = p.to_string_lossy();
            if s.starts_with("~/") {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(format!("{}/{}", home, &s[2..]))
            } else {
                p.clone()
            }
        })
    }
}
```

**Step 2: Update `tools.yaml`**

```yaml
mount: /tmp/modixfs
tools_dir: ~/.config/modixfs/tools
timeout: 30

tools:
  - name: echo
  - name: github
    # token_env: GITHUB_TOKEN
```

**Step 3: Build to confirm no errors**

```bash
cargo build 2>&1 | grep "^error"
```
Expected: no output.

**Step 4: Commit**

```bash
git add src/config.rs tools.yaml
git commit -m "feat: add tools_dir and timeout_secs to config"
```

---

## Task 2: Add `notify` crate dependency

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add dependency**

```toml
notify = { version = "6", features = ["macos_kqueue"] }
```

**Step 2: Build to confirm it resolves**

```bash
cargo build 2>&1 | grep "^error"
```
Expected: no output.

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add notify crate for filesystem watching"
```

---

## Task 3: Create `ExternalTool` struct

This implements the `Tool` trait by reading the tool directory from disk and spawning subprocesses for executable endpoints.

**Files:**
- Create: `src/tools/external.rs`
- Modify: `src/tools/mod.rs`

**Step 1: Create `src/tools/external.rs`**

```rust
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::registry::{Session, Tool, ToolResult};

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
        // Static description not needed for external tools — how_to.md is read dynamically.
        "External tool"
    }

    fn how_to(&self) -> &str {
        // External tools serve how_to.md directly from disk via passthrough.
        // This method is only used by built-in tools.
        ""
    }

    fn endpoints(&self) -> Vec<&str> {
        // External tools enumerate endpoints dynamically from disk.
        // Returning empty here — FUSE layer handles external tools differently.
        vec![]
    }

    async fn invoke(&self, endpoint: &str, input: &[u8], _session: &Session) -> ToolResult {
        let script = self.endpoint_path(endpoint);

        let mut child = match Command::new(&script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&self.dir)
            .env("MODIXFS_TOOL", &self.name)
            .env("MODIXFS_ENDPOINT", endpoint)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolResult::err(format!("failed to spawn {}: {}", script.display(), e)),
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input).await;
        }

        let result = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await;

        match result {
            Err(_) => ToolResult::err("timeout"),
            Ok(Err(e)) => ToolResult::err(format!("process error: {}", e)),
            Ok(Ok(out)) => {
                if out.status.success() {
                    ToolResult::ok(out.stdout)
                } else {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    ToolResult::err(stderr.trim().to_string())
                }
            }
        }
    }
}
```

**Step 2: Export from `src/tools/mod.rs`**

```rust
mod echo;
mod external;
mod github;

pub use echo::EchoTool;
pub use external::ExternalTool;
pub use github::GitHubTool;
```

**Step 3: Build**

```bash
cargo build 2>&1 | grep "^error"
```
Expected: no output.

**Step 4: Commit**

```bash
git add src/tools/external.rs src/tools/mod.rs
git commit -m "feat: add ExternalTool — subprocess invocation from script files"
```

---

## Task 4: Switch ToolRegistry to RwLock

The registry must be writable at runtime for hot-reload. Change from `Arc<ToolRegistry>` to `Arc<RwLock<ToolRegistry>>` throughout.

**Files:**
- Modify: `src/registry/mod.rs`
- Modify: `src/fs/vfs.rs`
- Modify: `src/main.rs`

**Step 1: Update `src/registry/mod.rs`**

Add `unregister` method:

```rust
pub fn unregister(&mut self, name: &str) {
    self.tools.remove(name);
}
```

No other changes needed — `register` and `unregister` take `&mut self` which is fine behind `RwLock`.

**Step 2: Update `src/fs/vfs.rs`**

Change import and field type:

```rust
use std::sync::{Arc, Mutex, RwLock};
// ...
use crate::registry::{Session, ToolRegistry};

pub struct ModixFS {
    registry: Arc<RwLock<ToolRegistry>>,   // was Arc<ToolRegistry>
    // ... rest unchanged
}

impl ModixFS {
    pub fn new(registry: Arc<RwLock<ToolRegistry>>, session: Session, rt: Handle) -> Self {
```

Every place `self.registry` is used, add `.read().unwrap()`:

```rust
// Before:
self.registry.list()
self.registry.get(name)
self.registry.root_index()

// After:
self.registry.read().unwrap().list()
self.registry.read().unwrap().get(name)
self.registry.read().unwrap().root_index()
```

**Step 3: Update `src/main.rs`**

```rust
use std::sync::{Arc, RwLock};
// ...
let registry = Arc::new(RwLock::new(build_registry(&cfg)));
```

**Step 4: Build**

```bash
cargo build 2>&1 | grep "^error"
```
Expected: no output.

**Step 5: Commit**

```bash
git add src/registry/mod.rs src/fs/vfs.rs src/main.rs
git commit -m "refactor: wrap ToolRegistry in RwLock for hot-reload support"
```

---

## Task 5: Load external tools at startup

Scan `tools_dir` on startup and register an `ExternalTool` per subdirectory.

**Files:**
- Modify: `src/main.rs`

**Step 1: Add `load_external_tools` function**

```rust
fn load_external_tools(cfg: &Config, registry: &mut ToolRegistry) {
    let Some(tools_dir) = cfg.resolved_tools_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&tools_dir) else {
        warn!("tools_dir not found: {}", tools_dir.display());
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        if registry.get(&name).is_some() {
            info!("skipping external tool '{}': shadowed by built-in", name);
            continue;
        }
        registry.register(Arc::new(ExternalTool::new(&name, path, cfg.timeout_secs)));
        info!("registered external tool: {}", name);
    }
}
```

**Step 2: Call it in `cmd_mount` after `build_registry`**

```rust
let mut registry = build_registry(&cfg);
load_external_tools(&cfg, &mut registry);
let registry = Arc::new(RwLock::new(registry));
```

**Step 3: Manual test**

```bash
mkdir -p ~/.config/modixfs/tools/hello
echo "#!/bin/bash\necho \"hello from script, you sent: $(cat -)\"" > ~/.config/modixfs/tools/hello/greet
chmod +x ~/.config/modixfs/tools/hello/greet
echo "# hello\nWrite anything to greet." > ~/.config/modixfs/tools/hello/how_to.md

cargo run -- /tmp/modixfs &
sleep 2
ls /tmp/modixfs/tools/
cat /tmp/modixfs/tools/hello/how_to.md
echo "world" > /tmp/modixfs/tools/hello/greet
sleep 1
cat /tmp/modixfs/tools/hello/greet
```

Expected: `hello from script, you sent: world`

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: load external tools from tools_dir at startup"
```

---

## Task 6: FUSE passthrough for external tool files

External tools have non-executable files (CSV, log, JSON) that should read/write directly to disk. The FUSE layer needs to distinguish between built-in tool endpoints and external tool passthrough files.

**Files:**
- Modify: `src/fs/vfs.rs`

**Background:** Currently the FUSE layer uses deterministic inode arithmetic (`tool_dir_ino`, `endpoint_ino`) which only works for built-in tools with known endpoints. External tools need a different approach: map virtual paths to real disk paths and delegate I/O directly.

**Step 1: Add `tools_dir` to `ModixFS`**

```rust
pub struct ModixFS {
    registry: Arc<RwLock<ToolRegistry>>,
    tools_dir: Option<PathBuf>,          // new
    session: Session,
    write_buf: WriteBuf,
    result_buf: ResultBuf,
    rt: Handle,
}

impl ModixFS {
    pub fn new(
        registry: Arc<RwLock<ToolRegistry>>,
        tools_dir: Option<PathBuf>,
        session: Session,
        rt: Handle,
    ) -> Self { ... }
}
```

Update `cmd_mount` in `main.rs`:

```rust
let fs = ModixFS::new(registry, cfg.resolved_tools_dir(), session, handle);
```

**Step 2: Add path resolution helpers**

```rust
impl ModixFS {
    /// If ino belongs to an external tool's non-executable file, returns its disk path.
    fn external_passthrough_path(&self, ino: u64) -> Option<PathBuf> {
        let tools_dir = self.tools_dir.as_ref()?;
        let reg = self.registry.read().unwrap();
        let tool_idx = self.tool_index_for_ino_with_reg(ino, &reg)?;
        let tool_name = reg.list()[tool_idx];
        // Check if this tool is external (ExternalTool)
        // For passthrough files: ino maps to files that are NOT executable on disk
        // We need to store inode→path mapping for external files
        // ... see step 3
        None
    }
}
```

**Note on inode mapping:** The current deterministic inode scheme works for built-in tools with static endpoint lists. For external tools, files change at runtime. We need a small inode table for external tool files.

**Step 3: Add an inode table for external files**

```rust
use std::collections::HashMap;

/// Maps inode → real disk path for external tool passthrough files.
type InodeTable = Arc<Mutex<HashMap<u64, PathBuf>>>;
/// Maps real disk path → inode (reverse index).
type PathTable = Arc<Mutex<HashMap<PathBuf, u64>>>;

pub struct ModixFS {
    registry: Arc<RwLock<ToolRegistry>>,
    tools_dir: Option<PathBuf>,
    session: Session,
    write_buf: WriteBuf,
    result_buf: ResultBuf,
    rt: Handle,
    inode_table: InodeTable,
    path_table: PathTable,
    next_ino: Arc<Mutex<u64>>,
}
```

Inode allocation for external files starts at 100_000 (well above the deterministic built-in range):

```rust
fn alloc_ino(&self) -> u64 {
    let mut n = self.next_ino.lock().unwrap();
    *n += 1;
    *n
}

fn ino_for_path(&self, path: &PathBuf) -> u64 {
    let mut pt = self.path_table.lock().unwrap();
    if let Some(&ino) = pt.get(path) {
        return ino;
    }
    let ino = self.alloc_ino();
    pt.insert(path.clone(), ino);
    self.inode_table.lock().unwrap().insert(ino, path.clone());
    ino
}

fn path_for_ino(&self, ino: u64) -> Option<PathBuf> {
    self.inode_table.lock().unwrap().get(&ino).cloned()
}
```

Initialize `next_ino` at 100_000 in `ModixFS::new`.

**Step 4: Implement `lookup` for external tool files**

In the `lookup` handler, when `parent` is a tool directory inode and the file is not a known built-in endpoint, check if a real file exists on disk:

```rust
fn lookup_external_file(&self, tool_name: &str, name: &str) -> Option<FileAttr> {
    let tools_dir = self.tools_dir.as_ref()?;
    let disk_path = tools_dir.join(tool_name).join(name);
    let meta = std::fs::metadata(&disk_path).ok()?;
    let ino = self.ino_for_path(&disk_path);
    let perm = meta.permissions().mode() as u16 & 0o777;
    if meta.is_dir() {
        Some(Self::dir_attr(ino))
    } else {
        Some(Self::file_attr(ino, meta.len(), perm))
    }
}
```

**Step 5: Implement `read` for passthrough files**

In the `read` handler, after checking built-in inodes, fall through to:

```rust
if let Some(disk_path) = self.path_for_ino(ino) {
    match std::fs::read(&disk_path) {
        Ok(bytes) => {
            let start = offset as usize;
            if start >= bytes.len() { reply.data(&[]); }
            else {
                let end = (start + size as usize).min(bytes.len());
                reply.data(&bytes[start..end]);
            }
        }
        Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
    }
    return;
}
```

**Step 6: Implement `write` for passthrough files**

In the `write` handler, passthrough files write directly to disk. Use write_buf for accumulation, but flush to disk on `release` instead of invoking a tool:

In `release`, detect passthrough (ino in inode_table, not an endpoint):

```rust
if let Some(disk_path) = self.path_for_ino(ino) {
    if let Some(data) = self.write_buf.lock().unwrap().remove(&ino) {
        let _ = std::fs::write(&disk_path, data);
    }
    reply.ok();
    return;
}
```

**Step 7: Implement `create`**

```rust
fn create(
    &mut self, _req: &Request, parent: u64, name: &OsStr,
    _mode: u32, _umask: u32, _flags: i32, reply: fuser::ReplyCreate,
) {
    if let Some(parent_path) = self.path_for_ino(parent)
        .or_else(|| self.tool_dir_disk_path(parent))
    {
        let disk_path = parent_path.join(name.to_string_lossy().as_ref());
        if std::fs::File::create(&disk_path).is_ok() {
            let ino = self.ino_for_path(&disk_path);
            let attr = Self::file_attr(ino, 0, 0o644);
            reply.created(&TTL, &attr, 0, 0, 0);
            return;
        }
    }
    reply.error(libc::EACCES);
}
```

Where `tool_dir_disk_path` resolves a built-in or external tool dir inode to its disk path:

```rust
fn tool_dir_disk_path(&self, ino: u64) -> Option<PathBuf> {
    let tools_dir = self.tools_dir.as_ref()?;
    let reg = self.registry.read().unwrap();
    let idx = self.tool_index_for_ino_with_reg(ino, &reg)?;
    let name = reg.list()[idx];
    Some(tools_dir.join(name))
}
```

**Step 8: Implement `mkdir`**

```rust
fn mkdir(
    &mut self, _req: &Request, parent: u64, name: &OsStr,
    _mode: u32, _umask: u32, reply: ReplyEntry,
) {
    // Only allow mkdir under TOOLS_DIR_INO (creates a new tool)
    if parent != TOOLS_DIR_INO {
        reply.error(libc::EACCES);
        return;
    }
    let Some(tools_dir) = &self.tools_dir else {
        reply.error(libc::EACCES);
        return;
    };
    let dir_path = tools_dir.join(name.to_string_lossy().as_ref());
    if std::fs::create_dir(&dir_path).is_ok() {
        let ino = self.ino_for_path(&dir_path);
        reply.entry(&TTL, &Self::dir_attr(ino), 0);
    } else {
        reply.error(libc::EIO);
    }
}
```

**Step 9: Implement `unlink` and `rename`**

```rust
fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
    if let Some(parent_path) = self.tool_dir_disk_path(parent) {
        let path = parent_path.join(name.to_string_lossy().as_ref());
        if std::fs::remove_file(&path).is_ok() {
            reply.ok();
        } else {
            reply.error(libc::EIO);
        }
    } else {
        reply.error(libc::EACCES);
    }
}

fn rename(
    &mut self, _req: &Request, parent: u64, name: &OsStr,
    newparent: u64, newname: &OsStr, _flags: u32, reply: fuser::ReplyEmpty,
) {
    let src = self.tool_dir_disk_path(parent).map(|p| p.join(name.to_string_lossy().as_ref()));
    let dst = self.tool_dir_disk_path(newparent).map(|p| p.join(newname.to_string_lossy().as_ref()));
    match (src, dst) {
        (Some(s), Some(d)) => {
            if std::fs::rename(&s, &d).is_ok() { reply.ok(); }
            else { reply.error(libc::EIO); }
        }
        _ => reply.error(libc::EACCES),
    }
}
```

**Step 10: Update `setattr` to write permissions to disk**

In the existing `setattr`, when `mode` is provided and ino maps to a disk path:

```rust
if let Some(mode) = mode {
    if let Some(disk_path) = self.path_for_ino(ino) {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        let _ = std::fs::set_permissions(&disk_path, perms);
    }
}
```

**Step 11: Build**

```bash
cargo build 2>&1 | grep "^error"
```

**Step 12: Manual test — passthrough file**

```bash
# With modixfs running and dataprep tool dir set up:
echo "col1,col2" > /tmp/modixfs/tools/dataprep/output.csv
cat /tmp/modixfs/tools/dataprep/output.csv
# verify file exists on disk:
cat ~/.config/modixfs/tools/dataprep/output.csv
```

**Step 13: Manual test — LLM creates a new tool**

```bash
mkdir /tmp/modixfs/tools/compose
printf '#!/bin/bash\necho "composed: $(cat -)"\n' > /tmp/modixfs/tools/compose/run
chmod +x /tmp/modixfs/tools/compose/run
echo "test" > /tmp/modixfs/tools/compose/run
sleep 1
cat /tmp/modixfs/tools/compose/run
# Expected: composed: test
```

**Step 14: Commit**

```bash
git add src/fs/vfs.rs src/main.rs
git commit -m "feat: FUSE passthrough for external tool files (create/read/write/mkdir/unlink/rename/chmod)"
```

---

## Task 7: Hot-reload watcher

Watch `tools_dir` with `notify` and update the registry when tool directories are added or removed.

**Files:**
- Create: `src/watcher.rs`
- Modify: `src/main.rs`

**Step 1: Create `src/watcher.rs`**

```rust
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::registry::ToolRegistry;
use crate::tools::ExternalTool;

pub fn spawn_watcher(
    tools_dir: PathBuf,
    registry: Arc<RwLock<ToolRegistry>>,
    timeout_secs: u64,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res| { let _ = tx.send(res); },
        notify::Config::default().with_poll_interval(Duration::from_secs(1)),
    ).expect("failed to create watcher");

    watcher.watch(&tools_dir, RecursiveMode::NonRecursive)
        .expect("failed to watch tools_dir");

    tokio::spawn(async move {
        let _watcher = watcher; // keep alive
        while let Some(res) = rx.recv().await {
            match res {
                Ok(event) => handle_event(event, &tools_dir, &registry, timeout_secs),
                Err(e) => warn!("watcher error: {}", e),
            }
        }
    });
}

fn handle_event(
    event: Event,
    tools_dir: &PathBuf,
    registry: &Arc<RwLock<ToolRegistry>>,
    timeout_secs: u64,
) {
    for path in &event.paths {
        // Only care about direct children of tools_dir (tool directories)
        let parent = path.parent();
        if parent != Some(tools_dir.as_path()) { continue; }
        if !path.is_dir() { continue; }

        let name = match path.file_name() {
            Some(n) => n.to_string_lossy().to_string(),
            None => continue,
        };

        match event.kind {
            EventKind::Create(_) => {
                let mut reg = registry.write().unwrap();
                if reg.get(&name).is_none() {
                    reg.register(std::sync::Arc::new(
                        ExternalTool::new(&name, path.clone(), timeout_secs)
                    ));
                    info!("hot-reload: registered tool '{}'", name);
                }
            }
            EventKind::Remove(_) => {
                let mut reg = registry.write().unwrap();
                reg.unregister(&name);
                info!("hot-reload: unregistered tool '{}'", name);
            }
            _ => {}
        }
    }
}
```

**Step 2: Wire into `cmd_mount` in `src/main.rs`**

```rust
mod watcher;
// ...
// After registry is built and tools_dir is known:
if let Some(tools_dir) = cfg.resolved_tools_dir() {
    if tools_dir.exists() {
        watcher::spawn_watcher(tools_dir, Arc::clone(&registry), cfg.timeout_secs);
        info!("hot-reload watcher started");
    }
}
```

This must be called after the Tokio runtime is created (the `rt.handle()` is available) and before `fuser::mount2` blocks.

**Step 3: Build**

```bash
cargo build 2>&1 | grep "^error"
```

**Step 4: Manual test — hot-reload**

```bash
# Start modixfs
GITHUB_TOKEN=... cargo run &
sleep 2

# Verify initial tools
ls /tmp/modixfs/tools/

# Add a new tool at runtime
mkdir ~/.config/modixfs/tools/newlive
echo "#!/bin/bash\necho 'live reload works'" > ~/.config/modixfs/tools/newlive/ping
chmod +x ~/.config/modixfs/tools/newlive/ping
sleep 2

# Should appear without restart
ls /tmp/modixfs/tools/
echo "" > /tmp/modixfs/tools/newlive/ping
sleep 1
cat /tmp/modixfs/tools/newlive/ping
# Expected: live reload works

# Remove it
rm -rf ~/.config/modixfs/tools/newlive
sleep 2
ls /tmp/modixfs/tools/
# newlive should be gone
```

**Step 5: Commit**

```bash
git add src/watcher.rs src/main.rs
git commit -m "feat: hot-reload watcher — register/unregister external tools at runtime"
```

---

## Task 8: Update README

**Files:**
- Modify: `README.md`

**Step 1: Add "External tools" section after "Built-in tools"**

```markdown
## External tools

Any developer can build a ModixFS tool without writing Rust. Create a directory
in your `tools_dir` and add scripts — no recompile, no restart required.

### Directory convention

\`\`\`
~/.config/modixfs/tools/
└── mytool/
    ├── how_to.md        ← LLM reads this to learn the tool
    ├── search           ← executable: write to invoke, read for result
    ├── output.csv       ← passthrough: LLM reads this file directly
    └── config.json      ← passthrough: LLM can write to configure the tool
\`\`\`

### File behavior

| File | Behavior |
|---|---|
| `how_to.md` | Read-only docs |
| Executable (`chmod +x`) | Write → stdin. Stdout → result on next read. |
| Regular file | Passthrough to disk. Reads and writes go directly to the file. |

### Subprocess environment

Scripts receive:
- `stdin` — what the LLM wrote to the endpoint
- `MODIXFS_TOOL` — tool name
- `MODIXFS_ENDPOINT` — endpoint name
- All env vars set when `modixfs` was launched

### Enable in tools.yaml

\`\`\`yaml
tools_dir: ~/.config/modixfs/tools
timeout: 30
\`\`\`

### The LLM can create tools too

\`\`\`bash
mkdir /tools/mytool
echo "# My Tool\n..." > /tools/mytool/how_to.md
printf '#!/bin/bash\ncurl -s https://api.example.com -d "$(cat -)"\n' > /tools/mytool/fetch
chmod +x /tools/mytool/fetch
# tool is immediately live
\`\`\`
```

**Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document external tools and hot-reload in README"
```

---

## Task 9: Tag and release

```bash
git tag v0.2.0
git push origin master --tags
```

CI builds and publishes binaries for Linux x86_64/ARM64 and macOS x86_64/ARM64 automatically.
