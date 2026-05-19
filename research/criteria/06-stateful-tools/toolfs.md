# ToolFS — Stateful Tools

**Evidence source:** `toolfs.go` (`MemoryStore`, `RAGStore`, `Session`), `skills/SKILL.md` (snapshot section), `sandbox.go`
**Rating:** ✓ strong

Stateful operation is a primary design goal of ToolFS: persistent key-value memory (`MemoryStore`) and a vector RAG store (`RAGStore`) are first-class subsystems accessible at `/toolfs/memory` and `/toolfs/rag`, and state written by one call is available to subsequent calls within the same session. The snapshot subsystem (`POST /toolfs/snapshots/create`, rollback) enables point-in-time state capture and restore across the full virtual filesystem. Skill executors receive a `SkillContext` with read/write access to the same stores, so code-based skills can maintain and mutate persistent state without external databases.
