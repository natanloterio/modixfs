# ModixFS

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
curl -fsSL https://raw.githubusercontent.com/natanloterio/modixfs/master/install.sh | bash
```

Detects your OS and architecture, downloads the right binary, and installs it to `/usr/local/bin`. Warns if FUSE is missing.

**Prerequisites**

- Linux: `sudo apt-get install fuse3` (or `dnf install fuse3`)
- macOS: install [macFUSE](https://osxfuse.github.io) first

**Manual install**

```bash
# Linux x86_64
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-linux-x86_64 -o modixfs
# Linux ARM64
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-linux-aarch64 -o modixfs
# macOS Apple Silicon
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-macos-aarch64 -o modixfs
# macOS Intel
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-macos-x86_64 -o modixfs

chmod +x modixfs && sudo mv modixfs /usr/local/bin/
```

**From source**

```bash
sudo apt-get install libfuse3-dev pkg-config  # Linux only
cargo install --git https://github.com/natanloterio/modixfs
```

### 2. Init

```bash
modixfs init
```

Creates `tools.yaml` in the current directory:

```yaml
mount: /tmp/modixfs

tools:
  - name: echo
  - name: github
    token_env: GITHUB_TOKEN
```

### 3. Install a tool

```bash
modixfs install github.com/someone/their-modixfs-tool
```

This downloads the tool, reads its `modix.yaml`, prompts for any required secrets, and stores them in `~/.config/modixfs/secrets.env`.

### 4. Mount

```bash
modixfs mount
```

Secrets from `~/.config/modixfs/secrets.env` are loaded automatically. The filesystem is live at the path set in `tools.yaml`.

### 5. Use it

```bash
ls /tmp/modixfs/tools/          # see all tools
cat /tmp/modixfs/tools/index.md # read tool descriptions

cat /tmp/modixfs/tools/github/how_to.md
echo "tokio stars:>1000" > /tmp/modixfs/tools/github/search_repos
sleep 2
cat /tmp/modixfs/tools/github/search_repos
```

Point your LLM agent at the mount path. It can discover, read, and invoke tools with standard file operations.

---

## Built-in tools

### `echo`

Reflects input back as output. Useful for smoke-testing the filesystem.

```bash
echo "hello" > /tmp/modixfs/tools/echo/send
cat /tmp/modixfs/tools/echo/send   # → hello
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
IDLE → write(input) → invoke() → COMPLETE → read() → IDLE
```

**vs MCP**

| | MCP | ModixFS |
|---|---|---|
| Protocol | JSON-RPC | File I/O |
| Discovery | Tool list API | `ls` / `cat` |
| Documentation | Schema | Free-form Markdown |
| Invocation | Function call | File write |
| Result | JSON response | File read |
| Composition | Limited | Shell pipes |

---

### External tools

Build a ModixFS tool without writing Rust — any language, any script.

**Directory layout**

```
~/.config/modixfs/tools/
└── mytool/
    ├── how_to.md     ← LLM reads this (read-only, served from disk)
    ├── search        ← executable: stdin = what LLM wrote, stdout = result
    ├── output.csv    ← regular file: passthrough read/write to disk
    └── config.json   ← regular file: LLM can write config directly
```

**File behavior**

| File type | Behavior |
|---|---|
| `how_to.md` | Served read-only from disk |
| Executable (`chmod +x`) | Write triggers invocation. Stdout becomes the next read result. |
| Regular file | Passthrough — reads and writes go directly to disk |

**Script environment**

Every executable receives:

- `stdin` — the bytes the LLM wrote
- `MODIXFS_TOOL` — tool directory name
- `MODIXFS_ENDPOINT` — endpoint filename
- All env vars present when `modixfs` was launched (including secrets)

**Example script**

```bash
#!/bin/bash
# ~/.config/modixfs/tools/weather/forecast
curl -s "https://wttr.in/$(cat -)?format=3"
```

```bash
chmod +x ~/.config/modixfs/tools/weather/forecast
# tool appears immediately — no restart needed
```

**Enable in tools.yaml**

```yaml
tools_dir: ~/.config/modixfs/tools
timeout: 30   # seconds before a subprocess is killed
```

**Hot-reload**

ModixFS watches `tools_dir` with inotify (Linux) / kqueue (macOS). Adding or removing a tool directory is picked up immediately.

---

### Secrets management

`modixfs install` stores secrets in `~/.config/modixfs/secrets.env` (mode `0600`). You can also edit it manually:

```
# ~/.config/modixfs/secrets.env
GITHUB_TOKEN=ghp_...
MYTOOL_API_KEY=sk-...
```

`modixfs mount` loads this file at startup. Shell environment always takes precedence — existing env vars are never overwritten.

---

### Publishing a tool

Add `modix.yaml` to your tool directory to make it installable with a single command:

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

`required: true` vars trigger an interactive prompt at install time. Tools without `modix.yaml` install fine — just without prompts.

Push to a public GitHub repo and share:

```bash
modixfs install github.com/you/your-tool

# For a tool inside a subdirectory
modixfs install github.com/owner/repo/tree/main/mytool
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
ModixFS (FUSE)
    ├── Virtual File Router     path → inode mapping
    ├── Tool Registry           Tool trait + hot-reload watcher
    └── Secrets Loader          ~/.config/modixfs/secrets.env → process env
            │
    Tool Implementations
    (async HTTP, subprocess, passthrough files)
```

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
