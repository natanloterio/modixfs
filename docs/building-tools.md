# Building tools

Any directory with a `folder.yaml` is a LiveFolders tool. No Rust required.

## Directory layout

```
~/.config/livefolders/tools/
└── weather/
    ├── folder.yaml
    └── forecast        ← optional script (if not using inline handler)
```

`how_to.md` and `schema.json` are auto-generated from `folder.yaml` — no need to include them.

## `folder.yaml`

```yaml
name: weather
description: Get the weather forecast for any city.

files:
  - name: forecast
    type: write_invoke
    handler: "curl -s \"https://wttr.in/$(cat -)?format=3\""
```

## File types

| Type | Write | Read |
|---|---|---|
| `write_invoke` | Runs `handler` with input on stdin, stores output | Returns stored output |
| `read_invoke` | No-op | Runs `handler`, returns output |
| `passthrough` | Writes to disk | Reads from disk |
| `readonly` | Returns error | Reads from disk |

---

## Input validation

Add an `input:` field to validate what the LLM writes before the handler runs:

```yaml
files:
  - name: search
    type: write_invoke
    handler: "./search.sh"
    input:
      type: json       # "json" | "string" | "none"
      schema:
        required: [query]
        properties:
          query: { type: string }
          limit: { type: number }
```

| Value | Behaviour |
|---|---|
| `json` | Rejects input that is not valid UTF-8 JSON |
| `string` | Accepts any bytes; supports `min_length`, `max_length`, `pattern` |
| `none` | Rejects any non-empty input |
| *(absent)* | No validation |

String constraints:

```yaml
input:
  type: string
  min_length: 1
  max_length: 500
  pattern: "^\\w+$"
```

JSON schema subset (`required`, `properties[*].type`) is enforced before the handler runs. Supported property types: `string`, `number`, `integer`, `boolean`, `array`, `object`, `null`.

On rejection the endpoint returns `[ERROR:INVALID_INPUT] reason` without invoking the handler. All declared constraints appear in the auto-generated `how_to.md` and `schema.json`.

---

## Stateful tools

Declare a `state_file` to persist data across invocations. The runtime holds an exclusive advisory lock (`flock LOCK_EX`) for the entire duration of each handler call, serialising concurrent invocations automatically:

```yaml
files:
  - name: counter
    type: write_invoke
    handler: "./counter.sh"
    state_file: counter.db
```

The resolved path is passed to the handler as `LIVEFOLDERS_STATE_FILE`. The file is created if it does not exist.

---

## Concurrency model

What you need to know when writing a handler:

- **Handlers run in parallel.** Each invocation is spawned onto a Tokio task. Two shells calling your endpoint at the same time will execute your handler concurrently, not in sequence. The FUSE thread is never blocked, so a slow handler does not hold up unrelated tools.
- **Per-session state isolation.** `write_invoke` results are scoped to the caller's shell session (`getsid(pid)`). `echo` and `cat` from the same shell pipeline share state and route correctly; two different shells writing to the same endpoint each get back their own result.
- **Shared external state is your responsibility.** If your handler reads or writes a file, hits a database, calls an API with a rate limit, or otherwise touches resources outside its own process, declare a `state_file` to serialise across invocations, or design the handler to be idempotent.
- **Timeouts hard-kill.** When the configured timeout (default 30s, set globally in `livefolders.yaml`) fires, the runtime sends SIGTERM and waits 1 second. If the handler is still alive it sends SIGKILL. Handlers that need to do cleanup on shutdown should trap SIGTERM and exit quickly; handlers that ignore SIGTERM will be killed unconditionally.
- **Stdin and stdout are streamed.** The runtime collects stdout in memory until the handler exits — keep responses bounded.

---

## Pipelines

Chain endpoints with `pipe:`. A single write invocation runs the stages in order, passing each stage's stdout as the next stage's stdin:

```yaml
files:
  - name: fetch_data
    type: write_invoke
    handler: "./fetch.sh"
  - name: format_report
    type: write_invoke
    handler: "./format.sh"
  - name: report          # ← pipe endpoint, no handler needed
    type: write_invoke
    pipe: [fetch_data, format_report]
```

```bash
echo "London" > .livefolders/tools/weather/report
cat .livefolders/tools/weather/report   # → formatted output from both stages
```

Per-stage `input:` schemas are validated before each stage executes. Any stage error stops the pipeline and returns a structured `[ERROR:CODE]` response immediately.

---

## Handlers

The `handler` is any shell command. Input comes via stdin, output via stdout:

```yaml
handler: ./bin/my-script           # local script
handler: python3 ./scripts/run.py  # any interpreter
handler: "curl -s -d @- https://api.example.com/search"  # HTTP request
```

Every handler receives:

- `stdin` — bytes the LLM wrote to the endpoint
- `LIVEFOLDERS_TOOL` — tool name
- `LIVEFOLDERS_ENDPOINT` — endpoint filename
- `LIVEFOLDERS_STATE_FILE` — resolved state file path (only when `state_file` is declared)
- All env vars present at mount time (including secrets)

---

## Secrets

Declare required secrets so users are prompted at `livefolders install` time:

```yaml
name: mytool
description: One-line description shown during install

env:
  - name: MYTOOL_API_KEY
    description: API key from https://example.com/settings
    required: true
```

Secrets are stored in `~/.config/livefolders/secrets.env` and injected into every handler at mount time.

---

## Hot-reload

LiveFolders watches `tools_dir` for changes. Adding or editing a tool takes effect immediately — no restart needed.

---

## Observability

After every invocation, a companion `<endpoint>.log` file is written alongside the endpoint:

```bash
cat .livefolders/tools/weather/forecast.log
# duration_ms: 342
# --- stderr ---
# (empty)
```

The `schema.json` file in each tool directory mirrors MCP's `list_tools` format:

```bash
cat .livefolders/tools/weather/schema.json
# {
#   "name": "weather",
#   "description": "Get the weather forecast for any city.",
#   "endpoints": [{ "name": "forecast", "kind": "write_invoke" }]
# }
```

---

## Error format

All handler errors are returned as `[ERROR:CODE] message`:

| Code | When |
|---|---|
| `INVALID_INPUT` | Input failed schema validation |
| `HANDLER` | Handler exited with non-zero status |
| `TIMEOUT` | Handler exceeded the configured timeout |
| `SPAWN` | Handler process failed to start |
| `PROCESS` | Unexpected process I/O error |

---

## `livefolders.yaml` reference

```yaml
mount: .livefolders                     # where to mount (set by `livefolders init`)
tools_dir: ~/.config/livefolders/tools  # where installed tools live
timeout: 30                             # seconds before a handler is killed

tools:
  - name: echo                          # built-in smoke-test tool
```
