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

1. LLM writes to an endpoint file (e.g. `echo "London" > .livefolders/tools/weather/forecast`)
2. FUSE `write()` accumulates bytes in `write_buf` (keyed by inode)
3. FUSE `release()` fires when the file descriptor closes — this triggers handler invocation
4. The handler runs as a shell command with input on stdin; output is stored in `result_buf`
5. LLM reads the endpoint (`cat .livefolders/tools/weather/forecast`) — FUSE `read()` drains `result_buf`
6. A `.log` companion file is written to `trace_buf` with `duration_ms`, `exit`, and `stderr`

For `read_invoke` endpoints, invocation is triggered on `read()` instead of `release()`.

### Module map

| Module | Role |
|--------|------|
| `src/fs/vfs.rs` | Core FUSE implementation (`LiveFolders` struct). Handles all VFS calls and dispatches to tools. |
| `src/fs/inode.rs` | Inode number scheme: built-in tools use a static range (1000–100000), external tools use dynamically allocated inodes ≥ 100000. |
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

The FUSE thread is synchronous. Handler invocations use `rt.block_on(...)` on a Tokio runtime created after any fork. When `state_file` is declared, an exclusive `flock` is acquired before the handler runs, serialising concurrent invocations of the same endpoint.

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
