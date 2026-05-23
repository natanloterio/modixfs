# Concurrency Refactor Plan

Target: make LiveFolders correctly and concurrently serve multiple simultaneous
clients on the same endpoint, with handler invocations running in parallel and
no shared per-inode state.

This plan supersedes items 2.5, 2.6, 2.7, and the relevant parts of 1.6 in
`IMPROVEMENTS.md`.

---

## 0. Scope and non-goals

**In scope**

- Replace inode-keyed buffers with per-file-handle state.
- Make handler invocation non-blocking on the FUSE dispatcher thread.
- Snapshot manifest data at `open()` time so hot-reload cannot corrupt an
  in-flight invocation.
- Hard-kill (`SIGKILL`) timed-out handlers.
- Lifecycle: TTL + reaper for abandoned `OpenFile` slots.

**Out of scope (separate work)**

- Sandbox changes.
- MCP transport unification.
- Splitting `vfs.rs` into multiple modules. (This refactor will *enable* that
  split but does not depend on it; do it after Phase 2 lands.)
- Cross-tool shared resource locking beyond the existing `state_file` flock.
  We will *document* the model rather than expand it in this pass.

---

## 1. Target architecture

### 1.1 New core types (`src/fs/open_file.rs`, new file)

```rust
pub struct OpenFile {
    pub fh: u64,
    pub ino: u64,
    pub kind: FileKind,
    pub snapshot: EndpointSnapshot,
    pub write_buf: Vec<u8>,
    pub invocation: InvocationState,
    pub opened_at: Instant,
    pub last_touched: Instant,
}

pub struct EndpointSnapshot {
    pub tool_name: String,
    pub file_name: String,
    pub cwd: PathBuf,
    pub handler: Option<String>,
    pub input_schema: Option<InputSchema>,
    pub state_file: Option<PathBuf>,
    pub pipe: Option<Vec<String>>,
    pub timeout_secs: u64,
    pub sandbox: Arc<dyn Sandbox>,
    pub manifest_version: u64,   // for diagnostics only
}

pub enum InvocationState {
    Idle,
    Pending(oneshot::Receiver<ToolResult>),
    Ready { output: Bytes, cursor: usize, trace: Bytes },
    Failed { error: String, trace: Bytes },
}
```

`Bytes` (from the `bytes` crate) is used so multiple readers on the same `fh`
can cheaply share the same buffer; we will not need it across `fh`s.

### 1.2 New table (`src/fs/open_table.rs`, new file)

```rust
pub struct OpenTable {
    next_fh: AtomicU64,                // monotonic, never reused within a mount
    open: DashMap<u64, Arc<Mutex<OpenFile>>>,
}

impl OpenTable {
    pub fn allocate(&self, snapshot: EndpointSnapshot, ino: u64, kind: FileKind) -> u64;
    pub fn get(&self, fh: u64) -> Option<Arc<Mutex<OpenFile>>>;
    pub fn release(&self, fh: u64) -> Option<OpenFile>;
    pub fn reap_idle(&self, max_idle: Duration) -> usize;
    pub fn len(&self) -> usize;
}
```

- `DashMap` for lock-free per-fh access.
- `Arc<Mutex<OpenFile>>` so a Tokio task holding the lock cannot block the
  FUSE thread on a *different* fh.
- `next_fh` is monotonic; no reuse within the process lifetime (`u64` is
  plenty). Avoids ABA on slow clients.

### 1.3 `LiveFolders` struct changes (`src/fs/vfs.rs`)

```rust
// Remove:
//   write_buf:  Arc<Mutex<HashMap<u64, Vec<u8>>>>
//   result_buf: Arc<Mutex<HashMap<u64, Vec<u8>>>>
//   trace_buf:  Arc<Mutex<HashMap<u64, Vec<u8>>>>
// Add:
open_table: Arc<OpenTable>,
manifest_version: Arc<AtomicU64>,   // bumped by watcher on each reload
```

`trace_buf` for `.log` companion files is replaced by reading from the
*most-recent* completed invocation per inode; see §1.6.

### 1.4 Snapshot taken at `open()`

We implement `fn open(&mut self, …, ReplyOpen)` (currently not overridden):

