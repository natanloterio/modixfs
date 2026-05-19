## LiveFoldersFS

- Handlers run as shell commands with the mount process's privileges
- Secrets stored in ~/.config/livefolders/secrets.env (file permissions only)
- No sandboxing: a malicious handler can read files, exec processes, phone home
- Timeout kills hung handlers (default 30s) — prevents DoS but not exfil
- Attack surface: folder.yaml handler field (arbitrary shell execution)

## MCP

- Server is a separate process; client controls what tools it exposes
- Claude Code runs MCP servers as child processes (same user privilege)
- No built-in sandboxing either — a malicious server.py has full process access
- Secrets passed via environment variables or config (same risk surface)
- Schema validation provides input sanitization layer (MCP advantage)

## Verdict

Both run with the user's privileges. Neither provides OS-level sandboxing.
MCP has a slight edge: schema validation prevents injection via structured inputs.
LiveFoldersFS's shell handler is a larger injection surface (shell metacharacters).
