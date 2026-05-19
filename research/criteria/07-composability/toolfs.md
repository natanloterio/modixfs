# ToolFS — Composability

**Evidence source:** `skills.go` (`ChainOperations`), `skills/SKILL.md` (chain endpoint), `skill_api.go`
**Rating:** ~ partial

ToolFS provides a `ChainOperations` API that accepts a list of typed operations (`search_memory`, `search_rag`, `read_file`, `execute_code_skill`, etc.) and executes them sequentially within a single request, passing the shared `Session` across steps. The `skills.go` `SearchMemoryAndOpenFile` function further demonstrates a hard-coded fallback chain (memory → RAG → filesystem). However, the chaining is sequential with no conditional branching, no data-flow wiring between steps (output of step N is not automatically the input of step N+1), and no graph-based composition — the developer must encode the chain logic in Go code or JSON at call time. Cross-skill composition where one skill invokes another is not documented.