1. Look up `(tool_name, file_name, spec)` for the inode.
2. If the inode is a manifest endpoint, build an `EndpointSnapshot` by
   `clone()`ing the relevant fields and the current `Arc<dyn Sandbox>`.
3. Allocate `fh` via `OpenTable::allocate`.
4. Reply with `fh` and `FOPEN_DIRECT_IO` (so the kernel doesn't page-cache
   our results — each `read` reaches us, and offsets are honest).

Hot-reload (`src/watcher.rs`) bumps `manifest_version` and swaps the registry,
but **does not touch** any live `OpenFile`. In-flight invocations finish
against their pinned snapshot.

### 1.5 Read/write/release on a manifest endpoint

```text
open()      → snapshot + allocate fh; FOPEN_DIRECT_IO
write()     → append to OpenFile.write_buf
release()   → if WriteInvoke: spawn invocation, store oneshot::Receiver
              if ReadInvoke:  no-op (input is consumed on first read)
              if Passthrough: write OpenFile.write_buf to disk
              drop fh from OpenTable iff state in {Idle, Ready, Failed}
              else mark "release_pending" — drop after read drains
read()      → state machine in §1.6
```

### 1.6 Read state machine

For both `WriteInvoke` (after release) and `ReadInvoke` (on first read):

```
match state {
    Idle if kind == ReadInvoke => {
        let (tx, rx) = oneshot::channel();
        tokio::spawn(run_invocation(snapshot.clone(), input, tx));
        state = Pending(rx);
        defer_reply(reply, fh);   // see §1.7
    }
    Idle if kind == WriteInvoke => {
        // release() never ran or invocation rejected (empty input).
        reply.data(&[]);
    }
    Pending(rx) => defer_reply(reply, fh),
    Ready { output, cursor, .. } => {
        let slice = &output[cursor..min(cursor + size, output.len())];
        cursor += slice.len();
        reply.data(slice);
        if cursor >= output.len() { /* keep Ready so .log read works */ }
    }
    Failed { error, .. } => reply.data(error.as_bytes()),
}
```

