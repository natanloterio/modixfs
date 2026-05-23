# LiveFoldersFS vs MCP: A Comparison Framework for Agentic Tool Integration

## Introduction

LLMs increasingly need access to external tools — APIs, scripts, databases, and system commands. Two distinct design philosophies have emerged for providing this access. The Model Context Protocol (MCP) takes a typed, structured, protocol-based approach: tools are defined in code, exposed over JSON-RPC, and consumed through SDK-aware host clients. LiveFoldersFS takes a filesystem-native approach: tools are YAML-defined files, mounted as a virtual filesystem, and consumed by any agent that can read files or run shell commands.

This document compares both approaches across ten criteria using concrete experimental evidence. The primary host for all experiments is Claude Code. All MCP examples use Python (FastMCP). The goal is not to declare a winner but to provide a framework for making context-appropriate choices. Both approaches are valid; the right choice depends on your deployment target, team skill set, and tool characteristics.

---

## Criteria Matrix

| # | Criterion | LiveFoldersFS | MCP | Winner |
|---|-----------|--------------|-----|--------|
| 01 | Setup complexity | 6-line YAML, no runtime | 8-line Python, needs `mcp` package | LiveFoldersFS |
| 02 | LLM compatibility | Shell-capable agents | MCP-native host clients | Context-dependent |
| 03 | Discoverability | Markdown index, human-readable | JSON schema, machine-validated | Tie |
| 04 | I/O expressiveness | Raw / binary / streaming | Typed / structured | Context-dependent |
| 05 | Security | Shell injection risk without care | Schema reduces injection surface | MCP (marginal) |
| 06 | Stateful tools | File-persisted state | In-process memory state | Context-dependent |
| 07 | Composability | Unix pipelines | Python function calls | Context-dependent |
| 08 | Observability | Plain-text errors to LLM | Structured error objects | MCP (marginal) |
| 09 | Hot-reload | inotify watcher, ~1s, no restart | Server restart + reconnect (~1-3s) | LiveFoldersFS |
| 10 | Publishing | One-command GitHub install | Manual registry / config steps | LiveFoldersFS |

---

### 01 — Setup Complexity

LiveFoldersFS requires only a `folder.yaml` file and no installed runtime beyond `livefolders` itself. The worked example is 10 lines of YAML (6 non-blank). The MCP equivalent is 22 lines of Python (18 non-blank), requires the `mcp` and `httpx` packages, and must be registered in the host's configuration. For tool authors who want the lowest possible barrier to entry, LiveFoldersFS has a clear advantage.

### 02 — LLM Compatibility

LiveFoldersFS works with any agent that has filesystem or shell access — Claude Code, custom shell wrappers, or any POSIX-compatible environment. It does not work in hosted web clients (Claude.ai) or MCP-native hosts that do not expose a shell. MCP works in any host that implements the MCP client protocol, including current and future adopters of the standard. The winning choice depends entirely on your target deployment environment.

### 03 — Discoverability

LiveFoldersFS generates an `index.md` file that the LLM reads as plain text. The format is natural markdown: tool names, descriptions, and available files. MCP exposes a `list_tools` JSON response with a formal input schema (`type`, `properties`, `required`). MCP's schema enables the host to validate parameters before the tool is even called; LiveFoldersFS's markdown is easier to read at a glance without tooling. The approaches trade off schema strictness against human readability — neither is clearly superior.

### 04 — I/O Expressiveness

Both approaches handle plain text without difficulty. For JSON, MCP enforces a typed schema at the boundary; LiveFoldersFS passes raw strings that the handler must parse. For multiline input, LiveFoldersFS accepts it natively through stdin; MCP requires the caller to pass an escaped string parameter. For binary data, LiveFoldersFS can pipe binary directly to a handler; MCP requires base64 encoding inside a string field. MCP wins for structured, validated I/O; LiveFoldersFS wins for raw, binary, or streaming workloads.

### 05 — Security

Both approaches run as a user process with no OS-level sandboxing. The key difference is the input path: in LiveFoldersFS, the LLM constructs shell invocations, so an unsanitized handler can be vulnerable to shell injection if the tool passes user-controlled strings directly to a shell command. MCP's schema validation layer enforces type constraints before execution, reducing but not eliminating injection risk. The MCP advantage is marginal — both models require careful handler design — but the schema provides a structural checkpoint that LiveFoldersFS lacks by default.

### 06 — Stateful Tools

LiveFoldersFS state lives in files (e.g., `/tmp/lf_counter`). This state persists across restarts. Per-invocation buffers are scoped to the caller's shell session, so concurrent `echo + cat` from different shells do not clobber each other; for cross-invocation state shared across sessions, declare a `state_file` in `folder.yaml` and the runtime will hold an exclusive `flock` for the duration of each handler call. MCP state lives in Python process memory: fast and lock-free for single-threaded servers, but lost on server restart. Tools that need persistence across restarts benefit from LiveFoldersFS's file-based model; tools with short-lived, in-memory state are more naturally expressed in MCP.

### 07 — Composability

