# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Build
cargo build
cargo build --release

# Run tests
cargo test

# Run a single test (by name substring)
cargo test <test_name>

# Run tests for a specific module
cargo test --lib config::tests

# Run with debug logging
RUST_LOG=livefolders=debug cargo run -- mount --foreground

# Cross-compile for aarch64 Linux (requires `cross`)
cross build --target aarch64-unknown-linux-gnu --release
```

## Architecture

LiveFolders is a FUSE filesystem that exposes tools as plain files. LLMs interact with tools using `cat` and `echo` with no protocol or SDK.

### Data flow

1. LLM writes to an endpoint file (e.g. `echo "London" > .livefolders/tools/weather/forecast`).
2. FUSE `open()` captures the caller's session id (via `getsid(req.pid())`) and binds it to a fresh file handle.
3. FUSE `write()` looks up the per-`(inode, sid)` invocation slot and appends bytes to its private `write_buf`.
4. FUSE `release()` fires when the fd closes. For `write_invoke` it marks the slot `Pending` and spawns the handler onto the Tokio runtime, returning immediately so the FUSE thread is free for other ops.
5. The handler runs as a shell command with input on stdin. On completion the spawned task stores the output in `slot.result`, transitions the slot to `Ready`, and wakes any awaiters via `Notify::notify_waiters()`.
6. LLM reads the endpoint (`cat .livefolders/tools/weather/forecast`) — FUSE `read()` looks up the slot, awaits if `Pending`, slices `slot.result[offset..]` when `Ready`, and drops the slot once fully drained.
7. The slot's `trace` (`duration_ms`, exit status, stderr) is exposed via the `<endpoint>.log` companion file.

For `read_invoke` endpoints the handler is kicked by the first `read()` instead of by `release()`; the state machine is otherwise the same.

### Concurrency model

- Invocation state is keyed by `(inode, session id)` so `echo` and `cat` from the same shell pipeline share a slot, while pipelines from different shells (different sids) run in parallel and never clobber each other.
- FUSE issues some operations (notably `release` and async `read`) with `pid == 0`; for those we look up the sid captured at `open()` time via the file handle (see `fs::sid`).
- Handlers run on the Tokio runtime, not on the FUSE dispatcher thread. Two slow handlers complete in `~max(t1, t2)`, not `t1 + t2`.
- `state_file` flock (declared in `folder.yaml`) is the only cross-tool isolation primitive. Handlers that share external state (a database, an API rate limit, a file) must either be idempotent or declare a `state_file`.
- Idle slots are reaped after 15 minutes by default (`vfs::spawn_reaper`) so abandoned opens cannot leak memory.

### Module map

| Module | Role |
|--------|------|
| `src/fs/vfs.rs` | Core FUSE implementation (`LiveFolders` struct). Handles all VFS calls and dispatches to tools via spawned Tokio tasks. |
| `src/fs/inode.rs` | Inode number scheme: built-in tools use a static range (1000–100000), external tools use dynamically allocated inodes ≥ 100000. |
| `src/fs/invocation.rs` | `InvocationSlot` (per-`(ino, sid)` state: write_buf, result, trace, state machine) and `EndpointSnapshot` (manifest data pinned at slot creation time). |
| `src/fs/slot_table.rs` | `SlotTable` indexed by `(ino, sid)` with `get_or_create`, `remove`, `remove_all_for_ino`, `reap_idle`. |
| `src/fs/sid.rs` | `caller_sid(req)` wraps `getsid(req.pid())` and returns `None` when FUSE gives us `pid == 0`. |
| `src/registry/` | `ToolRegistry` (name → `Arc<dyn Tool>`) + `Tool` trait + `Session` (per-mount context). |
| `src/tools/` | `EchoTool` (built-in smoke-test) and `ExternalTool` (loads `folder.yaml`, dispatches to shell handlers). |
| `src/manifest.rs` | Parses `folder.yaml`. Defines `FileKind` (`write_invoke`, `read_invoke`, `passthrough`, `readonly`), `InputSchema`, and `FileSpec`. |
| `src/fs/how_to_gen.rs` | Generates `how_to.md` from a `Manifest` at read time (never written to disk). |
| `src/fs/schema_gen.rs` | Generates `schema.json` (MCP-compatible) from a `Manifest` at read time. |
| `src/watcher.rs` | Watches `tools_dir` via `notify`. Hot-reloads tools by updating the `ToolRegistry` without remounting. |
| `src/installer.rs` | Downloads a `folder.yaml` + companion files from a GitHub URL, prompts for secrets. |
| `src/secrets.rs` | Loads `~/.config/livefolders/secrets.env` into the process environment at startup. |
| `src/daemon.rs` | Forks and daemonizes the mount process; `stop` sends SIGTERM to the stored PID. |
| `src/doctor.rs` | Validates FUSE availability, `livefolders.yaml`, and every installed tool's `folder.yaml`. |
| `src/config.rs` | Parses `livefolders.yaml` (`mount`, `tools_dir`, `timeout`, `tools`). |

### Tool definition (`folder.yaml`)

Each tool is a directory under `tools_dir` containing a `folder.yaml`. The manifest declares:

- `files[]` — endpoint specs with `type`, `handler` (shell command), optional `input` schema, optional `state_file`, optional `pipe` (ordered list of endpoints to chain)
- `env[]` — secret declarations; users are prompted at `livefolders install` time

`how_to.md` and `schema.json` are **always synthesized at read time** from the manifest — never stored on disk.

### Inode layout

```
1        root mount dir
2        /tools dir
3        /index.md
4        /how_to.md
1000+    built-in tool dirs and endpoints (100 slots per tool)
≥100000  external tool files (path↔inode tables in LiveFolders)
```

### Concurrency model

The FUSE dispatcher thread is synchronous, but handler invocations are spawned onto a Tokio runtime (created after any fork) via `rt.spawn(...)`. The dispatcher returns immediately after spawning, so two concurrent handlers run in parallel and slow handlers do not block unrelated FUSE operations.

Coordination uses a `(ino, sid)`-keyed `SlotTable` (see "Data flow" and "Concurrency model" above). Each slot owns its `write_buf` and `result`, plus a state machine (`Idle` / `Pending(Notify)` / `Ready`) that lets a concurrent `read()` await an in-flight invocation via `Notify::notified().await`.

Timeouts use SIGTERM with a 1s grace, then escalate to SIGKILL via `libc::kill(pid, SIGKILL)`. When `state_file` is declared, an exclusive `flock` is acquired before the handler runs, serialising concurrent invocations of the same endpoint.

### Error format

All handler errors are returned as `[ERROR:CODE] message`. Codes: `INVALID_INPUT`, `HANDLER`, `TIMEOUT`, `SPAWN`, `PROCESS`.

## LiveFolders tools (for LLM use in this repo)

If `livefolders` is mounted, tools are at `.livefolders/tools/`:

1. `cat .livefolders/tools/index.md` — list available tools
2. `cat .livefolders/tools/<name>/how_to.md` — read usage instructions
3. `echo "..." > .livefolders/tools/<name>/<endpoint>` — write input
4. `cat .livefolders/tools/<name>/<endpoint>` — read output

## LiveFolders tools

Tools are available at `.livefolders/tools/`. To use them:
1. `cat .livefolders/tools/index.md` — discover available tools
2. `cat .livefolders/tools/<name>/how_to.md` — read usage instructions for a tool
3. Write input: `echo "..." > .livefolders/tools/<name>/<endpoint>`
4. Read output: `cat .livefolders/tools/<name>/<endpoint>`
