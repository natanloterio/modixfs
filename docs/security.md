# Security / Sandbox

Every tool handler runs in an isolated sandbox. Filesystem access is restricted to the paths the tool explicitly declares, and outbound network connections are blocked by default. This limits the blast radius if a handler misbehaves or is supplied malicious input.

## Platform details

| Platform | Mechanism |
|---|---|
| Linux (kernel ≥ 5.13) | Landlock LSM + seccomp socket filter |
| macOS | `sandbox-exec` (deprecated by Apple but functional on all current releases) |

Both platforms degrade gracefully: if the kernel or OS feature is unavailable, LiveFolders logs a warning and continues running without isolation.

## Network access

Network is denied by default. Add `network: true` to the `sandbox:` block in `folder.yaml` for any tool that needs to reach external services:

```yaml
name: weather
description: Get the weather forecast for any city.

sandbox:
  network: true

files:
  - name: forecast
    type: write_invoke
    handler: "curl -s \"https://wttr.in/$(cat -)?format=3\""
```

## Strict mode

By default, LiveFolders logs a warning and continues if sandboxing is unavailable. Set `mode: strict` in `livefolders.yaml` to refuse to mount instead:

```yaml
sandbox:
  mode: strict   # abort mount if Landlock/sandbox-exec cannot be applied
```

## Session-scoped invocation state

`write_invoke` results are routed back to the caller using the kernel session id (`getsid(pid)`). This is what makes `echo X > ep && cat ep` work correctly when two shells run it in parallel: each shell has its own sid, and each sid gets its own slot.

This is **isolation for correctness, not for security**. A local user who can call `setsid` can manipulate their own sid arbitrarily, and the runtime treats all sids equally — there is no per-user or per-shell access control on top. If two unrelated processes share a sid (e.g. one process forks another without `setsid`), they share the same invocation slot.

The mount is created with `MountOption::AllowOther`, which means any local user on the host can read endpoint outputs. Do not mount LiveFolders on a shared host if endpoint outputs may contain secrets returned by handlers.

## Known limitations

- Secrets in `~/.config/livefolders/secrets.env` are loaded into the daemon's process environment at startup and inherited by every handler subprocess, regardless of whether the tool declared a need for them.
- The installer downloads `folder.yaml` and companion files over plain HTTPS with no checksum or signature pinning. Audit `folder.yaml` before installing tools from sources you don't control.
- The MCP proxy Unix socket is created with the process's default umask. On a host with permissive umask, the socket may be world-accessible.

These are tracked in `IMPROVEMENTS.md` (sections 1.1–1.10).
