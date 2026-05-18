use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyWrite,
    Request,
};
use libc::{ENOENT, ENOTDIR};
use tokio::runtime::Handle;
use tracing::debug;

use crate::registry::{Session, ToolRegistry};

use super::inode::*;

const TTL: Duration = Duration::from_secs(1);

/// Pending write buffers keyed by inode.
/// Written bytes accumulate here until flush/release triggers invocation.
type WriteBuf = Arc<Mutex<HashMap<u64, Vec<u8>>>>;

/// Last result keyed by inode — returned on the next read after invocation.
type ResultBuf = Arc<Mutex<HashMap<u64, Vec<u8>>>>;

/// Inode → disk path mapping for external tool files (inodes >= 100_000).
type InodeTable = Arc<Mutex<HashMap<u64, PathBuf>>>;

/// Disk path → inode mapping for external tool files.
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

impl ModixFS {
    pub fn new(
        registry: Arc<RwLock<ToolRegistry>>,
        tools_dir: Option<PathBuf>,
        session: Session,
        rt: Handle,
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
        }
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
        if ino < 1000 || ino >= 100_000 {
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
            blocks: (size + 511) / 512,
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
            _ => {
                // External inode (>= 100_000)?
                if let Some(disk_path) = self.path_for_ino(ino) {
                    if let Ok(meta) = std::fs::metadata(&disk_path) {
                        use std::os::unix::fs::PermissionsExt;
                        let perm = (meta.permissions().mode() as u16) & 0o777;
                        if meta.is_dir() {
                            return Some(Self::dir_attr(ino));
                        } else {
                            return Some(Self::file_attr(ino, meta.len(), perm));
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
                        let result = self.result_buf.lock().unwrap().get(&ino).cloned();
                        let size = result.map(|r| r.len()).unwrap_or(0) as u64;
                        return Some(Self::file_attr(ino, size, 0o644));
                    }
                }
                None
            }
        }
    }

    fn lookup_in_root(&self, name: &OsStr) -> Option<FileAttr> {
        let s = name.to_str()?;
        match s {
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
            let size = tool.how_to().len() as u64;
            return Some(Self::file_attr(how_to_ino(idx), size, 0o444));
        }

        let ep_pos = tool.endpoints().iter().position(|&e| e == s)?;
        let ino = endpoint_ino(idx, ep_pos);
        let result = self.result_buf.lock().unwrap().get(&ino).cloned();
        let size = result.map(|r| r.len()).unwrap_or(0) as u64;
        Some(Self::file_attr(ino, size, 0o644))
    }

    fn lookup_external_file(&self, tool_name: &str, name: &str) -> Option<FileAttr> {
        let tools_dir = self.tools_dir.as_ref()?;
        let disk_path = tools_dir.join(tool_name).join(name);
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
}

impl Filesystem for ModixFS {
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
                        let disk_path = parent_path.join(name.to_str().unwrap_or(""));
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
        _req: &Request,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        // Write permissions to disk for external files
        if let Some(mode) = mode {
            if let Some(disk_path) = self.path_for_ino(ino) {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(mode);
                let _ = std::fs::set_permissions(&disk_path, perms);
            }
        }

