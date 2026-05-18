# LiveFolders

A virtual filesystem that exposes tools to LLMs as plain files. Instead of JSON-RPC and protocol overhead, LLMs use `cat`, `echo`, and pipes — an interface they already know.

```
cat /tools/github/how_to.md
echo "language:rust fuse stars:>100" > /tools/github/search_repos
cat /tools/github/search_repos
```

---

## Getting started

### 1. Install

```bash
curl -fsSL https://raw.githubusercontent.com/natanloterio/LiveFolders/master/install.sh | bash
```

Detects your OS and architecture, downloads the right binary, and installs it to `/usr/local/bin`. Warns if FUSE is missing.

**Prerequisites**

- Linux: `sudo apt-get install fuse3` (or `dnf install fuse3`)
- macOS: install [macFUSE](https://osxfuse.github.io) first

**Manual install**

```bash
# Linux x86_64
curl -L https://github.com/natanloterio/LiveFolders/releases/latest/download/livefolders-linux-x86_64 -o livefolders
# Linux ARM64
curl -L https://github.com/natanloterio/LiveFolders/releases/latest/download/livefolders-linux-aarch64 -o livefolders
# macOS Apple Silicon
curl -L https://github.com/natanloterio/LiveFolders/releases/latest/download/livefolders-macos-aarch64 -o livefolders
# macOS Intel
curl -L https://github.com/natanloterio/LiveFolders/releases/latest/download/livefolders-macos-x86_64 -o livefolders

chmod +x livefolders && sudo mv livefolders /usr/local/bin/
```

**From source**

```bash
sudo apt-get install libfuse3-dev pkg-config  # Linux only
cargo install --git https://github.com/natanloterio/LiveFolders
```

### 2. Init

```bash
livefolders init
```

Creates `tools.yaml` in the current directory:

```yaml
mount: /tmp/livefolders

tools:
  - name: echo
  - name: github
    token_env: GITHUB_TOKEN
```

### 3. Install a tool

```bash
livefolders install github.com/someone/their-tool
```

This downloads the tool, reads its `livefolders.yaml`, prompts for any required secrets, and stores them in `~/.config/livefolders/secrets.env`.

### 4. Mount

```bash
livefolders mount
```

Secrets from `~/.config/livefolders/secrets.env` are loaded automatically. The filesystem is live at the path set in `tools.yaml`.

### 5. Use it

```bash
ls /tmp/livefolders/tools/          # see all tools
cat /tmp/livefolders/tools/index.md # read tool descriptions

cat /tmp/livefolders/tools/github/how_to.md
echo "tokio stars:>1000" > /tmp/livefolders/tools/github/search_repos  # blocks until done
cat /tmp/livefolders/tools/github/search_repos
```

Point your LLM agent at the mount path. It can discover, read, and invoke tools with standard file operations.

---

## Built-in tools

### `echo`

Reflects input back as output. Useful for smoke-testing the filesystem.

```bash
echo "hello" > /tmp/livefolders/tools/echo/send
cat /tmp/livefolders/tools/echo/send   # → hello
```

### `github`

Searches GitHub using the [Search API](https://docs.github.com/en/search-github). Requires `GITHUB_TOKEN`.

| Endpoint | What to write |
|---|---|
| `search_repos` | GitHub search query (e.g. `language:rust stars:>100`) |
| `search_code` | Code search query (e.g. `async fn main repo:tokio-rs/tokio`) |

---

## Advanced

### How it works

Every tool appears as a directory under `/tools/<name>/`. Writing to an endpoint file invokes the tool; reading retrieves the result. The result is cleared after reading.

```
/tools/
├── index.md              ← lists all tools and descriptions
├── github/
│   ├── how_to.md         ← LLM reads this to understand the tool
│   ├── search_repos      ← write query → read results
│   └── search_code
└── echo/
    ├── how_to.md
    └── send
```

State machine per endpoint:

```
IDLE → write(input) → invoke() [blocks] → COMPLETE → read() → IDLE
```

The write call blocks the caller until the tool finishes — result is always ready by the time `cat` runs.

**vs MCP**

| | MCP | LiveFolders |
|---|---|---|
| Protocol | JSON-RPC | File I/O |
| Discovery | Tool list API | `ls` / `cat` |
| Documentation | Schema | Free-form Markdown |
| Invocation | Function call | File write |
| Result | JSON response | File read |
| Composition | Limited | Shell pipes |

---

### External tools

Build a LiveFolders tool without writing Rust — any language, any script.

**Directory layout**

```
~/.config/livefolders/tools/
└── mytool/
    ├── how_to.md     ← LLM reads this (read-only, served from disk)
    ├── search        ← executable: stdin = what LLM wrote, stdout = result
    ├── output.csv    ← regular file: passthrough read/write to disk
    └── config.json   ← regular file: LLM can write config directly
```

**Declaring file behavior in `livefolders.yaml`**

Add a `files` section to declare how each virtual file behaves:

```yaml
files:
  - name: forecast
    type: read_invoke
    handler: ./bin/forecast         # read triggers handler; write optionally sets params

  - name: search
    type: write_invoke
    handler: "curl -s -X POST -d @- https://api.example.com/search"

  - name: config.json
    type: passthrough               # reads and writes go directly to disk; no handler

  - name: how_to.md
    type: readonly                  # served from disk; writes return error; no handler
```

| Type | Write | Read |
|---|---|---|
| `write_invoke` | Invokes handler (blocks), stores result | Returns last result |
| `read_invoke` | Stores params (non-blocking) | Invokes handler with stored params (blocks) |
| `passthrough` | Writes to disk | Reads from disk |
| `readonly` | Returns error | Reads from disk |

The `handler` is any shell command. LiveFolders passes input via stdin and reads output from stdout:

```bash
handler: ./bin/forecast                                              # local script
handler: python3 ./scripts/search.py                                # interpreter (no chmod +x needed)
handler: "curl -s -X POST -d @- https://api.example.com/search"     # HTTP via curl
```

Without a `files` section, LiveFolders falls back to the current heuristic: executable files (`chmod +x`) behave as `write_invoke`, regular files behave as `passthrough`.

**Script environment**

Every executable receives:

- `stdin` — the bytes the LLM wrote
- `LIVEFOLDERS_TOOL` — tool directory name
- `LIVEFOLDERS_ENDPOINT` — endpoint filename
- All env vars present when `livefolders` was launched (including secrets)

**Example script**

```bash
#!/bin/bash
# ~/.config/livefolders/tools/weather/forecast
curl -s "https://wttr.in/$(cat -)?format=3"
```

```bash
chmod +x ~/.config/livefolders/tools/weather/forecast
# tool appears immediately — no restart needed
```

**Enable in tools.yaml**

```yaml
tools_dir: ~/.config/livefolders/tools
timeout: 30   # seconds before a subprocess is killed
```

**Hot-reload**

LiveFolders watches `tools_dir` with inotify (Linux) / kqueue (macOS). Adding or removing a tool directory is picked up immediately.

---

### Secrets management

`livefolders install` stores secrets in `~/.config/livefolders/secrets.env` (mode `0600`). You can also edit it manually:

```
# ~/.config/livefolders/secrets.env
GITHUB_TOKEN=ghp_...
MYTOOL_API_KEY=sk-...
```

`livefolders mount` loads this file at startup. Shell environment always takes precedence — existing env vars are never overwritten.

---

### Publishing a tool

Add `livefolders.yaml` to your tool directory to make it installable with a single command:

```yaml
name: mytool
description: One-line description shown during install
version: 0.1.0
env:
  - name: MYTOOL_API_KEY
    description: API key from https://example.com/settings
    required: true
  - name: MYTOOL_TIMEOUT
    description: Request timeout in seconds
    required: false
    default: "30"
```

`required: true` vars trigger an interactive prompt at install time. Tools without `livefolders.yaml` install fine — just without prompts.

Push to a public GitHub repo and share:

```bash
livefolders install github.com/you/your-tool

# For a tool inside a subdirectory
livefolders install github.com/owner/repo/tree/main/mytool
```

---

### Adding a built-in tool (Rust)

Implement the `Tool` trait:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn how_to(&self) -> &str;            // rendered at /tools/<name>/how_to.md
    fn endpoints(&self) -> Vec<&str>;    // files under /tools/<name>/
    async fn invoke(&self, endpoint: &str, input: &[u8], session: &Session) -> ToolResult;
}
```

Register it in `main.rs`:

```rust
registry.register(Arc::new(MyTool::new()));
```

---

### Architecture

```
LLM Agent
    │  read / write syscalls
LiveFolders (FUSE)
    ├── Virtual File Router     path → inode mapping
    ├── Tool Registry           Tool trait + hot-reload watcher
    └── Secrets Loader          ~/.config/livefolders/secrets.env → process env
            │
    Tool Implementations
    (async HTTP, subprocess, passthrough files)
```

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
