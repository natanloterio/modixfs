/// The virtual path structure maps to inodes as follows:
///
///   /                       → ROOT (1)
///   /index.md               → root index file
///   /tools/                 → TOOLS_DIR (2)  [alias: mount root itself]
///   /tools/<tool>/          → tool dir, inode per tool
///   /tools/<tool>/how_to.md → static doc file
///   /tools/<tool>/<ep>      → invocable endpoint file
///
/// Inode allocation is deterministic from path so we never need a table.
pub const ROOT_INO: u64 = 1;
pub const TOOLS_DIR_INO: u64 = 2;
pub const ROOT_INDEX_INO: u64 = 3;
pub const ROOT_HOW_TO_INO: u64 = 4;
pub const ROOT_CREATE_TOOL_INO: u64 = 5;

/// Inodes 1000+ are tool dirs (1000 + tool_index * 100).
/// Inodes 1001+ are how_to files (tool_ino + 1).
/// Inodes 1010+ are endpoint files (tool_ino + 10 + ep_index).
pub fn tool_dir_ino(tool_index: usize) -> u64 {
    1000 + (tool_index as u64) * 100
}

pub fn how_to_ino(tool_index: usize) -> u64 {
    tool_dir_ino(tool_index) + 1
}

pub fn endpoint_ino(tool_index: usize, ep_index: usize) -> u64 {
    tool_dir_ino(tool_index) + 10 + ep_index as u64
}
