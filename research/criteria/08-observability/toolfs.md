# ToolFS — Observability

**Evidence source:** `toolfs.go` (`AuditLogEntry`, `StdoutAuditLogger`), `sandbox.go` (`SkillExecutionResult.Violations`, `CPUTime`, `MemoryUsed`)
**Rating:** ~ partial

Every filesystem operation (ReadFile, WriteFile, ListDir, Stat) emits a structured JSON `AuditLogEntry` to stdout via `StdoutAuditLogger`, capturing timestamp, session ID, path, success/failure, bytes transferred, and access-denied events; sessions can plug in a custom `AuditLogger` implementation. Sandboxed skill executions additionally report CPU time, memory used, and a list of security violations in `SkillExecutionResult`. There is no distributed tracing (no trace IDs, no span propagation), no metrics endpoint, and no log-level filtering — all audit output goes to a single stdout stream, requiring an external log aggregator to make it useful in production.
