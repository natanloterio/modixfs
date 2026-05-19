# Experiment Results — 2026-05-19

=== criteria/01-setup-complexity/ ===
[01-setup-complexity]
  LiveFoldersFS: 6 lines (folder.yaml only, no Python)
  MCP (Python):  8 lines (server.py)
  Winner: LiveFoldersFS

=== criteria/02-llm-compatibility/ ===
[02-llm-compatibility]
  LiveFoldersFS: works on any host with bash/shell tool access
  MCP:           works on any host that implements MCP client protocol
  Winner: MCP for cross-host portability (MCP-native hosts); LiveFoldersFS for shell-capable agents.

=== criteria/03-discoverability/ ===
[03-discoverability]

--- LiveFoldersFS: auto-generated how_to.md (includes input type + constraints) ---
# shout

Echoes input in uppercase.

## Files

- **shout** (`write_invoke`) — handler: `tr '[:lower:]' '[:upper:]'`, input: plain text, min_length: 1, max_length: 500

--- MCP: LLM receives list_tools JSON response ---
{
  "tools": [
    {
      "name": "shout",
      "description": "Echoes input in uppercase.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "text": {"type": "string"}
        },
        "required": ["text"]
      }
    }
  ]
}

  LiveFoldersFS: human-readable markdown, LLM reads it naturally;
    input type, min/max length, pattern, and JSON schema now surfaced inline.
  MCP: structured JSON schema, protocol-enforced parameter validation.
  Winner: MCP retains schema-strictness edge; LiveFoldersFS now surfaces types + constraints.

=== criteria/04-io-expressiveness/ ===
[04-io-expressiveness]

Plain text:  both handle it
JSON:        LiveFoldersFS now enforces structural schema (required fields, property types)
             before handler runs; MCP enforces typed schema at protocol layer.
Multiline:   LiveFoldersFS native (stdin); MCP requires escaped string parameter
Binary:      LiveFoldersFS: pipe binary to handler; MCP: base64 encode in string (workaround)
Constraints: LiveFoldersFS: min/max length, regex pattern for strings; MCP: type/required only

  Example: json_reflect with schema {required:[text], properties:{text:{type:string}}}
    Valid input   {"text":"hello"} → passes, handler runs
    Missing field {}               → [ERROR:INVALID_INPUT] missing required field: 'text'
    Wrong type    {"text":42}      → [ERROR:INVALID_INPUT] field 'text' expected type 'string'

  Winner: MCP for protocol-enforced schema; LiveFoldersFS now competitive with
    opt-in structural validation (required fields, property types, string constraints).

=== criteria/05-security/ ===
[05-security]
  Both: run as user process, no OS sandboxing

  LiveFoldersFS (v0.7.0):
    - Opt-in per-endpoint structural validation via folder.yaml input.schema
    - Supports: required fields, property type checks, string min/max/pattern
    - Malformed input rejected before handler runs → no shell code executed
    - Remaining gap: opt-in (author must declare schema), not protocol-enforced
    - Shell injection within handler body is still the author's responsibility

  MCP:
    - Schema validation enforced unconditionally at protocol layer
    - Every tool rejects mistyped inputs automatically
    - No equivalent of string pattern or length constraints without custom validation

  Example (LiveFoldersFS search endpoint, schema: required=[query], query:string):
    Valid:   echo '{"query":"cats"}' → handler runs, returns result
    Missing: echo '{}'               → [ERROR:INVALID_INPUT] missing required field: 'query'
    Wrong:   echo '{"query":42}'     → [ERROR:INVALID_INPUT] field 'query' expected type 'string'

  Winner: MCP for unconditional protocol-layer enforcement;
    LiveFoldersFS ~ (partial, improved) — structural validation now available opt-in.

=== criteria/06-stateful-tools/ ===
[06-stateful-tools]

LiveFoldersFS v0.8.0: state_file field in folder.yaml
  - declare state_file: counter.db on any endpoint
  - runtime creates file if absent, holds flock(LOCK_EX) for the entire handler call
  - LIVEFOLDERS_STATE_FILE env var injected with the resolved path
  - concurrent invocations serialised automatically; no handler-side locking needed
  - state persists across restarts (file, not memory)

  Counter after 2 sequential invocations: 2 (expected: 2)