        // Handle truncation on O_TRUNC open (e.g. shell `>` redirect)
        if let Some(new_size) = size {
            if self.ep_index_for_ino(ino).is_some() {
                self.write_buf.lock().unwrap().entry(ino).or_default().truncate(new_size as usize);
                self.result_buf.lock().unwrap().remove(&ino);
            }
        }
        match self.resolve_ino_attr(ino) {
            Some(a) => reply.attr(&TTL, &a),
            None => reply.error(ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        // Passthrough: external file on disk (non-executable = data file, not endpoint)
        if let Some(disk_path) = self.path_for_ino(ino) {
            use std::os::unix::fs::PermissionsExt;
            let is_exec = std::fs::metadata(&disk_path)
                .map(|m| m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false);
            if !is_exec {
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
        }

        let data: Option<Vec<u8>> = match ino {
            ROOT_INDEX_INO => Some(self.registry.read().unwrap().root_index().into_bytes()),
            _ => {
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    if ino == how_to_ino(idx) {
                        let registry = self.registry.read().unwrap();
                        let name = registry.list()[idx];
                        registry.get(name).map(|t| t.how_to().as_bytes().to_vec())
                    } else if self.ep_index_for_ino(ino).is_some() {
                        // reading an endpoint returns last result, then clears it
                        let result = self.result_buf.lock().unwrap().remove(&ino);
                        Some(result.unwrap_or_default())
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
                let start = offset as usize;
                if start >= bytes.len() {
                    reply.data(&[]);
                } else {
                    let end = (start + size as usize).min(bytes.len());
                    reply.data(&bytes[start..end]);
                }
            }
            None => reply.error(ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if self.ep_index_for_ino(ino).is_none() && self.path_for_ino(ino).is_none() {
            reply.error(libc::EACCES);
            return;
        }

        let mut buf = self.write_buf.lock().unwrap();
        let entry = buf.entry(ino).or_default();
        let end = offset as usize + data.len();
        if end > entry.len() {
            entry.resize(end, 0);
        }
        entry[offset as usize..end].copy_from_slice(data);
        reply.written(data.len() as u32);
    }

    fn release(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        // Passthrough: flush write_buf to disk for external non-executable files
        if let Some(disk_path) = self.path_for_ino(ino) {
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
        }

        let input = match self.write_buf.lock().unwrap().remove(&ino) {
            Some(b) if !b.is_empty() => b,
            _ => {
                reply.ok();
                return;
            }
        };

        let (tool_idx, ep_idx) = match self.ep_index_for_ino(ino) {
            Some(pair) => pair,
            None => {
                reply.ok();
                return;
            }
        };

        let (tool_name, endpoint, tool) = {
            let registry = self.registry.read().unwrap();
            let tool_name = registry.list()[tool_idx].to_string();
            let tool = registry.get(&tool_name).unwrap();
            let endpoint = tool.endpoints()[ep_idx].to_string();
            (tool_name, endpoint, tool)
        };
        let session = self.session.clone();
        let result_buf = self.result_buf.clone();

        tracing::debug!("invoking tool={} endpoint={} input_len={}", tool_name, endpoint, input.len());

        self.rt.spawn(async move {
            tracing::debug!("async invoke start: tool={} endpoint={}", tool_name, endpoint);
            let result = tool.invoke(&endpoint, &input, &session).await;
            let output = if result.is_error() {
                format!("ERROR: {}\n", result.error.unwrap()).into_bytes()
            } else {
                result.output
            };
            tracing::debug!("async invoke done: ino={} output_len={}", ino, output.len());
            result_buf.lock().unwrap().insert(ino, output);
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
            let dir_path = tools_dir.join(name.to_string_lossy().as_ref());
            if std::fs::create_dir(&dir_path).is_ok() {
                let ino = self.ino_for_path(&dir_path);
                reply.entry(&TTL, &Self::dir_attr(ino), 0);
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

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        let parent_path = self.tool_dir_disk_path(parent)
            .or_else(|| self.path_for_ino(parent));
        if let Some(pp) = parent_path {
            let path = pp.join(name.to_string_lossy().as_ref());
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
                                // Skip built-in endpoint enumeration for external tools
                            } else {
                                // Built-in tool: use existing how_to.md + endpoints logic
                                let reg = self.registry.read().unwrap();
                                let tool = reg.get(&tool_name).unwrap();
                                entries.push((how_to_ino(idx), FileType::RegularFile, "how_to.md".to_string()));
                                for (ei, ep) in tool.endpoints().iter().enumerate() {
                                    entries.push((endpoint_ino(idx, ei), FileType::RegularFile, ep.to_string()));
                                }
                            }
                        } else {
                            // No tools_dir: all tools are built-in
                            let reg = self.registry.read().unwrap();
                            let tool = reg.get(&tool_name).unwrap();
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
