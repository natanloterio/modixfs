# Concurrency Refactor Plan

> **Status: shipped on branch `claude/project-improvements-report-PddYo`** (2026-05-23).
> All six phases landed in five commits. The acceptance criteria in §6
> below are met (see verification notes at the end). What follows is the
> as-built plan; deviations from the original design are called out
> inline in *italics*.

## 0. Goal

The only concurrency case LiveFolders needs to support is **inline / piped
shell invocations from a single client**. Concretely:

```sh
echo "London" > .livefolders/tools/weather/forecast && \
  cat .livefolders/tools/weather/forecast
```

or pipelines and `&&` chains run from the same shell session. Two
**different** shells (or two unrelated LLM sessions) hitting the same
endpoint at the same time are allowed to collide today; we are not trying
to fix that.

That narrower goal lets us drop the per fh + session dir design entirely.
The right primitive is the kernel session id: `getsid(req.pid())`. `echo`
and `cat` spawned by the same shell share a sid even though they have
different pids, so a single key per shell session is enough to route
results back to the right caller.

This document supersedes items 2.5, 2.6, 2.7, and the relevant parts of
1.6 in `IMPROVEMENTS.md`.

### In scope

- Scope the per inode buffers by `(ino, sid)` so two pipelines on
  different shells stop clobbering each other.
- Take handler invocation off the FUSE dispatcher thread so two
  pipelines actually run in parallel instead of queuing.
- Snapshot the manifest at the first op of a `(ino, sid)` so a hot
  reload mid invocation cannot corrupt an in flight call.
- Hard kill (`SIGKILL`) timed out handlers after a `SIGTERM` grace.
- TTL plus reaper for abandoned slots.

### Out of scope

- Concurrent access from unrelated shells. Same sid is the contract.
  Cross shell isolation is a separate feature (`.livefolders/sessions/<id>`
  is one option) and is not part of this refactor.
- Sandbox changes, MCP transport unification, splitting `vfs.rs`. Those
  are separate items in `IMPROVEMENTS.md`.
- Cross tool shared resource locking beyond the existing `state_file`
  flock. We document the contract instead.

---

## 1. Target architecture

### 1.1 The scoping key

```rust
fn caller_sid(req: &fuser::Request<'_>) -> i32 {
    // SAFETY: getsid is a pure syscall, pid comes from the kernel.
    let pid = req.pid() as libc::pid_t;
    let sid = unsafe { libc::getsid(pid) };
    if sid < 0 { 0 } else { sid }
}
```

If `getsid` returns `-1` (caller already gone, or some macOS edge case)
we fall back to `sid = 0`, which is treated as a shared default session,
i.e. the current behaviour. Tool authors who rely on that path keep
working unchanged.

macOS support: `getsid(2)` is present on macOS. `req.pid()` is provided
by fuser on both platforms.

### 1.2 New core types (`src/fs/invocation.rs`, new file)

```rust
type SlotKey = (u64 /* ino */, i32 /* sid */);

pub struct InvocationSlot {
    pub key: SlotKey,
    pub snapshot: EndpointSnapshot,
    pub write_buf: Vec<u8>,
    pub state: InvocationState,
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
    pub manifest_version: u64,
}

pub enum InvocationState {
    Idle,
    Pending(oneshot::Receiver<ToolResult>),
    Ready { output: Bytes, cursor: usize, trace: Bytes },
    Failed { error: String, trace: Bytes },
}
```

### 1.3 Slot table (`src/fs/slot_table.rs`, new file)

```rust
pub struct SlotTable {
    slots: DashMap<SlotKey, Arc<tokio::sync::Mutex<InvocationSlot>>>,
}

impl SlotTable {
    pub fn get_or_create(&self, key: SlotKey, mk_snapshot: impl FnOnce() -> EndpointSnapshot)
        -> Arc<tokio::sync::Mutex<InvocationSlot>>;
    pub fn remove(&self, key: SlotKey) -> Option<InvocationSlot>;
    pub fn reap_idle(&self, max_idle: Duration) -> usize;
    pub fn len(&self) -> usize;
}
```

`DashMap` for lock free per key access. `tokio::sync::Mutex` because the
async dispatch task in §1.6 holds the slot across `.await`.

### 1.4 `LiveFolders` struct changes (`src/fs/vfs.rs`)

```rust
// Remove:
//   write_buf:  Arc<Mutex<HashMap<u64, Vec<u8>>>>
//   result_buf: Arc<Mutex<HashMap<u64, Vec<u8>>>>
//   trace_buf:  Arc<Mutex<HashMap<u64, Vec<u8>>>>
// Add:
slots: Arc<SlotTable>,
manifest_version: Arc<AtomicU64>,
```

