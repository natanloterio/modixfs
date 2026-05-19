# LiveFolders

Expose any tool to an LLM as plain files. The LLM uses `cat` and `echo` — no JSON, no protocol, no SDK.

```bash
cat .livefolders/tools/users/list
# → # Users
# →
# → ## Mr. Rudolph Robel-Fay
# → ID: 1
# → ...
```

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/natanloterio/LiveFolders/master/install.sh | bash
```

**Prerequisites**

- Linux: `sudo apt-get install fuse3`
- macOS: install [macFUSE](https://osxfuse.github.io)

<details>
<summary>Manual install / from source</summary>

```bash
# Linux x86_64
curl -L https://github.com/natanloterio/LiveFolders/releases/latest/download/livefolders-linux-x86_64 -o livefolders
chmod +x livefolders && sudo mv livefolders /usr/local/bin/

# From source
cargo install --git https://github.com/natanloterio/LiveFolders
```
</details>

---

## Quick start

```bash
# 1. Create a config file
livefolders init

# 2. Install a tool from GitHub
livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users

# 3. Mount (runs in background, returns to prompt immediately)
livefolders mount

# 4. Use it
## 4.1 Make a request from API
cat .livefolders/tools/users/list      # fetches users from the API

## 4.2 Call a simple local program
echo "hello world" > .livefolders/tools/demo/shout
cat .livefolders/tools/demo/shout      # → HELLO WORLD

# 5. Stop
livefolders stop
```


## Giving tools to Claude Code

Add this to your project's `CLAUDE.md`:

```markdown
## Tools

LiveFolders is mounted at `.livefolders/tools/`. Before using any tool:
1. `cat .livefolders/tools/index.md` to see what's available
2. `cat .livefolders/tools/<name>/how_to.md` to read usage instructions
3. Write input with `echo "..." > .livefolders/tools/<name>/<endpoint>`
4. Read output with `cat .livefolders/tools/<name>/<endpoint>`
```

---

## How it works

Every tool is a directory under `/tools/<name>/`. Each file inside is an **endpoint**:

- **Write** to an endpoint → sends input to the tool
- **Read** from an endpoint → gets the result

```
.livefolders/tools/
├── index.md            ← all tools and descriptions
├── demo/
│   ├── how_to.md       ← LLM reads this to understand the tool
│   ├── schema.json     ← machine-readable endpoint schemas (auto-generated)
│   ├── shout           ← write text, read it back uppercased
│   ├── shout.log       ← last invocation: duration_ms + stderr
│   └── status          ← read to get current status
└── users/
    ├── how_to.md
    ├── schema.json
    └── list            ← read to fetch users from the API
```

The write call **blocks until the tool finishes** — by the time `cat` runs, the result is ready.

---

## Using with an LLM

Once mounted, give the agent a single instruction:

> *"Tools are mounted at `.livefolders/tools/`. Read `index.md` to discover what's available, then read `how_to.md` inside any tool directory before using it."*

The agent then follows this natural sequence on its own:

**1. Discover available tools**
```
cat .livefolders/tools/index.md
# → weather  — Get the weather forecast for any city.
# → users    — Fetch users from the REST API.
```

**2. Read the instructions for a tool**
```
cat .livefolders/tools/weather/how_to.md
# → # weather
# →
# → Get the weather forecast for any city.
# →
# → ## Files
# →
# → - **forecast** (`write_invoke`) — handler: ..., input: plain text, min_length: 1
# →   → read `forecast.log` for last invocation timing and stderr
```

**3. Invoke the tool**
```
echo "London" > .livefolders/tools/weather/forecast
cat .livefolders/tools/weather/forecast
# → Weather report for London, United Kingdom:
# →    \☁️   Overcast
# →   15 °C
```

**4. Handle errors**

If input is invalid, the endpoint returns a structured error — no handler runs:
```
echo "" > .livefolders/tools/weather/forecast
cat .livefolders/tools/weather/forecast
# → [ERROR:INVALID_INPUT] input too short: minimum 1 characters required
```

If the handler fails, the error includes the exit reason:
```
# → [ERROR:HANDLER] curl: (6) Could not resolve host: wttr.in
```

**5. Check timing and diagnostics (optional)**
```
cat .livefolders/tools/weather/forecast.log
# → duration_ms: 342
# → exit: ok
# → stderr:
```

---

## Mount and stop

```bash
livefolders mount               # mount in background (default)
livefolders mount --foreground  # stay in foreground (useful for debugging)
livefolders stop                # stop the background daemon
```

Logs go to `~/.local/share/livefolders/livefolders.log`.

If something looks wrong, run `livefolders doctor` — it checks FUSE, your config, and every installed tool's `folder.yaml` and prints actionable fixes.

Handler timeouts are enforced in both foreground and daemon modes. A handler that hangs is killed after `timeout` seconds (default 30), and the endpoint returns an error string — the filesystem never freezes.

---

## Installing tools

```bash
livefolders install github.com/owner/repo

