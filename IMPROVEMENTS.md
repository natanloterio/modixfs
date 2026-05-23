# LiveFolders — Improvements Report

Date: 2026-05-22
Scope: full repo at `v0.21.1` (~8.4k LOC Rust, plus docs and examples).

This report is the result of three independent reviews (architecture/code,
security, UX/docs/ops) consolidated and de-duplicated, with each finding
verified against the actual source. Findings are grouped by theme, with
severity / effort labels so you can pick a slice and ship it.

> **Status update (2026-05-23):** items 1.6, 2.5, 2.6, 2.7 are **resolved**
> by the concurrency refactor — see `docs/concurrency-refactor.md` and the
> Phase 0–5 commits on this branch.

---

## TL;DR — top 10 things to do next

1. **Scope secrets per-handler** instead of dumping all of `secrets.env` into
   the daemon's process environment (every handler inherits everything).
2. **Drop `MountOption::AllowOther`** (or gate it behind an explicit flag) —
   right now every local user can `cat` your tool outputs.
3. **Pin/verify installer downloads** — `livefolders install <github-url>`
   currently has no checksum, signature, or TOFU step.
4. **Restrict the MCP socket** to `0600` and verify peer credentials.
5. **Split `src/fs/vfs.rs` (1268 lines) and `src/main.rs` (1021 lines)**
   into focused modules and `clap`-based subcommands.
6. **Add buffer cleanup on `release()` / inode reuse** — `write_buf`,
   `result_buf`, `trace_buf` currently grow without bound on long-running mounts.
7. **Replace the global `Mutex<HashMap>`s** behind the FUSE buffers with a
   `DashMap` (or per-inode locks) — every FUSE op currently contends on one lock.
8. **Add a `folder.yaml` `schema_version` field** and validate it — there is
   no migration story today.
9. **Add unit tests for the VFS, inode allocator, and sandbox** — coverage
   is essentially one e2e test that needs `ANTHROPIC_API_KEY` to run.
10. **Add `livefolders scaffold <name>`** so tool authors get a working
    starter `folder.yaml` instead of grepping `docs/building-tools.md`.

---

## 1. Security

### 1.1 [HIGH] Secrets leak into every handler subprocess
`src/secrets.rs:31` calls `unsafe { std::env::set_var(...) }` for every line
of `~/.config/livefolders/secrets.env` at daemon startup. Those env vars are
inherited by every `sh -c <handler>` spawn in `src/tools/external.rs:54,340`,
regardless of whether the tool's manifest declared a need for them.

**Fix:** thread a "requested secrets" list from `manifest::env` through to
`invoke_command_sandboxed`. Build a filtered environment per spawn (clear
inherited env, then inject only `LIVEFOLDERS_*` and the requested keys).

### 1.2 [HIGH] FUSE mount uses `AllowOther`
`src/main.rs:487` unconditionally adds `MountOption::AllowOther`. Combined
with a default mount path under `/mnt` or the user's home, any local
account on the host can read endpoint outputs — which legitimately contain
API responses, secrets, and tool state.

**Fix:** remove `AllowOther`, or make it opt-in via `livefolders.yaml`
(`mount: { allow_other: true }`). Default to owner-only.

### 1.3 [HIGH] Installer has no integrity check
`src/installer.rs` fetches a `folder.yaml` (and companion files) directly
from a GitHub URL. No checksum, no signature, no TOFU. A compromised
upstream repo (or anyone who can MITM TLS) gets arbitrary handler code
that then runs under the daemon's identity inside the sandbox.

**Fix:** support `github.com/owner/repo#sha256=...` pinning; allow
detached GPG signatures alongside `folder.yaml`; at minimum print the file
hashes and require `--yes` for non-interactive installs.

### 1.4 [MED] MCP proxy socket is world-accessible
`src/mcp_proxy/mod.rs` creates a Unix socket at a predictable path with
default umask (usually `0022` → world-readable). Anything on the box can
connect and invoke tools.

