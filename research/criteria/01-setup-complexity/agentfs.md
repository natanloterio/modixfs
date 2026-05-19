# AgentFS — Setup Complexity

**Evidence source:** https://github.com/tursodatabase/agentfs README, MANUAL.md
**Rating:** ~ partial

Installation requires a single curl-pipe-bash installer (`curl -fsSL https://agentfs.ai/install | bash`) and `agentfs init <name>` to create a SQLite-backed agent filesystem — the happy path is under two minutes for a CLI user. SDK integration (npm/pip/cargo) adds a dependency and ~10 lines of initialization code, which is reasonable but requires choosing and understanding three distinct interfaces (fs, kv, toolcall). FUSE mounting on Linux and NFS on macOS introduce platform-specific prerequisites (kernel FUSE module, macOS NFS daemon) that can silently fail. The setup bar is low for basic SDK use but rises noticeably when FUSE mounting or sandboxed `agentfs run` execution is needed.
