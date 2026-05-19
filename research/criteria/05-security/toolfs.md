# ToolFS — Security

**Evidence source:** `sandbox.go`, `toolfs.go` (`Session`, `AuditLogEntry`), `skill_api.go`
**Rating:** ~ partial

ToolFS implements path-level access control via `Session.AllowedPaths`: every filesystem operation is checked against the session's allowed path prefixes before execution, and access-denied events are emitted to an `AuditLogger` as structured JSON. A `SandboxConfig` type specifies CPU timeout (default 30 s) and memory limit (default 64 MB) for skill execution, and `AllowHostFS: false` blocks direct host-filesystem access from within skills. However, the provided `InMemorySandbox` is explicitly labeled a "mock/in-memory sandbox implementation for testing" — the production WASM runtime (wazero/wasmer) is not yet wired in, so actual process-level isolation for code skills is not implemented. Secret handling and network egress control are not documented.