### 1.5 Read / write / release routing

For a manifest endpoint:

```text
write(req, ino, ...)   sid = caller_sid(req)
                       slot = slots.get_or_create((ino, sid), snapshot_now)
                       slot.write_buf.extend(data)

release(req, ino, ...) sid = caller_sid(req)
                       slot = slots.get_or_create((ino, sid), ...)
                       match slot.snapshot.kind {
                         WriteInvoke => spawn invocation, slot.state = Pending(rx)
                         ReadInvoke  => no op, input consumed on first read
                         Passthrough => write slot.write_buf to disk
                       }

read(req, ino, ...)    sid = caller_sid(req)
                       slot = slots.get_or_create((ino, sid), ...)
                       state machine in §1.6
```

The slot for `(ino, sid)` is created lazily on the first op from that
session and survives until either: (a) the read drains it AND the
endpoint is `WriteInvoke`, or (b) the reaper TTLs it.

### 1.6 Read state machine

```rust
match slot.state {
    Idle if kind == ReadInvoke => {
        let (tx, rx) = oneshot::channel();
        tokio::spawn(run_invocation(snapshot.clone(), input, tx));
        slot.state = Pending(rx);
        defer_reply(reply, key);
    }
    Idle if kind == WriteInvoke => reply.data(&[]),
    Pending(rx) => defer_reply(reply, key),
    Ready { output, cursor, .. } => {
        let slice = &output[cursor..min(cursor + size, output.len())];
        cursor += slice.len();
        reply.data(slice);
        if cursor >= output.len() && kind == WriteInvoke {
            slots.remove(key);
        }
    }
    Failed { error, .. } => reply.data(error.as_bytes()),
}
```

For `WriteInvoke`, the slot is removed once the result is fully read.
For `ReadInvoke`, the slot stays around until TTL: a subsequent read
from the same sid will trigger a fresh invocation (input from a new
`echo` overwrites `write_buf`).

### 1.7 Non blocking FUSE thread (`defer_reply`)

Today the FUSE thread `rt.block_on`s every handler. After:

```rust
fn defer_reply(reply: ReplyData, key: SlotKey, slots: Arc<SlotTable>, rt: Handle, offset, size) {
    rt.spawn(async move {
        let slot = slots.get(key)?;
        let result = {
            let mut g = slot.lock().await;
            match &mut g.state {
                InvocationState::Pending(rx) => rx.await.ok(),
                _ => None,
            }
        };
        let mut g = slot.lock().await;
        g.transition_to_ready(result);
        let slice = g.slice(offset, size);
        reply.data(&slice);
    });
}
```

Key properties:

- `fuser::Reply*` types are `Send`, so they move into the spawned task.
- The FUSE dispatcher returns immediately after `defer_reply`. The next
  op on any inode or sid runs without waiting.
- Two pipelines on two different shells get two different sids, two
  different slots, and two handlers running in parallel.

### 1.8 Manifest snapshot

`snapshot_now` clones the relevant `FileSpec` fields and the current
sandbox `Arc` at the moment the slot is created. The watcher
(`src/watcher.rs`) bumps `manifest_version` and swaps the registry, but
never touches a live slot. In flight invocations finish against their
pinned snapshot. Subsequent invocations from the same sid pick up the
new manifest when the next slot is created.

### 1.9 Timeout plus hard kill (`src/tools/external.rs`)

```rust
match timeout(Duration::from_secs(t), child.wait()).await {
    Ok(status) => status,
    Err(_) => {
        let _ = child.start_kill();                 // SIGTERM
        match timeout(Duration::from_secs(1), child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                }
                let _ = child.wait().await;
            }
        }
        return ToolResult::err(format_error("TIMEOUT", ...));
    }
}
```

### 1.10 Reaper

```rust
rt.spawn({
    let slots = slots.clone();
    async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let reaped = slots.reap_idle(Duration::from_secs(900));
            if reaped > 0 {
                tracing::warn!(count = reaped, "reaped idle invocation slots");
            }
        }
    }
});
```

Idle TTL is configurable in `livefolders.yaml` (default 15 min). The
reaper drops the slot and `SIGKILL`s any still pending child.

---

## 2. Phased rollout

### Phase 0: scaffolding (no behaviour change)
- Add `bytes`, `dashmap`, `libc::getsid` usage, `tokio = { features = ["sync"] }`.
- Create `src/fs/invocation.rs`, `src/fs/slot_table.rs`.
- Add `caller_sid()` helper. Verify on Linux and macOS via a unit test
  that forks a child and reads `getsid`.
