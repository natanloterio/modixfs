use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    ReplyWrite, Request,
};
use libc::{ENOENT, ENOTDIR};
use tokio::runtime::Handle;
use tracing::debug;

use crate::manifest::FileKind;
use crate::registry::{Session, ToolRegistry};
use crate::tools::{invoke_command_validated, ExternalTool};

use super::inode::*;
use super::invocation::{EndpointSnapshot, InvocationState, SlotHandle};
use super::root_doc::{ROOT_CREATE_TOOL, ROOT_HOW_TO};
use super::sid::caller_sid;
use super::slot_table::SlotTable;

const TTL: Duration = Duration::from_secs(1);

/// What the read state machine should do next.
enum ReadAction {
    Kick,
    Wait(std::sync::Arc<tokio::sync::Notify>),
    Slice,
}

/// Inode → disk path mapping for external tool files (inodes >= 100_000).
type InodeTable = Arc<Mutex<HashMap<u64, PathBuf>>>;

/// Disk path → inode mapping for external tool files.
type PathTable = Arc<Mutex<HashMap<PathBuf, u64>>>;

pub struct LiveFolders {
    registry: Arc<RwLock<ToolRegistry>>,
    tools_dir: Option<PathBuf>,
    mount_path: PathBuf,
    session: Session,
    /// Per-(ino, sid) invocation state. Routes echo and cat in the same
    /// shell pipeline to the same slot; isolates pipelines from different
    /// shells.
    slots: Arc<SlotTable>,
    /// Map from FUSE file handle to the caller's sid captured at
    /// `open()` time. FUSE issues some later operations (notably
    /// `release` and async `read`) with `pid == 0`, for which
    /// `getsid` would return the daemon's own sid. By stashing the
    /// real sid at open time and looking it up by fh, every op in the
    /// open's lifecycle resolves to the same correct slot.
    fh_to_sid: Arc<Mutex<HashMap<u64, i32>>>,
    /// Monotonic file handle allocator.
    next_fh: Arc<AtomicU64>,
    rt: Handle,
    inode_table: InodeTable,
    path_table: PathTable,
    next_ino: Arc<Mutex<u64>>,
    timeout_secs: u64,
    sandbox_mode: crate::sandbox::SandboxMode,
    /// Bumped by the watcher on every tool registry change. The current
    /// value is captured into a slot's EndpointSnapshot at slot creation
    /// time, so an in-flight invocation always runs against a stable
    /// snapshot even if the manifest is reloaded mid-flight.
    manifest_version: Arc<AtomicU64>,
}