# Tool inside a subdirectory
livefolders install github.com/owner/repo/tree/main/mytool
```

If the tool declares required secrets in its `folder.yaml`, you'll be prompted for them on install. Secrets are stored in `~/.config/livefolders/secrets.env` and loaded automatically on every mount.

---

## Example tools

Ready-to-install tools in this repo:

| Tool | Install | What it does |
|------|---------|--------------|
| `demo` | `livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/demo` | Demonstrates all file types and input schema validation |
| `users` | `livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users` | Fetches users from a REST API via GET |

---

## Building a tool

Any directory with a `folder.yaml` is a LiveFolders tool. No Rust required.

**Directory layout**

```
~/.config/livefolders/tools/
└── weather/
    ├── folder.yaml
    └── forecast        ← optional script (if not using inline handler)
```

> `how_to.md` and `schema.json` are auto-generated from `folder.yaml` — no need to include them.

**`folder.yaml`**

```yaml
name: weather
description: Get the weather forecast for any city.

files:
  - name: forecast
    type: write_invoke
    handler: "curl -s \"https://wttr.in/$(cat -)?format=3\""
```

**File types**

| Type | Write | Read |
|---|---|---|
| `write_invoke` | Runs `handler` with input on stdin, stores output | Returns stored output |
| `read_invoke` | No-op | Runs `handler`, returns output |
| `passthrough` | Writes to disk | Reads from disk |
| `readonly` | Returns error | Reads from disk |

---

**Input validation**

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

**Stateful tools**

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

**Pipelines**

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

**Observability**

After every invocation, a companion `<endpoint>.log` file is written alongside the endpoint:

```bash
cat .livefolders/tools/weather/forecast.log
# duration_ms: 342
# --- stderr ---
# (empty)
```

This lets the LLM or a monitoring script check execution timing and stderr without a separate round-trip.

The `schema.json` file in each tool directory mirrors MCP's `list_tools` format, making the tool surface parseable by MCP-aware clients and scripts:

```bash
cat .livefolders/tools/weather/schema.json
# {
#   "name": "weather",
#   "description": "Get the weather forecast for any city.",
#   "endpoints": [{ "name": "forecast", "kind": "write_invoke" }]
# }
```

---

**Error format**

All handler errors are returned as `[ERROR:CODE] message` so LLMs and scripts can parse them reliably:

| Code | When |
|---|---|
| `INVALID_INPUT` | Input failed schema validation |
| `HANDLER` | Handler exited with non-zero status |
| `TIMEOUT` | Handler exceeded the configured timeout |
| `SPAWN` | Handler process failed to start |
| `PROCESS` | Unexpected process I/O error |

---

**Handlers**

The `handler` is any shell command. Input comes via stdin, output via stdout:

```yaml
handler: ./bin/my-script           # local script
handler: python3 ./scripts/run.py  # any interpreter
handler: "curl -s -d @- https://api.example.com/search"  # HTTP request
```

**Script environment**

Every handler receives:

- `stdin` — bytes the LLM wrote to the endpoint
- `LIVEFOLDERS_TOOL` — tool name
- `LIVEFOLDERS_ENDPOINT` — endpoint filename
- `LIVEFOLDERS_STATE_FILE` — resolved state file path (only when `state_file` is declared)
- All env vars present at mount time (including secrets)

**Hot-reload**

LiveFolders watches `tools_dir` for changes. Adding or editing a tool takes effect immediately — no restart needed.

---

## Publishing a tool

Add `folder.yaml` to your repo and push. Anyone can install it with one command:

```bash
livefolders install github.com/you/your-tool
```

Declare required secrets so users are prompted at install time:

```yaml
name: mytool
description: One-line description shown during install

env:
  - name: MYTOOL_API_KEY
    description: API key from https://example.com/settings
    required: true
```

---

## `livefolders.yaml` reference

```yaml
mount: .livefolders                     # where to mount (set by `livefolders init`)
tools_dir: ~/.config/livefolders/tools  # where installed tools live
timeout: 30                             # seconds before a handler is killed

tools:
  - name: echo                          # built-in smoke-test tool
```

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