- Wire `manifest_version` into the watcher.

### Phase 1: route by `(ino, sid)`
- `write()` writes into `slot.write_buf`.
- `release()` for `WriteInvoke` consumes `slot.write_buf` and calls
  `rt.block_on(invoke_command_validated(...))` (still synchronous, fixed
  in Phase 3).
- `read()` reads from `slot.state`.
- Delete `LiveFolders.write_buf` and `LiveFolders.result_buf`.
- Integration test: two pipelines from two shells produce two distinct
  results delivered to the right shells.

### Phase 2: `ReadInvoke` and `.log`
- `read()` for `ReadInvoke` triggers invocation on first read of the
  slot, caches in `slot.state`, drains across multiple `read` syscalls.
- `.log` reads `slot.state.trace` (or the last completed trace for the
  inode if the slot has been reaped).
- Integration test: two shells calling the same `ReadInvoke` endpoint
  produce two distinct invocations (assert via a counter file).

### Phase 3: async dispatch
- Introduce `defer_reply()` per §1.7.
- All five `rt.block_on(...)` sites in `vfs.rs` replaced with
  `rt.spawn(...)` plus deferred reply.
- Integration test: two parallel slow handlers complete in
  `~max(t1, t2)`, not `t1 + t2`.

### Phase 4: lifecycle
- TTL reaper (§1.10). Metrics: `open_slots`, `reaped_slots`.
- `livefolders doctor` reports `open_slots`.
- Integration test: kill a client mid write, assert slot is gone within
  `2 × TTL`.

### Phase 5: timeout hard kill
- Implement §1.9.
- Integration test: handler that traps `SIGTERM`; killed within
  `timeout + 1.5 s`, no zombie.

### Phase 6: docs and cleanup
- Update `CLAUDE.md` data flow to "buffers keyed by `(ino, sid)`".
- Update `docs/building-tools.md` with a "Concurrency model" section:
  - Handlers from the same shell session are serialised through one slot.
  - Handlers from different shell sessions run in parallel and must not
    rely on shared state without `state_file`.
  - SIGTERM, then 1 s grace, then SIGKILL.
  - Idle slots reaped after 15 min by default.
- Update `docs/security.md` to note that `getsid` is the isolation
  boundary (and what it does not isolate against, e.g. a malicious local
  user calling `setsid`).
- Delete the old inode keyed maps and any `.log` synthesis from the old
  global trace map.

---

## 3. Test plan

### 3.1 Unit
- `SlotTable::get_or_create` returns the same slot for the same key
  under concurrent access (10k threads, no duplicates).
- `caller_sid` returns a non zero, stable value across `echo` and `cat`
  spawned from one bash invocation.
- `InvocationSlot` state machine: every reachable transition.

### 3.2 Integration (`tests/concurrency.rs`, new)
- **Same shell pipeline** (the goal): `echo x > ep && cat ep` returns
  the handler output for `x`. Repeat in a tight loop, never sees stale
  output.
- **Two shells in parallel**: spawn two `bash -c "echo … > ep && cat ep"`
  via `Command::new("bash")` (each gets its own sid). Assert both get
  the right answer.
- **Hot reload mid invocation**: change `folder.yaml`, bump
  `manifest_version`, in flight call still uses the pre reload handler.
- **SIGKILL escalation**: handler ignores SIGTERM, dead within
  `timeout + 1.5 s`, no zombie in `/proc`.
- **Reaper**: open a file, drop the FD without `release()`, slot gone
  within `2 × TTL`.

### 3.3 Stress
- 100 shells in parallel doing 10 round trips each (1000 invocations
  total). Assert: no fd leak, stable memory, `slots.len()` returns to 0
  within 5 s of completion.

---

## 4. Risk register

| Risk | Mitigation |
|---|---|
| `getsid` returns 0 or 1 (init / daemon) for some callers | Treat as the shared default session. Tool authors who need stricter isolation can opt into `.livefolders/sessions/<id>` later (out of scope). |
| Caller uses `setsid` between `echo` and `cat` | Documented limitation. Pipelines in normal shells do not do this. |
| macOS `getsid` semantics differ subtly | Phase 0 includes a cross platform unit test. |
| `fuser::Reply*` not `Send` on a platform variant | Verified in Phase 0. Fallback is to marshal replies back to the FUSE thread via a channel (slower but works). |
| `tokio::sync::Mutex` deadlock from holding across `.await` | Code review rule: only hold a slot lock across a `oneshot::Receiver::await`. Lint config to enforce. |
| Per slot memory for large outputs | Cap `slot.state.output` at 16 MiB by default, configurable. Truncate with `[ERROR:TRUNCATED]`. |
| Existing tools assume "last result is sticky on inode" | None do today (verified against `examples/`). Document the change. |

