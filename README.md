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
cat .livefolders/tools/users/list      # fetches users from the API
echo "hello world" > .livefolders/tools/demo/shout
cat .livefolders/tools/demo/shout      # → HELLO WORLD

# 5. Stop
livefolders stop
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
│   ├── shout           ← write text, read it back uppercased
│   └── status          ← read to get current status
└── users/
    ├── how_to.md
    └── list            ← read to fetch users from the API
```

The write call **blocks until the tool finishes** — by the time `cat` runs, the result is ready.

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
    ├── how_to.md
    └── forecast        ← optional script (if not using handler in yaml)
```

**`folder.yaml`**

```yaml
name: weather
description: Get the weather forecast for any city.

files:
  - name: forecast
    type: write_invoke
    handler: "curl -s \"https://wttr.in/$(cat -)?format=3\""
```

> `how_to.md` is auto-generated from `folder.yaml` if not present on disk — no need to include it.

**File types**

| Type | Write | Read |
|---|---|---|
| `write_invoke` | Runs `handler` with input on stdin, stores output | Returns stored output |
| `read_invoke` | No-op | Runs `handler`, returns output |
| `passthrough` | Writes to disk | Reads from disk |
| `readonly` | Returns error | Reads from disk |

**Input validation**

Add an `input:` field to any endpoint to validate what the LLM writes before the handler runs:

```yaml
files:
  - name: search
    type: write_invoke
    handler: "./search.sh"
    input:
      type: json       # "json" | "string" | "none"
```

| Value | Behaviour |
|---|---|
| `json` | Rejects input that is not valid UTF-8 JSON |
| `string` | Accepts any bytes (explicit no-op, same as omitting the field) |
| `none` | Rejects any non-empty input |
| *(absent)* | No validation — current behaviour preserved |

On rejection the endpoint returns `[ERROR:INVALID_INPUT] reason` instead of invoking the handler. The generated `how_to.md` documents the declared input type automatically.

**Error format**

All handler errors are returned as `[ERROR:CODE] message` so LLMs and scripts can parse them reliably:

| Code | When |
|---|---|
| `INVALID_INPUT` | Input failed schema validation |
| `HANDLER` | Handler exited with non-zero status |
| `TIMEOUT` | Handler exceeded the configured timeout |
| `SPAWN` | Handler process failed to start |
| `PROCESS` | Unexpected process I/O error |

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