LiveFoldersFS handlers compose through standard Unix pipelines: any command-line tool is trivially composable using `|`. Cross-tool calls within LiveFoldersFS require writing to another endpoint file, which is awkward for tight chaining. MCP handlers compose through Python function calls: typed, testable, and readable. Cross-server MCP calls are not natively supported and require LLM orchestration. LiveFoldersFS wins for Unix-pipeline composition; MCP wins for within-server function composition.

### 08 — Observability

LiveFoldersFS routes stderr to a log file and returns errors to the LLM as plain text strings. The LLM receives the error message but has no structured metadata about error type or code. MCP converts Python exceptions into structured error objects with error codes and stack context, which are easier to handle programmatically by the host or a monitoring layer. The MCP advantage here is marginal for most use cases but meaningful for production tooling that needs error-type routing.

### 09 — Hot-Reload

LiveFoldersFS uses an inotify filesystem watcher. Editing `folder.yaml` or a handler script takes effect within approximately one second — no server restart and no reconnect handshake required. MCP requires a full server process restart followed by a reconnect handshake from the host; measured latency is approximately 1–3 seconds for Python startup plus reconnect. For iterative development, LiveFoldersFS's hot-reload is a concrete productivity advantage.

### 10 — Publishing

Publishing a LiveFoldersFS tool requires adding a `folder.yaml` to any Git repository. Users install it with a single command: `livefolders install github.com/you/repo`. No registry, no `npm publish`, no `pip install` configuration. MCP has no official registry; publishing options are to upload a package to npm or PyPI, list in a community registry, or share a repository URL for manual setup. For individual tool authors or small teams, LiveFoldersFS's GitHub-native publishing model has substantially lower friction.

---

## Worked Example: Users REST API

Both implementations fetch a list of users from a REST API and return formatted markdown. They use different upstream APIs (LiveFoldersFS uses an existing pre-configured mockapi.io endpoint; MCP uses jsonplaceholder.typicode.com), but the output structure is equivalent.

### LiveFoldersFS — `folder.yaml` (10 lines, 6 non-blank)

```yaml
name: users
description: List users from the mock API.

files:
  - name: list
    type: read_invoke
    handler: >-
      curl -s https://6a0b5d085aa893e1015a2d32.mockapi.io/users
      | jq -r '"# Users\n", (.[] | "## \(.name)\nID: \(.id)\nCreated: \(.createdAt)\nAvatar: \(.avatar)\n")'

  - name: how_to.md
    type: readonly
```

### MCP — `server.py` (22 lines, 18 non-blank)

```python
import httpx
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("users")

@mcp.tool()
def list_users() -> str:
    """Fetches all users from the JSONPlaceholder API."""
    response = httpx.get("https://jsonplaceholder.typicode.com/users")
    response.raise_for_status()
    users = response.json()
    lines = ["# Users", ""]
    for u in users:
        lines.append(f"## {u['name']}")
        lines.append(f"ID: {u['id']}")
        lines.append(f"Email: {u['email']}")
        lines.append("")
    return "\n".join(lines)

if __name__ == "__main__":
    mcp.run()
```

### What Claude Sees

In the LiveFoldersFS case, Claude reads the `list` file using a standard `Read` tool call. The FUSE layer intercepts the read, executes the `curl | jq` pipeline, and returns the output as file content. Claude treats this identically to reading any other file.

In the MCP case, Claude calls the `list_users` tool through the host's tool-use interface. The call is dispatched over JSON-RPC, the Python function executes, and the return value is delivered as a tool result string.

The observable behavior is the same from Claude's perspective. The difference is architectural: LiveFoldersFS tools are filesystem objects that happen to execute on read; MCP tools are protocol-dispatched functions that happen to return strings.

---

## Decision Guide

### Use LiveFoldersFS when:

- You want zero-dependency tool publishing (one GitHub URL, no registry required)
- Your agent has shell or filesystem access (Claude Code, custom shell agents, POSIX environments)
- You need hot-reload during development (edit YAML, change takes effect in ~1s without restart)
- Your tool is a thin wrapper over a CLI command, shell script, or `curl` invocation
- You want tools as first-class filesystem objects (inspectable with any file viewer, pipeable with standard Unix tools)

### Use MCP when:

- You need cross-host portability (Claude.ai web client, future MCP-adopting hosts, non-shell environments)
- Your tool takes complex typed inputs (JSON schemas enforce structure and prevent malformed calls)
- You need structured error handling (error codes and typed exceptions, not plain-text error strings)
- Your tool maintains long-lived in-process state that should not be serialized to disk
- Your team prefers a Python or TypeScript development model with standard package tooling

---

## Running the Experiments Yourself

All experiments are in the `research/` directory of this repository. Each criterion has its own subdirectory under `research/criteria/` with the tool definitions and test scripts used to generate the results above.

To reproduce the full experiment suite:

```bash
bash research/run-all.sh
```

Results are written to `research/results/summary.md`. Individual criterion outputs are in `research/results/` alongside `summary.md`.

The worked example implementations are in `research/criteria/worked-example/livefolders/` (LiveFoldersFS) and `research/criteria/worked-example/mcp/` (MCP).
