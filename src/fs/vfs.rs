use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
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

pub struct ModixFS {
    registry: Arc<ToolRegistry>,
    session: Session,
    write_buf: WriteBuf,
    result_buf: ResultBuf,
    rt: Handle,
}

impl ModixFS {
    pub fn new(registry: Arc<ToolRegistry>, session: Session, rt: Handle) -> Self {
        Self {
            registry,
            session,
            write_buf: Arc::new(Mutex::new(HashMap::new())),
            result_buf: Arc::new(Mutex::new(HashMap::new())),
            rt,
        }
    }

    fn tool_index_by_name(&self, name: &str) -> Option<usize> {
        self.registry.list().iter().position(|&n| n == name)
    }

    fn tool_index_for_ino(&self, ino: u64) -> Option<usize> {
        if ino < 1000 {
            return None;
        }
        let idx = ((ino - 1000) / 100) as usize;
        if idx < self.registry.list().len() {
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
        let tool_name = self.registry.list()[tool_idx];
        let tool = self.registry.get(tool_name)?;
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
                let content = self.registry.root_index();
                Some(Self::file_attr(ROOT_INDEX_INO, content.len() as u64, 0o444))
            }
            _ => {
                // tool dir?
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    let base = tool_dir_ino(idx);
                    if ino == base {
                        return Some(Self::dir_attr(ino));
                    }
                    // how_to?
                    if ino == how_to_ino(idx) {
                        let name = self.registry.list()[idx];
                        let tool = self.registry.get(name)?;
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
                let content = self.registry.root_index();
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
        let tool_name = self.registry.list()[idx];
        let tool = self.registry.get(tool_name)?;

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
}

impl Filesystem for ModixFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup parent={} name={:?}", parent, name);
        let attr = match parent {
            ROOT_INO => self.lookup_in_root(name),
            TOOLS_DIR_INO => self.lookup_in_tools(name),
            _ => {
                // parent is a tool dir?
                if self.tool_index_for_ino(parent).is_some() {
                    let base = tool_dir_ino(self.tool_index_for_ino(parent).unwrap());
                    if parent == base {
                        self.lookup_in_tool_dir(parent, name)
                    } else {
                        None
                    }
                } else {
                    None
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
        _mode: Option<u32>,
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
        let data: Option<Vec<u8>> = match ino {
            ROOT_INDEX_INO => Some(self.registry.root_index().into_bytes()),
            _ => {
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    if ino == how_to_ino(idx) {
                        let name = self.registry.list()[idx];
                        self.registry.get(name).map(|t| t.how_to().as_bytes().to_vec())
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
        if self.ep_index_for_ino(ino).is_none() {
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

        let tool_name = self.registry.list()[tool_idx].to_string();
        let endpoint = {
            let tool = self.registry.get(&tool_name).unwrap();
            tool.endpoints()[ep_idx].to_string()
        };
        let tool = self.registry.get(&tool_name).unwrap();
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
                for (i, name) in self.registry.list().iter().enumerate() {
                    entries.push((tool_dir_ino(i), FileType::Directory, name.to_string()));
                }
            }
            _ => {
                if let Some(idx) = self.tool_index_for_ino(ino) {
                    let base = tool_dir_ino(idx);
                    if ino == base {
                        let tool_name = self.registry.list()[idx];
                        let tool = self.registry.get(tool_name).unwrap();
                        entries.push((how_to_ino(idx), FileType::RegularFile, "how_to.md".to_string()));
                        for (ei, ep) in tool.endpoints().iter().enumerate() {
                            entries.push((endpoint_ino(idx, ei), FileType::RegularFile, ep.to_string()));
                        }
                    } else {
                        reply.error(ENOTDIR);
                        return;
                    }
                } else {
                    reply.error(ENOENT);
                    return;
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