`.log` companion files read `OpenFile.invocation.trace`. They share the
parent endpoint's fh via a parallel `OpenFile` allocated at `open(.log)` —
when allocated for a `.log`, we *clone the last-completed trace* off the
parent's most-recent `OpenFile` by inode (kept in a small `DashMap<ino,
Bytes>` of "last trace" for cross-fh visibility).

### 1.7 Non-blocking FUSE thread (`defer_reply`)

Today: `rt.block_on(invoke_command_validated(…))` stalls the FUSE thread for
the full handler duration.

After:

```rust
fn defer_reply(reply: ReplyData, fh: u64, table: Arc<OpenTable>, rt: Handle, offset, size) {
    rt.spawn(async move {
        let of = table.get(fh)?;
        let result = {
            let mut g = of.lock().await;
            match &mut g.invocation {
                InvocationState::Pending(rx) => rx.await.ok(),
                _ => None,
            }
        };
        // Re-lock, transition to Ready, slice from offset.
        let mut g = of.lock().await;
        g.transition_to_ready(result);
        let slice = g.slice(offset, size);
        reply.data(&slice);
    });
}
```

Critical correctness points:

- `fuser`'s `Reply*` types are `Send`. We move them into a Tokio task.
- The FUSE dispatcher thread returns *immediately* after `defer_reply`, so
  the next `read`/`write`/`open` on any inode is unblocked.
- We use `tokio::sync::Mutex` (not `std::sync::Mutex`) on `OpenFile` so
  the async task can hold the lock across `.await` points safely.

Result: N concurrent `cat`s on N different endpoints run N handlers in
parallel. Two concurrent `cat`s on the *same* endpoint each get their own
fh, their own invocation, and their own output.

### 1.8 Timeout + hard kill (`src/tools/external.rs`)

Replace the existing `child.kill().await` path with:

```rust
match timeout(Duration::from_secs(t), child.wait()).await {
    Ok(status) => status,
    Err(_elapsed) => {
        let _ = child.start_kill();             // SIGTERM via tokio
        match timeout(Duration::from_secs(1), child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                if let Some(pid) = child.id() {
                    // SAFETY: pid is fresh from this Child; no races.
                    unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                }
                let _ = child.wait().await;
            }
        }
        return ToolResult::err(format_error("TIMEOUT", …));
    }
}
```

### 1.9 Reaper for abandoned fhs

A background Tokio task at mount start:

```rust
rt.spawn({
    let table = open_table.clone();
    async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let reaped = table.reap_idle(Duration::from_secs(900));
            if reaped > 0 {
                tracing::warn!(count = reaped, "reaped abandoned OpenFile slots");
            }
        }
    }
});
```

`reap_idle` drops any `OpenFile` whose `last_touched` is older than the TTL.
`SIGKILL`s the child if its invocation is still `Pending`.

---

## 2. Phased rollout

Each phase is independently shippable and individually testable. Land in
order; do not skip.

### Phase 0 — scaffolding (no behaviour change)
- Add `bytes`, `dashmap`, `tokio = { features = ["sync"] }` deps.
- Create `src/fs/open_file.rs`, `src/fs/open_table.rs`.
- Implement `fn open()` and `fn opendir()` that allocate but **do not**
  change read/write/release paths yet (per-fh state is created and dropped
  but unused).
- Wire `manifest_version` into the watcher; verify it ticks on reload.
- Tests: unit tests for `OpenTable::allocate / get / release / reap_idle`.

### Phase 1 — per-fh write buffer
- `write()` writes into `OpenFile.write_buf` (look up via `fh`).
- `release()` for `WriteInvoke` consumes `OpenFile.write_buf` and calls
  `rt.block_on(invoke_command_validated(…))` (still synchronous!) and
  stores the result into `OpenFile`.
- `read()` for `WriteInvoke` reads from `OpenFile.invocation` (the
  `Ready` variant), no longer from the global `result_buf`.
- Delete `LiveFolders.write_buf` and `LiveFolders.result_buf`.
- Tests: integration test "two writers, two readers, four distinct messages".

### Phase 2 — per-fh read buffer for `ReadInvoke`
- `read()` for `ReadInvoke` triggers invocation on first read of the fh,
  caches in `OpenFile.invocation`, drains across multiple `read` syscalls
  on the same fh.
- Tests: integration test "two simultaneous `cat`s on the same
  `ReadInvoke` endpoint produce two distinct invocations" (assert
  `invocation_count == 2` via a counter the test handler writes).

### Phase 3 — async dispatch
- Introduce `defer_reply()` (see §1.7).
- All five `rt.block_on(...)` sites in `vfs.rs` replaced with
  `rt.spawn(...)` + deferred reply.
- The FUSE dispatcher thread should never block on a handler.
- Tests:
  - "two parallel slow handlers complete in ~max(t1,t2), not t1+t2"
    (use sleeps; tolerance ±200 ms).
  - "open/read on tool B is not blocked by a stuck handler on tool A".

### Phase 4 — lifecycle
- TTL reaper task (§1.9).
- Metrics: gauge for `open_files`, counter for `reaped_files`.
- `livefolders doctor` reports current `open_files` count.
- Tests: kill a client mid-write; assert reaper drops the slot within 2×TTL.

### Phase 5 — timeout hard kill
- Implement §1.8 in `external.rs`.
- Tests: handler that traps `SIGTERM` and sleeps; assert kill within
  `timeout_secs + 1s + epsilon` and that the child is reaped (no zombies).

### Phase 6 — docs + cleanup
- Update `CLAUDE.md` (the §"Data flow" section) to describe per-fh model.
- Update `docs/building-tools.md` with "Concurrency model" section:
  - Each `open()` gets its own invocation.
  - Handlers must be idempotent or use `state_file`.
  - SIGTERM → 1 s → SIGKILL.
- Delete dead code paths (the inode-keyed maps, any `.log` synthesis from
  the old global trace map).

---

## 3. Test plan

### 3.1 Unit
- `OpenTable::allocate` monotonic and unique under contention (10k threads).
- `OpenFile` state machine: every reachable transition, including
  `Pending` → cancellation.
- `EndpointSnapshot` clone is cheap (Arc on sandbox).

### 3.2 Integration (`tests/concurrency.rs`, new)
- Two concurrent writers on one endpoint produce two distinct invocations
  with two distinct outputs delivered to the right readers.
- 100 concurrent `cat`s on a `ReadInvoke` endpoint produce 100
  invocations (verified by a counter file under `state_file` flock).
- Hot reload of `folder.yaml` between `open()` and `release()` uses the
  pre-reload handler. Bump `manifest_version`, observe in-flight
  invocation still completes against the original snapshot.
- `SIGKILL` escalation: handler ignores SIGTERM; assert it is dead within
  `timeout + 1.5 s` and `ps` shows no zombie.
- Reaper: open a file, drop the FD without `release()`; assert the slot
  is gone within 2× TTL.

### 3.3 Stress
- 1000 concurrent invocations over 60 s; assert no fd leak (`/proc/$pid/fd`
  count stable), no memory growth beyond a soft cap, no panics, and
  `open_table.len()` returns to 0 within 5 s of completion.

### 3.4 Loom (optional, valuable)
- Loom-model the `OpenFile` state machine to prove no deadlock under
  arbitrary task scheduling. Restrict to the `Pending` ↔ `Ready`
  transitions; sandbox/handler are out of model scope.

---

## 4. Risk register

| Risk | Mitigation |
|---|---|
| `fuser` reply types not `Send` on some platform variant | Verify in Phase 0 against macOS + Linux fuser builds; if not `Send`, marshal via channel back to the FUSE thread (slower but works). |
| `FOPEN_DIRECT_IO` changes user-visible `cat` buffering | Acceptable: this filesystem is RPC-shaped, not a file store. Document. |
| Hot reload semantics surprise tool authors | Document in `building-tools.md`: "an open `fh` is bound to the manifest at the moment of `open()`; close + reopen to pick up a reloaded `folder.yaml`." |
| `tokio::sync::Mutex` deadlock from async tasks holding across `.await` | Code review rule: never hold an `OpenFile` lock across an `await` that is not a `oneshot::Receiver::await`. Add a clippy lint config. |
| `next_fh: AtomicU64` exhaustion | At 1 M opens/sec it lasts 580k years. Not a real risk. |
| Per-fh memory for large outputs | Cap `OpenFile.invocation.output` at e.g. 16 MiB; truncate with `[ERROR:TRUNCATED]` marker. Configurable in `livefolders.yaml`. |
| Existing tools assume "last result is sticky on inode" | None do today (verified by reading examples). Document the change. |

---

## 5. Estimated effort

| Phase | Engineer-days | Notes |
|---|---|---|
| 0 — scaffolding | 1 | New files, deps, no behaviour change. |
| 1 — per-fh write buffer | 2 | Most of the routing fix lives here. |
| 2 — per-fh read buffer | 1 | Mostly mechanical after Phase 1. |
| 3 — async dispatch | 3 | Highest-risk phase; needs careful review. |
| 4 — lifecycle | 1 | Reaper, metrics, doctor integration. |
| 5 — timeout hard kill | 1 | Self-contained. |
| 6 — docs + cleanup | 1 | Delete dead code; update three docs. |
| Tests across phases | 2 | Largely written alongside each phase. |
| **Total** | **~12 days** | One engineer, including review time. |

---

## 6. Acceptance criteria

The refactor is done when *all* of these hold:

1. `grep -n 'rt.block_on' src/fs/vfs.rs` returns no hits.
2. `grep -n 'write_buf\|result_buf\|trace_buf' src/fs/vfs.rs` returns no hits.
3. The new integration test "two concurrent `cat`s on the same endpoint
   produce two distinct invocations" passes on Linux and macOS CI.
4. A stress test of 1000 concurrent invocations completes with stable
   memory and zero leaked file handles.
5. A handler that ignores SIGTERM is killed within `timeout + 1.5 s` in
   `tests/concurrency.rs::sigkill_escalation`.
6. `CLAUDE.md`, `docs/building-tools.md` reflect the per-fh model.
7. `IMPROVEMENTS.md` items 2.5, 2.6, 2.7 and 1.6 are marked resolved.