**Fix:** `umask(0o077)` around the bind, set `0600` on the path explicitly,
and check `SO_PEERCRED` against the daemon's uid on accept.

### 1.5 [MED] Secrets file permissions not re-checked at read time
`secrets.rs:append_secret` writes with `0o600`, but `load_secrets_env()`
does not re-verify the mode on read. A misconfigured backup, sync tool, or
`chmod` leaves the file world-readable with no warning.

**Fix:** in `load_secrets_env()`, refuse to read if
`metadata.permissions().mode() & 0o077 != 0`.

### 1.6 [MED] Timeout kill is best-effort
`src/tools/external.rs` calls `child.kill().await` on timeout but does not
escalate to `SIGKILL` if the handler ignores `SIGTERM`. Zombies accumulate
on long-running mounts.

**Fix:** after `kill()`, race a 1 s wait against the child; if the child
is still alive, `libc::kill(pid, SIGKILL)`.

### 1.7 [MED] macOS sandbox profile allows `process-exec*`
`src/sandbox/macos.rs` permits `(allow process-exec*)` unconditionally,
which materially weakens the confinement — anything reachable via the
filesystem rules can be `exec`'d.

**Fix:** allowlist `/bin/sh`, `/bin/bash`, the tool's own `cwd` binary,
and reject everything else.

### 1.8 [MED] Linux sandbox degrades silently on old kernels
`src/sandbox/linux.rs` warns but continues when landlock is unavailable
(< 5.13). Users who expected isolation get none.

**Fix:** make this a hard error by default (`sandbox: strict` in
`livefolders.yaml`); require explicit `sandbox: warn` to opt in.

### 1.9 [LOW] Shell handlers are passed via `sh -c` (note, not an LLM-injection bug)
Handlers are run as `sh -c "<handler_string>"` (`external.rs:54,340`). LLM
input arrives on **stdin**, not interpolated into the handler string, so
this is not an active shell-injection vector against LiveFolders. It is a
footgun for *tool authors* who write `grep $(cat input)` style handlers
in their own `folder.yaml`.

**Fix:** document the convention "use stdin, never substitute LLM input
into the handler string"; add a `livefolders doctor` lint that flags
suspicious patterns in handlers (e.g. `$(cat`, backticks around user-path
arguments).

### 1.10 [LOW] `docs/security.md` overstates guarantees
The document does not mention any of 1.1–1.4 above. Users may rely on a
threat model the code does not actually implement.

**Fix:** add explicit "Known limitations" and "Not in scope" sections.

---

## 2. Architecture & code quality

### 2.1 [LARGE] `src/fs/vfs.rs` is 1268 lines and mixes concerns
Inode allocation, FUSE dispatch, write/result/trace buffering, external file
I/O, and tool invocation are all interleaved.

**Suggestion:** extract `fs/buffers.rs` (the three `HashMap`s + their
lifecycle), `fs/external.rs` (passthrough files), and keep `vfs.rs` to the
FUSE trait impl only. Target < 500 lines per file.

### 2.2 [LARGE] `src/main.rs` is 1021 lines and rolls its own argv parsing
Every subcommand (init, mount, install, search, publish, doctor, mcp serve,
mcp proxy) lives in one function with a hand-maintained `USAGE` string.

**Suggestion:** move to `clap` derive, one file per command under
`src/commands/`. Auto-generated `--help`, completions, and far less
drift between docs and reality.

### 2.3 [LARGE] Two parallel MCP implementations
`src/mcp.rs` (491 lines, stdio MCP server) and `src/mcp_proxy/` (637 lines,
Unix-socket proxy + server pool) duplicate request/response wiring.

**Suggestion:** define a `Transport` trait, share the server core, and
keep stdio / unix-socket as thin adapters.

### 2.4 [MED] `src/tools/external.rs` is 978 lines
Bundles invocation, state-file locking, input validation, JSON-schema
checking, and subprocess management.

**Suggestion:** extract `tools/{invocation.rs, validation.rs, subprocess.rs}`.
The validator alone is worth its own module.