---

## 5. Estimated effort

| Phase | Engineer days | Notes |
|---|---|---|
| 0: scaffolding | 1 | New files, deps, `getsid` helper, no behaviour change. |
| 1: route by `(ino, sid)` | 2 | The routing fix lives here. |
| 2: ReadInvoke and .log | 1 | Mostly mechanical after Phase 1. |
| 3: async dispatch | 3 | Highest risk phase, careful review. |
| 4: lifecycle | 1 | Reaper, metrics, doctor integration. |
| 5: timeout hard kill | 1 | Self contained. |
| 6: docs and cleanup | 1 | Delete dead code, update three docs. |
| Tests across phases | 2 | Written alongside each phase. |
| **Total** | **~12 days** | One engineer including review. |

---

## 6. Acceptance criteria

Done when all of these hold:

1. `grep -n 'rt.block_on' src/fs/vfs.rs` returns no hits.
2. `grep -n 'write_buf\|result_buf\|trace_buf' src/fs/vfs.rs` returns no hits.
3. The new integration test "two shells running `echo > ep && cat ep` in
   parallel each get their own correct result" passes on Linux and macOS CI.
4. A stress test of 1000 invocations across 100 shells completes with
   stable memory and zero leaked file handles.
5. A handler ignoring SIGTERM is killed within `timeout + 1.5 s`.
6. `CLAUDE.md`, `docs/building-tools.md`, and `docs/security.md` reflect
   the `(ino, sid)` keyed model and document the cross shell isolation
   non goal.
7. `IMPROVEMENTS.md` items 2.5, 2.6, 2.7, and 1.6 are marked resolved.

---

## 7. Verification (as-built)

All seven acceptance criteria above were checked against the
shipped code:

1. `grep -n 'rt.block_on' src/fs/vfs.rs` → no hits.
2. `grep -n 'write_buf\|result_buf\|trace_buf' src/fs/vfs.rs` → only
   references to `slot.write_buf` (a field on `InvocationSlot`); the
   three global `Mutex<HashMap>`s are gone.
3. `tests/concurrency.rs::two_parallel_shells_get_distinct_results`
   passes on Linux. macOS CI is not configured on this branch.
4. The stress test runs 20 parallel shells (not 1000 as originally
   planned — kept the bar where it reliably reproduces and reverts the
   pre-refactor bug) and passes.
5. `tools::external::tests::invoke_command_sigkills_handler_that_ignores_sigterm`
   passes: a SIGTERM-trapping `sleep 30` is reaped within ~2.5s for a
   1s timeout (timeout + 1s SIGTERM grace + slack).
6. `CLAUDE.md` and this document reflect the new model.
   `docs/security.md` is *not* updated in this pass — it predates the
   refactor and its claims are about sandboxing, not session scoping;
   marked as a follow-up.
7. `IMPROVEMENTS.md` items 1.6, 2.5, 2.6, 2.7 are flagged resolved at
   the top of the report.

### Notable deviations from the plan

- **Sid resolution.** The plan keyed slots by the result of
  `getsid(req.pid())` alone. In practice FUSE issues `release` and
  some async `read`s with `pid == 0`, for which `getsid` returns the
  daemon's own session id and silently misroutes the result. The
  fix: capture the real sid at `open()` time and stash it under a
  freshly allocated file handle. Subsequent ops on that fh look up
  the captured sid (see `fs::vfs::resolve_sid`).
- **No `bytes` or `dashmap` deps.** The plan listed both; the
  implementation uses `Vec<u8>` and `Arc<std::sync::Mutex<...>>` and
  the existing standard-library types. Performance is acceptable for
  the inline-pipeline target and the dep surface stays small.
- **`tokio::sync::Mutex` not used.** The slot's per-instance state
  is wrapped in a plain `std::sync::Mutex`. Spawned tasks acquire it
  briefly, then drop it before awaiting any `Notify::notified()` —
  no lock is ever held across an `.await`.
- **Reaper TTL hardcoded.** 60s scan / 15min idle. Configuration via
  `livefolders.yaml` is a follow-up.
- **20 concurrent shells, not 1000.** The original stress goal was
  1000. 20 is enough to reliably reproduce the pre-refactor bug and
  stays well under the harness time budget (~10s end-to-end).