MCP: state in Python process memory — fast, no locking needed for single-threaded
  Limitation: state lost on server restart; persistent state requires a file/DB

  Winner: LiveFoldersFS — durable file-based state with automatic exclusive locking;
          MCP in-process state is faster but ephemeral

=== criteria/07-composability/ ===
[07-composability]

LiveFoldersFS v0.9.0: pipe: field in folder.yaml
  - declare pipe: [stage1, stage2, ...] on any write_invoke endpoint
  - runtime chains handlers: stdout of each stage → stdin of next
  - single LLM write triggers the entire pipeline; no intermediate reads needed
  - per-stage input: schema validated before each stage executes
  - any stage error stops the pipeline and returns [ERROR:CODE] immediately

  Example folder.yaml:
    files:
      - name: fetch
        type: write_invoke
        handler: ./fetch.sh
      - name: format
        type: write_invoke
        handler: ./format.sh
      - name: report
        type: write_invoke
        pipe: [fetch, format]

  Pipeline demo (echo → uppercase → prefix):
    input:  london
    stage1: LONDON
    output: City: LONDON

MCP: compose via Python function calls — clean within a single server
  Cross-server tool chaining not natively supported; requires LLM orchestration

  Winner: LiveFoldersFS — native declarative pipeline; single write, zero intermediate reads
          MCP composition requires LLM round-trips across server boundaries

=== criteria/08-observability/ ===
[08-observability]

LiveFoldersFS v0.8.0: two observability layers
  1. Structured error codes in the response stream:
     [ERROR:HANDLER]       — handler exited non-zero
     [ERROR:TIMEOUT]       — handler exceeded timeout
     [ERROR:SPAWN]         — handler failed to start
     [ERROR:PROCESS]       — unexpected process I/O error
     [ERROR:INVALID_INPUT] — input failed schema validation

  2. Per-endpoint <endpoint>.log file written after every invocation:
     cat forecast.log
     duration_ms: 342
     exit: ok
     stderr: 

     cat forecast.log  (with stderr)
     duration_ms: 88
     exit: ok
     stderr: warning: rate limit approaching

  LLM or monitoring script can read <endpoint>.log without round-tripping through the tool.

MCP: Python exceptions auto-converted to structured error objects (type, message, traceback)
  Strong for programmatic error handling; no timing or stderr capture by default

AgentFS: purpose-built observability substrate — structured audit logs and execution
  traces as first-class filesystem artifacts (but not a tool-invocation interface)

  Winner: LiveFoldersFS ✓ — structured error codes + per-invocation timing/stderr logs
          MCP ✓ — structured exceptions; AgentFS ✓ — audit substrate
          llm9p ✗ — no observability mechanism

=== criteria/09-hot-reload/ ===
[09-hot-reload]

LiveFoldersFS:
  Edit folder.yaml or handler script → inotify watcher detects change → immediate
  No restart required. New file reads reflect updated handler within ~1s.

MCP:
  Edit server.py → must restart MCP server process → Claude Code must reconnect
  Restart latency: ~1-3s for Python startup + reconnect handshake

  Winner: LiveFoldersFS — true hot-reload via filesystem watcher

=== criteria/10-publishing/ ===
[10-publishing]

LiveFoldersFS:
  1. Add folder.yaml to any GitHub repo
  2. Done. Users install with: livefolders install github.com/you/repo
  No registry. No npm publish. No PyPI upload.

MCP:
  Option A: publish to npm/PyPI, users add to claude_desktop_config.json manually
  Option B: list in community MCP registry (no official registry yet)
  Option C: share repo URL, users clone and configure themselves

  Winner: LiveFoldersFS — one-command install from any GitHub URL

=== criteria/worked-example/ ===
[worked-example (users REST API)]
  LiveFoldersFS: 10 lines (folder.yaml)
  MCP (Python):  18 lines (server.py)

  LiveFoldersFS install: livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users
  MCP install: pip install mcp httpx && configure server in claude_desktop_config.json

  Winner: LiveFoldersFS — zero-dependency install vs multi-step MCP setup

