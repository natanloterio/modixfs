use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::manifest::{FileKind, InputSchema};

/// Identity of an invocation slot.
///
/// Slots are keyed by `(inode, sid)` rather than inode alone so that
/// concurrent shell sessions calling the same endpoint do not clobber
/// each other's input or result. `sid` comes from `getsid(req.pid())` —
/// `echo` and `cat` in the same shell pipeline share a sid.
pub type SlotKey = (u64, i32);

/// Snapshot of the manifest data needed to invoke an endpoint, taken at
/// slot creation time. Hot reload of `folder.yaml` swaps the registry
/// but never touches a live slot, so an in-flight invocation always
/// runs against the snapshot it started with.
#[derive(Clone)]
pub struct EndpointSnapshot {
    pub tool_name: String,
    pub file_name: String,
    pub cwd: PathBuf,
    pub kind: FileKind,
    pub handler: Option<String>,
    pub input_schema: Option<InputSchema>,
    pub state_file: Option<PathBuf>,
    pub pipe: Option<Vec<String>>,
    pub timeout_secs: u64,
    pub manifest_version: u64,
}

/// Per-invocation state for one `(ino, sid)` pair.
pub struct InvocationSlot {
    pub key: SlotKey,
    pub snapshot: EndpointSnapshot,
    pub write_buf: Vec<u8>,
    pub result: Vec<u8>,
    pub trace: Vec<u8>,
    /// Set to true once `release` has run the handler (for WriteInvoke)
    /// or once `read` has run it (for ReadInvoke). The result is then in
    /// `result` and a subsequent `read` will drain it.
    pub ready: bool,
    pub last_touched: Instant,
}

impl InvocationSlot {
    pub fn new(key: SlotKey, snapshot: EndpointSnapshot) -> Self {
        Self {
            key,
            snapshot,
            write_buf: Vec::new(),
            result: Vec::new(),
            trace: Vec::new(),
            ready: false,
            last_touched: Instant::now(),
        }
    }

    pub fn touch(&mut self) {
        self.last_touched = Instant::now();
    }

    /// Slice `self.result[offset..offset+size]`, clamped to bounds.
    /// Returns an empty slice when `offset >= result.len()`.
    pub fn slice(&self, offset: i64, size: u32) -> &[u8] {
        let start = offset as usize;
        if start >= self.result.len() {
            return &[];
        }
        let end = (start + size as usize).min(self.result.len());
        &self.result[start..end]
    }
}

pub type SlotHandle = Arc<std::sync::Mutex<InvocationSlot>>;