### 2.5 [MED] Synchronous `rt.block_on(...)` inside the FUSE thread
Five `block_on` sites in `vfs.rs` (lines ~613, 626, 814, 872, 919) stall the
single FUSE dispatcher for the full handler duration. Two concurrent
endpoint invocations serialize.

**Suggestion:** spawn invocations onto the Tokio runtime and return
`reply.error(EAGAIN)` immediately for the second concurrent invoke on the
same fd, or move to `fuser`'s async-friendly path. Profile first to confirm
the contention.

### 2.6 [SMALL] Lock contention on the buffer maps
`write_buf`, `result_buf`, `trace_buf` are `Mutex<HashMap<u64, Vec<u8>>>`.
Every FUSE op acquires the global lock even for disjoint inodes.

**Suggestion:** swap to `Arc<DashMap<...>>` (already-in-ecosystem crate)
or shard by `inode % N`. Trivial change, large concurrency win.

### 2.7 [SMALL] Buffer maps never shrink
Entries are inserted on first write/read but never removed on `release()`
or when a tool is unloaded. Long-running mounts leak.

**Suggestion:** in `release()`, `remove()` the entry once consumed. Track
total entries in a metric; warn at 10k.

### 2.8 [MED] Inode allocation scheme is fragile
`tool_ino = 1000 + idx*100`, `endpoint_ino = tool_ino + 10 + ep_idx`. This
caps tools at ~990 and endpoints at 90 per tool, wastes 90 slots per tool,
and reuse on tool unload is untested.

**Suggestion:** use a `BTreeSet<u64>` free-list plus a monotonically
increasing watermark. Document the cap. Add a reuse test.

### 2.9 [MED] Error handling is `anyhow` end-to-end
`src/error.rs` is 27 lines and exposes only `format_error(code, msg)`.
Everything else returns `anyhow::Result`, so callers cannot match on
specific failure modes (timeout vs. spawn vs. validation).

**Suggestion:** introduce a `LiveFoldersError` enum with the existing
codes (`INVALID_INPUT`, `HANDLER`, `TIMEOUT`, `SPAWN`, `PROCESS`) as
variants. Keep `anyhow` for CLI-level error chaining only.

### 2.10 [SMALL] `tokio = { features = ["full"] }`
Pulls ~20 sub-crates the daemon doesn't need.

**Suggestion:** trim to `["rt-multi-thread", "process", "time", "sync",
"io-util", "fs", "macros"]`. Faster builds, smaller binary.

### 2.11 [SMALL] `marketplace/` has no shared abstraction
`resolve.rs`, `search.rs`, `info.rs`, `publish.rs` repeat URL/auth setup.

**Suggestion:** one `Marketplace` struct that owns the client and exposes
the four operations as methods.

> Note: one of the source reviews flagged `edition = "2024"` in `Cargo.toml`
> as a typo. This is **not** a bug — Rust 2024 edition was stabilized in
> Rust 1.85 (Feb 2025). Keep it.

---

## 3. UX, docs, and ops

### 3.1 [SMALL] No `livefolders scaffold <name>`
Today the path from "I want to write a tool" to "I have a working
`folder.yaml`" goes through 213 lines of `docs/building-tools.md`.

**Suggestion:** ship a `scaffold` subcommand that drops a minimal,
heavily-commented `folder.yaml` + executable handler stub into
`tools/<name>/`.

### 3.2 [MED] `folder.yaml` has no schema version
Manifests parsed by `src/manifest.rs` carry no `schema_version`. Any
future breaking change (renamed `FileKind`, new required field) silently
breaks every installed tool.

**Suggestion:** add `schema_version: 1` as required; reject manifests
with unknown versions and point at a migration doc.

### 3.3 [SMALL] Default timeout is invisible
`config.rs` defaults `timeout` to 30 s. Tools that take 35 s fail with
`[ERROR:TIMEOUT]` and no hint that the limit is configurable.

**Suggestion:** include the limit in the error string and allow
`timeout_secs:` per `FileSpec` in `folder.yaml`.