impl LiveFolders {
    pub fn new(
        registry: Arc<RwLock<ToolRegistry>>,
        tools_dir: Option<PathBuf>,
        mount_path: PathBuf,
        session: Session,
        rt: Handle,
        timeout_secs: u64,
        sandbox_mode: crate::sandbox::SandboxMode,
    ) -> Self {
        Self {
            registry,
            tools_dir,
            mount_path,
            session,
            slots: Arc::new(SlotTable::new()),
            fh_to_sid: Arc::new(Mutex::new(HashMap::new())),
            next_fh: Arc::new(AtomicU64::new(1)),
            rt,
            inode_table: Arc::new(Mutex::new(HashMap::new())),
            path_table: Arc::new(Mutex::new(HashMap::new())),
            next_ino: Arc::new(Mutex::new(100_000)),
            timeout_secs,
            sandbox_mode,
            manifest_version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a handle to the SlotTable so the watcher (or tests) can
    /// observe live invocation state.
    #[allow(dead_code)]
    pub fn slots(&self) -> Arc<SlotTable> {
        self.slots.clone()
    }

    /// Spawns a background task that periodically reaps slots whose
    /// last touch is older than `max_idle`. Returns the JoinHandle so
    /// callers can keep or detach it; the task lives for as long as the
    /// SlotTable Arc is reachable.
    pub fn spawn_reaper(&self, interval: Duration, max_idle: Duration) -> tokio::task::JoinHandle<()> {
        let slots = self.slots.clone();
        self.rt.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let reaped = slots.reap_idle(max_idle);
                if reaped > 0 {
                    tracing::warn!(count = reaped, "reaped {} idle invocation slot(s)", reaped);
                }
            }
        })
    }

    /// Bump the manifest version. The watcher calls this on every
    /// reload; in-flight slots keep their snapshot, but the next slot
    /// created after the bump sees the new version.
    #[allow(dead_code)]
    pub fn bump_manifest_version(&self) {
        self.manifest_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Build an EndpointSnapshot for a manifest-declared endpoint inode.
    /// Returns None when the inode has no manifest spec (built-in tools
    /// and the executable-heuristic legacy path use `synth_snapshot`).
    fn snapshot_for_ino(&self, ino: u64) -> Option<EndpointSnapshot> {
        let (tool_name, file_name, spec) = self.file_spec_for_ino(ino)?;
        let cwd = self
            .tools_dir
            .as_ref()
            .map(|d| d.join(&tool_name))
            .unwrap_or_else(|| PathBuf::from("."));
        let state_file = spec.state_file.clone().map(|sf| cwd.join(sf));
        Some(EndpointSnapshot {
            tool_name,
            file_name,
            cwd,
            kind: spec.kind,
            handler: spec.handler.clone(),
            input_schema: spec.input.clone(),
            state_file,
            pipe: spec.pipe.clone(),
            timeout_secs: self.timeout_secs,
            manifest_version: self.manifest_version.load(Ordering::Relaxed),
        })
    }

    /// Build a placeholder snapshot for non-manifest invocation paths
    /// (built-in tools, executable-heuristic legacy path). The kind is
    /// WriteInvoke so the read state machine drains slot.result after
    /// release. tool_name and file_name are populated for diagnostics.
    fn synth_snapshot(&self, tool_name: &str, file_name: &str, cwd: PathBuf) -> EndpointSnapshot {
        EndpointSnapshot {
            tool_name: tool_name.to_string(),
            file_name: file_name.to_string(),
            cwd,
            kind: FileKind::WriteInvoke,
            handler: None,
            input_schema: None,
            state_file: None,
            pipe: None,
            timeout_secs: self.timeout_secs,
            manifest_version: self.manifest_version.load(Ordering::Relaxed),
        }
    }

    /// Look up or create the slot for a manifest endpoint. Returns None
    /// if the inode has no manifest spec.
    fn slot_for_manifest(&self, ino: u64, sid: i32) -> Option<SlotHandle> {
        if let Some(h) = self.slots.get((ino, sid)) {
            return Some(h);
        }
        let snap = self.snapshot_for_ino(ino)?;
        Some(self.slots.get_or_create((ino, sid), move || snap))
    }

    /// Look up or create a slot using a caller-supplied synthetic
    /// snapshot, for built-in tools and the executable-heuristic path.
    fn slot_for_synth(&self, ino: u64, sid: i32, snap_fn: impl FnOnce() -> EndpointSnapshot) -> SlotHandle {
        if let Some(h) = self.slots.get((ino, sid)) {
            return h;
        }
        self.slots.get_or_create((ino, sid), snap_fn)
    }

    /// Resolves the sid for an op:
    /// 1. if the op carries a non-zero fh and that fh was registered at
    ///    open() time, return the captured sid (most reliable),
    /// 2. else ask the kernel via `getsid(req.pid())`,
    /// 3. else 0 (the shared default slot).
    ///
    /// FUSE sometimes issues `release` and async `read` with
    /// `pid == 0`. For those, the fh-based path is the only reliable
    /// route back to the caller's session.
    fn resolve_sid(&self, req: &Request, fh: u64) -> i32 {
        if fh != 0
            && let Some(sid) = self.fh_to_sid.lock().unwrap().get(&fh).copied()
        {
            return sid;
        }
        caller_sid(req).unwrap_or(0)
    }

    /// Allocates a new file handle and binds it to the caller's sid.
    fn alloc_fh_for(&self, req: &Request) -> u64 {
        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        if let Some(sid) = caller_sid(req) {
            self.fh_to_sid.lock().unwrap().insert(fh, sid);
        }
        fh
    }

    /// Drops the fh→sid mapping. Called from `release`.
    fn drop_fh(&self, fh: u64) {
        if fh != 0 {
            self.fh_to_sid.lock().unwrap().remove(&fh);
        }
    }

    /// Look up or create a slot for any invocable endpoint, regardless of
    /// whether it is manifest-declared or a built-in. Returns None when
    /// the inode is not an endpoint at all.
    fn slot_for_endpoint(&self, ino: u64, sid: i32) -> Option<SlotHandle> {
        if let Some(h) = self.slot_for_manifest(ino, sid) {
            return Some(h);
        }
        if let Some((tool_idx, ep_idx)) = self.ep_index_for_ino(ino) {
            let (tool_name, file_name) = {
                let reg = self.registry.read().unwrap();
                let tool_name = reg.list()[tool_idx].to_string();
                let tool = reg.get(&tool_name)?;
                let file_name = tool.endpoints()[ep_idx].to_string();
                (tool_name, file_name)
            };
            let cwd = self
                .tools_dir
                .as_ref()
                .map(|d| d.join(&tool_name))
                .unwrap_or_else(|| PathBuf::from("."));
            return Some(self.slot_for_synth(ino, sid, || {
                self.synth_snapshot(&tool_name, &file_name, cwd)
            }));
        }
        None
    }

    fn system_prompt_content(&self) -> Vec<u8> {
        let mount_str = self.mount_path.to_string_lossy();
        self.registry
            .read()
            .unwrap()
            .system_prompt(&mount_str, self.tools_dir.as_deref())
            .into_bytes()
    }

    fn alloc_ino(&self) -> u64 {
        let mut n = self.next_ino.lock().unwrap();
        *n += 1;
        *n
    }

    fn ino_for_path(&self, path: &Path) -> u64 {
        let mut pt = self.path_table.lock().unwrap();
        if let Some(&ino) = pt.get(path) {
            return ino;
        }
        let ino = self.alloc_ino();
        pt.insert(path.to_path_buf(), ino);
        self.inode_table.lock().unwrap().insert(ino, path.to_path_buf());
        ino
    }

    fn path_for_ino(&self, ino: u64) -> Option<PathBuf> {
        self.inode_table.lock().unwrap().get(&ino).cloned()
    }

    fn tool_dir_disk_path(&self, ino: u64) -> Option<PathBuf> {
        let tools_dir = self.tools_dir.as_ref()?;
        let reg = self.registry.read().unwrap();
        let idx = self.tool_index_for_ino(ino)?;
        let name = reg.list()[idx].to_string();
        Some(tools_dir.join(name))
    }

    fn tool_index_by_name(&self, name: &str) -> Option<usize> {
        self.registry.read().unwrap().list().iter().position(|&n| n == name)
    }

    fn tool_index_for_ino(&self, ino: u64) -> Option<usize> {
        if !(1000..100_000).contains(&ino) {
            return None;
        }
        let idx = ((ino - 1000) / 100) as usize;
        if idx < self.registry.read().unwrap().list().len() {
            Some(idx)
        } else {
            None
        }
    }

    fn ep_index_for_ino(&self, ino: u64) -> Option<(usize, usize)> {
        let tool_idx = self.tool_index_for_ino(ino)?;
        let base = tool_dir_ino(tool_idx);
        if ino < base + 10 {
            return None; // how_to or dir itself
        }
        let ep_idx = (ino - base - 10) as usize;
        let registry = self.registry.read().unwrap();
        let tool_name = registry.list()[tool_idx];
        let tool = registry.get(tool_name)?;
        if ep_idx < tool.endpoints().len() {
            Some((tool_idx, ep_idx))
        } else {
            None
        }
    }

    fn dir_attr(ino: u64) -> FileAttr {
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o755,
            nlink: 2,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    fn file_attr(ino: u64, size: u64, perm: u16) -> FileAttr {
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm,
            nlink: 1,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }

    fn resolve_ino_attr(&self, ino: u64) -> Option<FileAttr> {
        match ino {
            ROOT_INO => Some(Self::dir_attr(ROOT_INO)),
            TOOLS_DIR_INO => Some(Self::dir_attr(TOOLS_DIR_INO)),
            ROOT_INDEX_INO => {
                let content = self.registry.read().unwrap().root_index();
                Some(Self::file_attr(ROOT_INDEX_INO, content.len() as u64, 0o444))
            }
            ROOT_HOW_TO_INO => {
                Some(Self::file_attr(ROOT_HOW_TO_INO, ROOT_HOW_TO.len() as u64, 0o444))
            }
            ROOT_CREATE_TOOL_INO => {
                Some(Self::file_attr(ROOT_CREATE_TOOL_INO, ROOT_CREATE_TOOL.len() as u64, 0o444))
            }
            ROOT_SYSTEM_PROMPT_INO => {
                let len = self.system_prompt_content().len() as u64;
                Some(Self::file_attr(ROOT_SYSTEM_PROMPT_INO, len, 0o444))
            }
            _ => {
                // External inode (>= 100_000)?
                if let Some(disk_path) = self.path_for_ino(ino) {
                    // If the disk file is absent but is named how_to.md, generate from folder.yaml.
                    if !disk_path.exists()
                        && disk_path.file_name().is_some_and(|n| n == "how_to.md")
                        && let Some(tool_dir) = disk_path.parent()
                            && let Ok(Some(manifest)) = crate::manifest::Manifest::load(tool_dir) {
                                let content = crate::fs::how_to_gen::generate_how_to(&manifest);
                                return Some(Self::file_attr(ino, content.len() as u64, 0o444));
                            }
                    if let Ok(meta) = std::fs::metadata(&disk_path) {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = meta.permissions().mode();
                        let perm = (mode as u16) & 0o777;
                        if meta.is_dir() {
                            return Some(Self::dir_attr(ino));
                        }
                        let is_exec = mode & 0o111 != 0;
                        // For executable tools (built-in invocation path)
                        // and virtual endpoints we report size 0 and rely
                        // on FOPEN_DIRECT_IO so read() drives EOF, not the
                        // cached file size.
                        let size = if is_exec { 0 } else { meta.len() };
                        return Some(Self::file_attr(ino, size, perm));
                    }
                    // Virtual file (no disk counterpart): check manifest for write_invoke / read_invoke.
                    if let Some((_, _, spec)) = self.file_spec_for_ino(ino) {
                        use crate::manifest::FileKind;
                        match spec.kind {
                            FileKind::WriteInvoke | FileKind::ReadInvoke => {
                                return Some(Self::file_attr(ino, 0, 0o644));
                            }
                            _ => {}
                        }
                    }
                }
                // tool dir?
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    let base = tool_dir_ino(idx);
                    if ino == base {
                        return Some(Self::dir_attr(ino));
                    }
                    // how_to?
                    if ino == how_to_ino(idx) {
                        let registry = self.registry.read().unwrap();
                        let name = registry.list()[idx];
                        let tool = registry.get(name)?;
                        let size = tool.how_to().len() as u64;
                        return Some(Self::file_attr(ino, size, 0o444));
                    }
                    // endpoint?
                    if let Some((_ti, _ei)) = self.ep_index_for_ino(ino) {
                        // Size reported as 0: read() drives EOF via FOPEN_DIRECT_IO.
                        return Some(Self::file_attr(ino, 0, 0o644));
                    }
                }
                None
            }
        }
    }

    fn lookup_in_root(&self, name: &OsStr) -> Option<FileAttr> {
        let s = name.to_str()?;
        match s {
            "how_to.md" => {
                Some(Self::file_attr(ROOT_HOW_TO_INO, ROOT_HOW_TO.len() as u64, 0o444))
            }
            "create_tool.md" => {
                Some(Self::file_attr(ROOT_CREATE_TOOL_INO, ROOT_CREATE_TOOL.len() as u64, 0o444))
            }
            "system_prompt.md" => {
                let len = self.system_prompt_content().len() as u64;
                Some(Self::file_attr(ROOT_SYSTEM_PROMPT_INO, len, 0o444))
            }
            "index.md" => {
                let content = self.registry.read().unwrap().root_index();
                Some(Self::file_attr(ROOT_INDEX_INO, content.len() as u64, 0o444))
            }
            "tools" => Some(Self::dir_attr(TOOLS_DIR_INO)),
            _ => None,
        }
    }

    fn lookup_in_tools(&self, name: &OsStr) -> Option<FileAttr> {
        let s = name.to_str()?;
        let idx = self.tool_index_by_name(s)?;
        Some(Self::dir_attr(tool_dir_ino(idx)))
    }

    fn lookup_in_tool_dir(&self, tool_ino: u64, name: &OsStr) -> Option<FileAttr> {
        let s = name.to_str()?;
        let idx = self.tool_index_for_ino(tool_ino)?;
        let registry = self.registry.read().unwrap();
        let tool_name = registry.list()[idx];
        let tool = registry.get(tool_name)?;

        if s == "how_to.md" {
            let how_to = tool.how_to();
            if how_to.is_empty() {
                // External tool: how_to.md lives on disk — fall through to lookup_external_file
                return None;
            }
            let size = how_to.len() as u64;
            return Some(Self::file_attr(how_to_ino(idx), size, 0o444));
        }

        let ep_pos = tool.endpoints().iter().position(|&e| e == s)?;
        let ino = endpoint_ino(idx, ep_pos);
        // Size reported as 0: read() drives EOF via FOPEN_DIRECT_IO.
        Some(Self::file_attr(ino, 0, 0o644))
    }

    fn manifest_for_tool(&self, tool_name: &str) -> Option<crate::manifest::Manifest> {
        let tools_dir = self.tools_dir.as_ref()?;
        let manifest = crate::manifest::Manifest::load(&tools_dir.join(tool_name)).ok().flatten()?;
        if let Err(e) = manifest.validate() {
            tracing::warn!("manifest for '{}' is invalid: {}", tool_name, e);
            return None;
        }
        Some(manifest)
    }

    /// Given an external inode (>= 100_000), return (tool_name, file_name, FileSpec)
    /// if the file is declared in the tool's folder.yaml.
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

    fn lookup_external_file(&self, tool_name: &str, name: &str) -> Option<FileAttr> {
        use crate::manifest::FileKind;
        let tools_dir = self.tools_dir.as_ref()?;
        let disk_path = tools_dir.join(tool_name).join(name);

        // .log file: synthesize attr from the most recent trace for the endpoint inode.
        if let Some(ep_name) = name.strip_suffix(".log") {
            if let Some(manifest) = self.manifest_for_tool(tool_name)
                && let Some(spec) = manifest.spec_for(ep_name)
                && matches!(spec.kind, FileKind::WriteInvoke | FileKind::ReadInvoke) {
                    let ep_path = tools_dir.join(tool_name).join(ep_name);
                    let ep_ino = self.ino_for_path(&ep_path);
                    let size = self.slots.latest_trace_for_ino(ep_ino)
                        .map(|t| t.len()).unwrap_or(0) as u64;
                    let log_ino = self.ino_for_path(&disk_path);
                    return Some(Self::file_attr(log_ino, size, 0o444));
                }
        }

        // schema.json: synthesize from folder.yaml.
        if name == "schema.json"
            && let Ok(Some(manifest)) = crate::manifest::Manifest::load(&tools_dir.join(tool_name)) {
                let content = crate::fs::schema_gen::generate_schema_json(&manifest);
                let ino = self.ino_for_path(&disk_path);
                return Some(Self::file_attr(ino, content.len() as u64, 0o444));
            }

        // anthropic_tools.json: synthesize from folder.yaml.
        if name == "anthropic_tools.json"
            && let Ok(Some(manifest)) = crate::manifest::Manifest::load(&tools_dir.join(tool_name)) {
                let content = crate::fs::schema_gen::generate_anthropic_tools_json(tool_name, &manifest);
                let ino = self.ino_for_path(&disk_path);
                return Some(Self::file_attr(ino, content.len() as u64, 0o444));
            }

        // For virtual files (write_invoke / read_invoke) declared in the manifest,
        // synthesize an attr without requiring a disk file. Size is 0 — read()
        // drives EOF via FOPEN_DIRECT_IO.
        if let Some(manifest) = self.manifest_for_tool(tool_name)
            && let Some(spec) = manifest.spec_for(name) {
                match spec.kind {
                    FileKind::WriteInvoke | FileKind::ReadInvoke => {
                        let ino = self.ino_for_path(&disk_path);
                        return Some(Self::file_attr(ino, 0, 0o644));
                    }
                    FileKind::Passthrough | FileKind::Readonly => {}
                }
            }

        // how_to.md: synthesize if absent on disk.
        if name == "how_to.md" && !disk_path.exists()
            && let Ok(Some(manifest)) = crate::manifest::Manifest::load(&tools_dir.join(tool_name)) {
                let content = crate::fs::how_to_gen::generate_how_to(&manifest);
                let ino = self.ino_for_path(&disk_path);
                return Some(Self::file_attr(ino, content.len() as u64, 0o444));
            }

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

    /// Formats an invocation trace for the `.log` virtual file.
    fn format_trace(duration_ms: u64, is_error: bool, stderr: &[u8]) -> Vec<u8> {
        let exit_str = if is_error { "error" } else { "ok" };
        let stderr_str = String::from_utf8_lossy(stderr);
        let stderr_str = stderr_str.trim();
        format!("duration_ms: {}\nexit: {}\nstderr: {}\n", duration_ms, exit_str, stderr_str)
            .into_bytes()
    }

    /// Writes a finished ToolResult into a slot: stores the bytes,
    /// trace, flips state to Ready, and notifies any awaiters. Standalone
    /// fn so it can be called from spawned tasks without `&self`.
    fn commit_result_into(handle: &SlotHandle, result: crate::registry::ToolResult) {
        let trace = Self::format_trace(result.duration_ms, result.is_error(), &result.stderr);
        let bytes = if result.is_error() {
            format!("{}\n", result.error.unwrap()).into_bytes()
        } else {
            result.output
        };
        let notify_to_wake = {
            let mut s = handle.lock().unwrap();
            s.result = bytes;
            s.trace = trace;
            let prev = std::mem::replace(&mut s.state, InvocationState::Ready);
            s.touch();
            match prev {
                InvocationState::Pending(n) => Some(n),
                _ => None,
            }
        };
        if let Some(n) = notify_to_wake {
            n.notify_waiters();
        }
    }

    /// Async invocation runner: takes input out of the slot's write_buf
    /// and runs the handler (or pipe) against the slot's pinned
    /// snapshot. Returns the ToolResult; the caller commits it via
    /// `commit_result_into`.
    async fn run_invocation_async(handle: SlotHandle) -> crate::registry::ToolResult {
        let (snapshot, input) = {
            let mut s = handle.lock().unwrap();
            let snap = s.snapshot.clone();
            let input = std::mem::take(&mut s.write_buf);
            (snap, input)
        };

        if let Some(stages) = snapshot.pipe.as_ref() {
            let stages = stages.clone();
            let cwd = snapshot.cwd.clone();
            let tool_name = snapshot.tool_name.clone();
            let timeout = snapshot.timeout_secs;
            match crate::manifest::Manifest::load(&cwd) {
                Ok(Some(m)) => {
                    let sandbox = crate::sandbox::build(
                        m.sandbox.as_ref(),
                        crate::sandbox::SandboxMode::Disabled,
                    );
                    crate::tools::invoke_pipe(
                        &stages, &input, &m, &tool_name, &cwd, timeout, sandbox.as_ref(),
                    ).await
                }
                _ => crate::registry::ToolResult::err("[ERROR:SPAWN] manifest not found"),
            }
        } else {
            let handler = snapshot.handler.clone().unwrap_or_default();
            invoke_command_validated(
                &handler,
                &input,
                &snapshot.tool_name,
                &snapshot.file_name,
                &snapshot.cwd,
                snapshot.timeout_secs,
                snapshot.input_schema.as_ref(),
                snapshot.state_file.as_deref(),
            ).await
        }
    }

    /// Marks the slot as Pending and returns the Notify that will be
    /// signaled when the invocation completes. Called before spawning
    /// the invocation task.
    fn mark_pending(handle: &SlotHandle) -> std::sync::Arc<tokio::sync::Notify> {
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut s = handle.lock().unwrap();
        s.state = InvocationState::Pending(notify.clone());
        s.touch();
        notify
    }
}

fn reply_bytes(reply: fuser::ReplyData, bytes: &[u8], offset: i64, size: u32) {
    let start = offset as usize;
    if start >= bytes.len() {
        reply.data(&[]);
    } else {
        let end = (start + size as usize).min(bytes.len());
        reply.data(&bytes[start..end]);
    }
}

impl Filesystem for LiveFolders {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup parent={} name={:?}", parent, name);
        let attr = match parent {
            ROOT_INO => self.lookup_in_root(name),
            TOOLS_DIR_INO => self.lookup_in_tools(name),
            _ => {
                if let Some(idx) = self.tool_index_for_ino(parent) {
                    let base = tool_dir_ino(idx);
                    if parent == base {
                        let reg = self.registry.read().unwrap();
                        let tool_name = reg.list()[idx].to_string();
                        drop(reg);
                        self.lookup_in_tool_dir(parent, name)
                            .or_else(|| self.lookup_external_file(&tool_name, name.to_str().unwrap_or("")))
                    } else {
                        None
                    }
                } else {
                    // parent might be an external path (inode_table entry)
                    if let Some(parent_path) = self.path_for_ino(parent) {
                        let name_str = name.to_str().unwrap_or("");

                        // If the parent is a direct tool directory (one level under tools_dir),
                        // delegate to lookup_external_file so manifest-declared virtual files
                        // (write_invoke / read_invoke) resolve correctly even when the parent
                        // was assigned a dynamic inode before the tool was registered.
                        if let Some(tools_dir) = self.tools_dir.clone() {
                            if let Ok(rel) = parent_path.strip_prefix(&tools_dir) {
                                let mut comps = rel.components();
                                if let (Some(tool_comp), None) = (comps.next(), comps.next()) {
                                    let tool_name = tool_comp.as_os_str().to_string_lossy().to_string();
                                    if let Some(attr) = self.lookup_external_file(&tool_name, name_str) {
                                        return reply.entry(&TTL, &attr, 0);
                                    }
                                }
                            }
                        }

                        let disk_path = parent_path.join(name_str);
                        if let Ok(meta) = std::fs::metadata(&disk_path) {
                            let ino = self.ino_for_path(&disk_path);
                            use std::os::unix::fs::PermissionsExt;
                            let perm = (meta.permissions().mode() as u16) & 0o777;
                            if meta.is_dir() {
                                Some(Self::dir_attr(ino))
                            } else {
                                Some(Self::file_attr(ino, meta.len(), perm))
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
            }
        };

        match attr {
            Some(a) => reply.entry(&TTL, &a, 0),
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.resolve_ino_attr(ino) {
            Some(a) => reply.attr(&TTL, &a),
            None => reply.error(ENOENT),
        }
    }

    fn setattr(
        &mut self,
        req: &Request,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // Write permissions to disk for external files
        if let Some(mode) = mode
            && let Some(disk_path) = self.path_for_ino(ino) {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(mode);
                let _ = std::fs::set_permissions(&disk_path, perms);
            }

        // Handle truncation on O_TRUNC open (e.g. shell `>` redirect).
        // Scope the truncation to the caller's session id so two concurrent
        // shells redirecting to the same endpoint do not clobber each other.
        if let Some(new_size) = size {
            let is_virtual_endpoint = self.ep_index_for_ino(ino).is_some()
                || self.file_spec_for_ino(ino)
                    .map(|(_, _, s)| matches!(s.kind, FileKind::WriteInvoke | FileKind::ReadInvoke))
                    .unwrap_or(false);
            if is_virtual_endpoint {
                let sid = self.resolve_sid(req, fh.unwrap_or(0));
                if let Some(handle) = self.slot_for_endpoint(ino, sid) {
                    let mut s = handle.lock().unwrap();
                    s.write_buf.truncate(new_size as usize);
                    s.result.clear();
                    s.trace.clear();
                    s.state = InvocationState::Idle;
                    s.touch();
                }
            } else if let Some(disk_path) = self.path_for_ino(ino)
                && new_size == 0 {
                    let _ = std::fs::write(&disk_path, b"");
                }
        }
        match self.resolve_ino_attr(ino) {
            Some(a) => reply.attr(&TTL, &a),
            None => reply.error(ENOENT),
        }
    }

    fn open(&mut self, req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        // Bypass the kernel page cache for every invocable endpoint so:
        //   (a) read() is always called even when getattr reports size 0; and
        //   (b) two opens by different sessions never share a cached payload.
        let is_endpoint = self.file_spec_for_ino(ino)
            .map(|(_, _, s)| matches!(s.kind, FileKind::WriteInvoke | FileKind::ReadInvoke))
            .unwrap_or(false)
            || self.ep_index_for_ino(ino).is_some();
        if is_endpoint {
            let fh = self.alloc_fh_for(req);
            reply.opened(fh, fuser::consts::FOPEN_DIRECT_IO);
            return;
        }
        // For non-endpoint files (passthrough disk, readonly, etc) we also
        // allocate an fh so writes get routed to the caller's session.
        let fh = self.alloc_fh_for(req);
        reply.opened(fh, 0);
    }

    fn read(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        let sid = self.resolve_sid(req, fh);
        // External file on disk: dispatch on manifest FileSpec or fall back to disk read.
        if let Some(disk_path) = self.path_for_ino(ino) {
            // Manifest-declared file: dispatch on declared type.
            if let Some((_, _, spec)) = self.file_spec_for_ino(ino) {
                match spec.kind {
                    FileKind::ReadInvoke => {
                        let Some(handle) = self.slot_for_manifest(ino, sid) else {
                            reply.error(ENOENT);
                            return;
                        };
                        let slots = self.slots.clone();
                        // First-read kicks the handler; subsequent reads
                        // (and reads from concurrent fhs on the same slot)
                        // wait via the slot's Notify. Spawned task owns
                        // the reply so the FUSE dispatcher returns now.
                        let action = {
                            let s = handle.lock().unwrap();
                            match &s.state {
                                InvocationState::Idle => ReadAction::Kick,
                                InvocationState::Pending(n) => ReadAction::Wait(n.clone()),
                                InvocationState::Ready => ReadAction::Slice,
                            }
                        };
                        let (notify, needs_kick) = match action {
                            ReadAction::Kick => (Some(Self::mark_pending(&handle)), true),
                            ReadAction::Wait(n) => (Some(n), false),
                            ReadAction::Slice => (None, false),
                        };
                        let handle_for_task = handle.clone();
                        self.rt.spawn(async move {
                            if needs_kick {
                                let h2 = handle_for_task.clone();
                                tokio::spawn(async move {
                                    let result = Self::run_invocation_async(h2.clone()).await;
                                    Self::commit_result_into(&h2, result);
                                });
                            }
                            if let Some(n) = notify {
                                n.notified().await;
                            }
                            let (bytes, past_end) = {
                                let mut s = handle_for_task.lock().unwrap();
                                s.touch();
                                let bytes = s.slice(offset, size).to_vec();
                                let past_end = (offset as usize) >= s.result.len();
                                (bytes, past_end)
                            };
                            if past_end {
                                slots.remove((ino, sid));
                            }
                            reply.data(&bytes);
                        });
                        return;
                    }
                    FileKind::WriteInvoke => {
                        // Result was set by release(). If release's
                        // invocation is still in flight, wait for it.
                        let Some(handle) = self.slot_for_manifest(ino, sid) else {
                            reply.error(ENOENT);
                            return;
                        };
                        let slots = self.slots.clone();
                        let action = {
                            let s = handle.lock().unwrap();
                            match &s.state {
                                InvocationState::Pending(n) => ReadAction::Wait(n.clone()),
                                _ => ReadAction::Slice,
                            }
                        };
                        let notify = match action {
                            ReadAction::Wait(n) => Some(n),
                            _ => None,
                        };
                        let handle_for_task = handle.clone();
                        self.rt.spawn(async move {
                            if let Some(n) = notify {
                                n.notified().await;
                            }
                            let (bytes, past_end) = {
                                let mut s = handle_for_task.lock().unwrap();
                                s.touch();
                                let bytes = s.slice(offset, size).to_vec();
                                let past_end = (offset as usize) >= s.result.len();
                                (bytes, past_end)
                            };
                            if past_end {
                                slots.remove((ino, sid));
                            }
                            reply.data(&bytes);
                        });
                        return;
                    }
                    FileKind::Passthrough | FileKind::Readonly => {
                        // Fall through to disk read.
                    }
                }
            }

            // No manifest entry or passthrough/readonly: read from disk.

            // .log virtual file: return the most recent trace for the
            // corresponding endpoint inode.
            if let Some(path_str) = disk_path.to_str()
                && let Some(ep_str) = path_str.strip_suffix(".log") {
                    let ep_path = PathBuf::from(ep_str);
                    let trace = self.path_table.lock().unwrap()
                        .get(&ep_path).copied()
                        .and_then(|ep_ino| self.slots.latest_trace_for_ino(ep_ino))
                        .unwrap_or_default();
                    reply_bytes(reply, &trace, offset, size);
                    return;
                }

            // schema.json: generate from folder.yaml.
            if disk_path.file_name().is_some_and(|n| n == "schema.json")
                && let Some(tool_dir) = disk_path.parent()
                    && let Ok(Some(manifest)) = crate::manifest::Manifest::load(tool_dir) {
                        let content = crate::fs::schema_gen::generate_schema_json(&manifest);
                        let data = content.into_bytes();
                        reply_bytes(reply, &data, offset, size);
                        return;
                    }

            // anthropic_tools.json: generate from folder.yaml.
            if disk_path.file_name().is_some_and(|n| n == "anthropic_tools.json")
                && let Some(tool_dir) = disk_path.parent()
                    && let Ok(Some(manifest)) = crate::manifest::Manifest::load(tool_dir) {
                        let tool_name = tool_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown");
                        let content = crate::fs::schema_gen::generate_anthropic_tools_json(tool_name, &manifest);
                        let data = content.into_bytes();
                        reply_bytes(reply, &data, offset, size);
                        return;
                    }

            // how_to.md: generate from folder.yaml if absent on disk.
            if !disk_path.exists()
                && disk_path.file_name().is_some_and(|n| n == "how_to.md")
                && let Some(tool_dir) = disk_path.parent()
                    && let Ok(Some(manifest)) = crate::manifest::Manifest::load(tool_dir) {
                        let content = crate::fs::how_to_gen::generate_how_to(&manifest);
                        let data = content.into_bytes();
                        let start = offset as usize;
                        let end = (start + size as usize).min(data.len());
                        reply.data(if start < data.len() { &data[start..end] } else { b"" });
                        return;
                    }
            match std::fs::read(&disk_path) {
                Ok(bytes) => {
                    reply_bytes(reply, &bytes, offset, size);
                }
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            }
            return;
        }

        let data: Option<Vec<u8>> = match ino {
            ROOT_HOW_TO_INO => Some(ROOT_HOW_TO.as_bytes().to_vec()),
            ROOT_CREATE_TOOL_INO => Some(ROOT_CREATE_TOOL.as_bytes().to_vec()),
            ROOT_SYSTEM_PROMPT_INO => Some(self.system_prompt_content()),
            ROOT_INDEX_INO => Some(self.registry.read().unwrap().root_index().into_bytes()),
            _ => {
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    if ino == how_to_ino(idx) {
                        let registry = self.registry.read().unwrap();
                        let name = registry.list()[idx];
                        registry.get(name).map(|t| t.how_to().as_bytes().to_vec())
                    } else if self.ep_index_for_ino(ino).is_some() {
                        // Built-in endpoint: drain the per-sid slot.
                        let handle = self.slot_for_endpoint(ino, sid);
                        let bytes = if let Some(h) = handle {
                            let mut s = h.lock().unwrap();
                            s.touch();
                            let b = s.slice(offset, size).to_vec();
                            let past_end = (offset as usize) >= s.result.len();
                            drop(s);
                            if past_end {
                                self.slots.remove((ino, sid));
                            }
                            b
                        } else {
                            Vec::new()
                        };
                        // Built-in path uses reply.data directly so we can
                        // return the already-sliced bytes without going
                        // through reply_bytes again.
                        reply.data(&bytes);
                        return;
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };

        match data {
            Some(bytes) => {
                reply_bytes(reply, &bytes, offset, size);
            }
            None => reply.error(ENOENT),
        }
    }

    fn write(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let is_writable = self.ep_index_for_ino(ino).is_some()
            || match self.file_spec_for_ino(ino) {
                Some((_, _, spec)) => !matches!(spec.kind, FileKind::Readonly),
                None => self.path_for_ino(ino).is_some(),
            };
        if !is_writable {
            reply.error(libc::EACCES);
            return;
        }

        let sid = self.resolve_sid(req, fh);

        // Endpoint writes accumulate per (ino, sid). Two shells writing
        // to the same endpoint never interleave because they have
        // distinct sids.
        if let Some(handle) = self.slot_for_endpoint(ino, sid) {
            let mut s = handle.lock().unwrap();
            let end = offset as usize + data.len();
            if end > s.write_buf.len() {
                s.write_buf.resize(end, 0);
            }
            s.write_buf[offset as usize..end].copy_from_slice(data);
            s.touch();
            reply.written(data.len() as u32);
            return;
        }

        // Passthrough disk file (no manifest spec, just a plain file).
        // Use a sid-scoped slot to buffer the bytes until release().
        if let Some(disk_path) = self.path_for_ino(ino) {
            let cwd_path = disk_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            let handle = self.slot_for_synth(ino, sid, || {
                self.synth_snapshot("", "", cwd_path)
            });
            let mut s = handle.lock().unwrap();
            let end = offset as usize + data.len();
            if end > s.write_buf.len() {
                s.write_buf.resize(end, 0);
            }
            s.write_buf[offset as usize..end].copy_from_slice(data);
            s.touch();
            reply.written(data.len() as u32);
            return;
        }

        reply.error(libc::EACCES);
    }

    fn release(
        &mut self,
        req: &Request,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        let sid = self.resolve_sid(req, fh);
        // The fh→sid mapping is only needed for the duration of this op
        // (release is always the last op on a given fh).
        self.drop_fh(fh);

        // External file on disk: dispatch on manifest FileSpec or executable heuristic.
        if let Some(disk_path) = self.path_for_ino(ino) {
            // Manifest-declared file: dispatch on declared type.
            if let Some((_, _, spec)) = self.file_spec_for_ino(ino) {
                match spec.kind {
                    FileKind::WriteInvoke => {
                        let Some(handle) = self.slot_for_manifest(ino, sid) else {
                            reply.ok();
                            return;
                        };
                        // Async dispatch: mark the slot Pending now so a
                        // racing read() waits on the Notify, spawn the
                        // handler, and return immediately. The FUSE
                        // thread is freed to handle other ops while the
                        // handler runs.
                        let has_input = !handle.lock().unwrap().write_buf.is_empty();
                        if has_input {
                            Self::mark_pending(&handle);
                            let h = handle.clone();
                            self.rt.spawn(async move {
                                let result = Self::run_invocation_async(h.clone()).await;
                                Self::commit_result_into(&h, result);
                            });
                        }
                        reply.ok();
                        return;
                    }
                    FileKind::ReadInvoke => {
                        // write stores params in slot.write_buf; read() triggers invocation.
                        reply.ok();
                        return;
                    }
                    FileKind::Passthrough => {
                        if let Some(handle) = self.slot_for_manifest(ino, sid) {
                            let data = std::mem::take(&mut handle.lock().unwrap().write_buf);
                            if !data.is_empty() {
                                let _ = std::fs::write(&disk_path, data);
                            }
                            self.slots.remove((ino, sid));
                        }
                        reply.ok();
                        return;
                    }
                    FileKind::Readonly => {
                        reply.ok();
                        return;
                    }
                }
            }

            // No manifest entry: fall back to heuristic (executable bit).
            use std::os::unix::fs::PermissionsExt;
            let is_exec = std::fs::metadata(&disk_path)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !is_exec {
                // Plain file: flush the per-sid buffer to disk.
                if let Some(handle) = self.slots.get((ino, sid)) {
                    let data = std::mem::take(&mut handle.lock().unwrap().write_buf);
                    if !data.is_empty() {
                        let _ = std::fs::write(&disk_path, data);
                    }
                    self.slots.remove((ino, sid));
                }
                reply.ok();
                return;
            }

            // Executable with no manifest: invoke via the registry.
            if let Some(tools_dir) = self.tools_dir.clone()
                && let Ok(rel) = disk_path.strip_prefix(&tools_dir) {
                    let parts: Vec<_> = rel.components().collect();
                    if parts.len() >= 2 {
                        let tool_name = parts[0].as_os_str().to_string_lossy().to_string();
                        let ep_name = parts[1].as_os_str().to_string_lossy().to_string();
                        let tool = self.registry.read().unwrap().get(&tool_name);
                        if let Some(tool) = tool {
                            let cwd_path = disk_path
                                .parent()
                                .unwrap_or(Path::new("."))
                                .to_path_buf();
                            let handle = self.slot_for_synth(ino, sid, || {
                                self.synth_snapshot(&tool_name, &ep_name, cwd_path)
                            });
                            let input = std::mem::take(&mut handle.lock().unwrap().write_buf);
                            if !input.is_empty() {
                                Self::mark_pending(&handle);
                                let session = self.session.clone();
                                let h = handle.clone();
                                let tool_name_dbg = tool_name.clone();
                                let ep_name_dbg = ep_name.clone();
                                self.rt.spawn(async move {
                                    tracing::debug!("invoke start: tool={} endpoint={}", tool_name_dbg, ep_name_dbg);
                                    let result = tool.invoke(&ep_name, &input, &session).await;
                                    Self::commit_result_into(&h, result);
                                });
                            }
                        }
                    }
                }
            reply.ok();
            return;
        }

        // Built-in tool endpoint (inode in the 1000..100_000 range).
        let Some((tool_idx, ep_idx)) = self.ep_index_for_ino(ino) else {
            reply.ok();
            return;
        };

        let (tool_name, endpoint, tool) = {
            let registry = self.registry.read().unwrap();
            let tool_name = registry.list()[tool_idx].to_string();
            let tool = match registry.get(&tool_name) {
                Some(t) => t,
                None => { reply.ok(); return; }
            };
            let endpoint = tool.endpoints()[ep_idx].to_string();
            (tool_name, endpoint, tool)
        };

        let cwd_path = self
            .tools_dir
            .as_ref()
            .map(|d| d.join(&tool_name))
            .unwrap_or_else(|| PathBuf::from("."));
        let handle = self.slot_for_synth(ino, sid, || {
            self.synth_snapshot(&tool_name, &endpoint, cwd_path)
        });
        let input = std::mem::take(&mut handle.lock().unwrap().write_buf);
        if input.is_empty() {
            reply.ok();
            return;
        }

        Self::mark_pending(&handle);
        let session = self.session.clone();
        tracing::debug!("invoking tool={} endpoint={} input_len={}", tool_name, endpoint, input.len());
        let h = handle.clone();
        let tool_name_dbg = tool_name.clone();
        let endpoint_dbg = endpoint.clone();
        self.rt.spawn(async move {
            tracing::debug!("invoke start: tool={} endpoint={}", tool_name_dbg, endpoint_dbg);
            let result = tool.invoke(&endpoint, &input, &session).await;
            Self::commit_result_into(&h, result);
        });
        reply.ok();
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let parent_path = self.path_for_ino(parent)
            .or_else(|| self.tool_dir_disk_path(parent));
        if let Some(pp) = parent_path {
            let disk_path = pp.join(name.to_string_lossy().as_ref());
            if std::fs::File::create(&disk_path).is_ok() {
                let ino = self.ino_for_path(&disk_path);
                let attr = Self::file_attr(ino, 0, 0o644);
                reply.created(&TTL, &attr, 0, 0, 0);
                return;
            }
        }
        reply.error(libc::EACCES);
    }

    fn mkdir(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        if parent == TOOLS_DIR_INO {
            let Some(tools_dir) = &self.tools_dir else {
                reply.error(libc::EACCES);
                return;
            };
            let tool_name = name.to_string_lossy().to_string();
            let dir_path = tools_dir.join(&tool_name);
            if std::fs::create_dir(&dir_path).is_ok() {
                // Register the tool synchronously so the kernel receives the stable
                // static inode (tool_dir_ino) from the very first mkdir reply.
                // Without this, the kernel caches a dynamic inode (>= 100_000) and
                // subsequent lookups of virtual manifest-declared files inside the dir
                // fall into the disk-only code path and return ENOENT within the TTL.
                let idx = {
                    let mut reg = self.registry.write().unwrap_or_else(|e| e.into_inner());
                    if reg.get(&tool_name).is_none() {
                        reg.register(Arc::new(
                            ExternalTool::with_sandbox_mode(&tool_name, dir_path, self.timeout_secs, self.sandbox_mode),
                        ));
                    }
                    reg.list().iter().position(|&n| n == tool_name.as_str()).unwrap_or(0)
                };
                reply.entry(&TTL, &Self::dir_attr(tool_dir_ino(idx)), 0);
            } else {
                reply.error(libc::EIO);
            }
        } else if let Some(parent_path) = self.path_for_ino(parent) {
            let dir_path = parent_path.join(name.to_string_lossy().as_ref());
            if std::fs::create_dir(&dir_path).is_ok() {
                let ino = self.ino_for_path(&dir_path);
                reply.entry(&TTL, &Self::dir_attr(ino), 0);
            } else {
                reply.error(libc::EIO);
            }
        } else {
            reply.error(libc::EACCES);
        }
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        if parent == TOOLS_DIR_INO {
            let Some(tools_dir) = &self.tools_dir else {
                reply.error(libc::EACCES);
                return;
            };
            let tool_name = name.to_string_lossy().to_string();
            let dir_path = tools_dir.join(&tool_name);
            match std::fs::remove_dir_all(&dir_path) {
                Ok(_) => {
                    self.registry.write().unwrap_or_else(|e| e.into_inner()).unregister(&tool_name);
                    reply.ok();
                }
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            }
        } else if let Some(parent_path) = self.tool_dir_disk_path(parent).or_else(|| self.path_for_ino(parent)) {
            let dir_path = parent_path.join(name.to_string_lossy().as_ref());
            match std::fs::remove_dir_all(&dir_path) {
                Ok(_) => reply.ok(),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            }
        } else {
            reply.error(libc::EACCES);
        }
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        let parent_path = self.tool_dir_disk_path(parent)
            .or_else(|| self.path_for_ino(parent));
        if let Some(pp) = parent_path {
            let name_str = name.to_str().unwrap_or("");
            let path = pp.join(name_str);
            // Virtual files (write_invoke / read_invoke) have no disk representation.
            // Treat unlink as clearing buffered state and succeed.
            if !path.exists() {
                if let Some(tools_dir) = self.tools_dir.clone() {
                    if let Ok(rel) = pp.strip_prefix(&tools_dir) {
                        let mut comps = rel.components();
                        if let (Some(tc), None) = (comps.next(), comps.next()) {
                            let tool_name = tc.as_os_str().to_string_lossy().to_string();
                            if let Some(manifest) = self.manifest_for_tool(&tool_name) {
                                if let Some(spec) = manifest.spec_for(name_str) {
                                    if matches!(spec.kind, FileKind::WriteInvoke | FileKind::ReadInvoke) {
                                        if let Some(&ino) = self.path_table.lock().unwrap().get(&path) {
                                            // Drop slots for this inode across all sids.
                                            self.slots.remove_all_for_ino(ino);
                                        }
                                        reply.ok();
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            match std::fs::remove_file(&path) {
                Ok(_) => reply.ok(),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            }
        } else {
            reply.error(libc::EACCES);
        }
    }

    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        let src = self.tool_dir_disk_path(parent)
            .or_else(|| self.path_for_ino(parent))
            .map(|p| p.join(name.to_string_lossy().as_ref()));
        let dst = self.tool_dir_disk_path(newparent)
            .or_else(|| self.path_for_ino(newparent))
            .map(|p| p.join(newname.to_string_lossy().as_ref()));
        match (src, dst) {
            (Some(s), Some(d)) => match std::fs::rename(&s, &d) {
                Ok(_) => reply.ok(),
                Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
            },
            _ => reply.error(libc::EACCES),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino, FileType::Directory, ".".to_string()),
            (ROOT_INO, FileType::Directory, "..".to_string()),
        ];

        match ino {
            ROOT_INO => {
                entries.push((ROOT_HOW_TO_INO, FileType::RegularFile, "how_to.md".to_string()));
                entries.push((ROOT_CREATE_TOOL_INO, FileType::RegularFile, "create_tool.md".to_string()));
                entries.push((ROOT_SYSTEM_PROMPT_INO, FileType::RegularFile, "system_prompt.md".to_string()));
                entries.push((ROOT_INDEX_INO, FileType::RegularFile, "index.md".to_string()));
                entries.push((TOOLS_DIR_INO, FileType::Directory, "tools".to_string()));
            }
            TOOLS_DIR_INO => {
                for (i, name) in self.registry.read().unwrap().list().iter().enumerate() {
                    entries.push((tool_dir_ino(i), FileType::Directory, name.to_string()));
                }
            }
            _ => {
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    let base = tool_dir_ino(idx);
                    if ino == base {
                        let reg = self.registry.read().unwrap();
                        let tool_name = reg.list()[idx].to_string();
                        drop(reg);

                        // Try reading from disk (external tool) — covers all files
                        if let Some(tools_dir) = &self.tools_dir {
                            let tool_path = tools_dir.join(&tool_name);
                            if tool_path.is_dir() {
                                // Use disk as source of truth for external tools
                                if let Ok(dir_entries) = std::fs::read_dir(&tool_path) {
                                    for entry in dir_entries.flatten() {
                                        let fname = entry.file_name().to_string_lossy().to_string();
                                        let fpath = entry.path();
                                        let child_ino = self.ino_for_path(&fpath);
                                        let kind = if fpath.is_dir() { FileType::Directory } else { FileType::RegularFile };
                                        entries.push((child_ino, kind, fname));
                                    }
                                }
                                if let Ok(Some(manifest)) = crate::manifest::Manifest::load(&tool_path) {
                                    // Synthesize how_to.md if absent on disk.
                                    if !entries.iter().any(|(_, _, n)| n == "how_to.md") {
                                        let p = tool_path.join("how_to.md");
                                        entries.push((self.ino_for_path(&p), FileType::RegularFile, "how_to.md".to_string()));
                                    }
                                    // Always include schema.json.
                                    if !entries.iter().any(|(_, _, n)| n == "schema.json") {
                                        let p = tool_path.join("schema.json");
                                        entries.push((self.ino_for_path(&p), FileType::RegularFile, "schema.json".to_string()));
                                    }
                                    // Always include anthropic_tools.json.
                                    if !entries.iter().any(|(_, _, n)| n == "anthropic_tools.json") {
                                        let p = tool_path.join("anthropic_tools.json");
                                        entries.push((self.ino_for_path(&p), FileType::RegularFile, "anthropic_tools.json".to_string()));
                                    }
                                    // Merge manifest-declared virtual files and their .log companions.
                                    for spec in &manifest.files {
                                        use crate::manifest::FileKind;
                                        if !entries.iter().any(|(_, _, n)| n == &spec.name) {
                                            let vp = tool_path.join(&spec.name);
                                            entries.push((self.ino_for_path(&vp), FileType::RegularFile, spec.name.clone()));
                                        }
                                        if matches!(spec.kind, FileKind::WriteInvoke | FileKind::ReadInvoke) {
                                            let log_name = format!("{}.log", spec.name);
                                            if !entries.iter().any(|(_, _, n)| *n == log_name) {
                                                let lp = tool_path.join(&log_name);
                                                entries.push((self.ino_for_path(&lp), FileType::RegularFile, log_name));
                                            }
                                        }
                                    }
                                }
                                // Skip built-in endpoint enumeration for external tools
                            } else {
                                // Built-in tool: use existing how_to.md + endpoints logic
                                let reg = self.registry.read().unwrap();
                                let tool = match reg.get(&tool_name) {
                                    Some(t) => t,
                                    None => { reply.ok(); return; }
                                };
                                entries.push((how_to_ino(idx), FileType::RegularFile, "how_to.md".to_string()));
                                for (ei, ep) in tool.endpoints().iter().enumerate() {
                                    entries.push((endpoint_ino(idx, ei), FileType::RegularFile, ep.to_string()));
                                }
                            }
                        } else {
                            // No tools_dir: all tools are built-in
                            let reg = self.registry.read().unwrap();
                            let tool = match reg.get(&tool_name) {
                                Some(t) => t,
                                None => { reply.ok(); return; }
                            };
                            entries.push((how_to_ino(idx), FileType::RegularFile, "how_to.md".to_string()));
                            for (ei, ep) in tool.endpoints().iter().enumerate() {
                                entries.push((endpoint_ino(idx, ei), FileType::RegularFile, ep.to_string()));
                            }
                        }
                    } else {
                        reply.error(ENOTDIR);
                        return;
                    }
                } else {
                    // External path (subdirectory of a tool dir)
                    if let Some(dir_path) = self.path_for_ino(ino) {
                        if let Ok(dir_entries) = std::fs::read_dir(&dir_path) {
                            for entry in dir_entries.flatten() {
                                let fname = entry.file_name().to_string_lossy().to_string();
                                let fpath = entry.path();
                                let child_ino = self.ino_for_path(&fpath);
                                let kind = if fpath.is_dir() { FileType::Directory } else { FileType::RegularFile };
                                entries.push((child_ino, kind, fname));
                            }
                        }
                    } else {
                        reply.error(ENOENT);
                        return;
                    }
                }
            }
        }

        for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

#[cfg(test)]
mod tests {
    use super::LiveFolders;

    #[test]
    fn format_trace_success_includes_duration_and_ok() {
        let out = LiveFolders::format_trace(123, false, b"");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("duration_ms: 123"), "got: {s}");
        assert!(s.contains("exit: ok"), "got: {s}");
    }

    #[test]
    fn format_trace_error_sets_exit_to_error() {
        let out = LiveFolders::format_trace(0, true, b"");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("exit: error"), "got: {s}");
    }

    #[test]
    fn format_trace_includes_trimmed_stderr() {
        let out = LiveFolders::format_trace(50, false, b"  warning: something\n");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("warning: something"), "got: {s}");
        assert!(!s.ends_with("  "), "leading spaces should be trimmed");
    }

    #[test]
    fn format_trace_empty_stderr_produces_empty_line() {
        let out = LiveFolders::format_trace(10, false, b"");
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("stderr: \n"), "got: {s}");
    }
}