### 3.4 [SMALL] `livefolders doctor` is missing common-pain checks
`src/doctor.rs` validates FUSE presence and YAMLs, but does not check:
membership in the `fuse` group, kernel landlock support, `inotify`
watch limit, free disk in the mount path, or whether the mount point
is already a FUSE mount.

**Suggestion:** add those checks with actionable remediation strings.

### 3.5 [SMALL] CLI errors leak `anyhow` backtraces
A bad `livefolders.yaml` prints a Rust derive error rather than
"`tools_dir: ./tools` does not exist; create it with `mkdir -p ./tools`."

**Suggestion:** centralize CLI-level error rendering; strip backtraces
unless `RUST_BACKTRACE` is set; map common errors to remediation hints.

### 3.6 [SMALL] Hot reload is invisible to clients
`src/watcher.rs` logs to stderr when tools change but the LLM has no way
to know `index.md` is stale.

**Suggestion:** maintain a `.livefolders/.reloaded_at` file with a
monotonic counter or timestamp; document it.

### 3.7 [MED] No CI on this repo
`.github/` has no `workflows/` running `cargo test`, `cargo clippy`, or
release builds. The only test (`tests/e2e_claude.rs`) requires
`ANTHROPIC_API_KEY` and `fusermount`.

**Suggestion:** add a GH Actions job that runs `cargo fmt --check`,
`cargo clippy -- -D warnings`, and `cargo test --lib` on Linux and macOS.
Gate the e2e suite behind a secret-protected job.

### 3.8 [SMALL] No CHANGELOG / migration guide
Going from 0.20 → 0.21 surfaces no notes for tool authors.

**Suggestion:** start a `CHANGELOG.md` with Keep-a-Changelog format; tag
breaking changes for tool authors specifically.

### 3.9 [SMALL] `install.sh` not idempotent / unversioned
The script always grabs the latest release. No pin, no upgrade-in-place
that preserves config.

**Suggestion:** accept `LIVEFOLDERS_VERSION=v0.20.0 ./install.sh`; bail
if a different version is already installed unless `--upgrade` is passed.

### 3.10 [SMALL] README has no asciinema / no "what does a session look like"
The README explains the concept but a 10-second screencast of
`mount → cat index.md → echo > endpoint → cat endpoint` would do far more
for adoption.

**Suggestion:** record an asciinema cast, embed via `<a href>`; the
existing `architecture.svg` is great but it's static.

### 3.11 [SMALL] `docs/building-tools.md` is one long page
Hard to navigate; mixes "hello world" with state files, pipes, secrets,
and sandbox config.

**Suggestion:** split into `basics.md`, `validation.md`, `advanced.md`
(state, pipes, secrets, sandbox), and add a "test locally without mount"
section.

### 3.12 [MED] No structured logging / per-invocation spans
`tracing` is set up, but there are no enclosing spans for
"install", "mount", or "invoke tool X endpoint Y". A user debugging a
flaky tool gets a flat log.

**Suggestion:** wrap each public entry point in a `tracing::info_span!`
with the relevant ids; offer `livefolders logs <tool>` to tail the
per-tool `.log` files.

---

## Suggested rollout order

1. **Week 1 — security hardening:** 1.1, 1.2, 1.3, 1.4, 1.5. All are
   small, all are user-visible safety wins.
2. **Week 2 — VFS hot-path:** 2.6, 2.7, 2.8. Concrete bug-class fixes
   (lock contention, buffer leak, fragile inode math).
3. **Week 3 — refactor wedge:** start 2.1 and 2.2 in parallel with a
   `clap` migration; this unblocks 3.1, 3.3, 3.4, 3.5.
4. **Week 4 — UX & CI:** 3.1, 3.4, 3.7, 3.10.
5. **Backlog:** 2.3, 2.4, 2.5, 2.9, 3.2 — bigger architectural moves; do
   when the refactor wedge has landed.

Rough total: ~3–4 focused weeks for a single engineer to land everything
above 1.5 and small/medium items; the three "large" refactors (2.1, 2.2,
2.3) are each their own ~1-week project.
