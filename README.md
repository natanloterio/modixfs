# ModixFS

A virtual filesystem alternative to MCP (Model Context Protocol). Instead of JSON-RPC and schema overhead, ModixFS exposes tools to LLMs as files — using `cat`, `echo`, and pipes that LLMs already know how to use.

```
cat /tools/github/how_to.md                          # discover how to use a tool
echo "language:rust fuse stars:>100" > /tools/github/search_repos
cat /tools/github/search_repos                       # read the results
```

## Why

MCP requires a protocol layer (JSON-RPC), schema definitions, and a dedicated client. ModixFS uses the filesystem as the protocol — an interface every LLM already speaks natively.

| | MCP | ModixFS |
|---|---|---|
| Protocol | JSON-RPC | File I/O |
| Discovery | Tool list API | `ls` / `cat` |
| Documentation | Schema | Free-form Markdown |
| Invocation | Function call | File write |
| Result | JSON response | File read |
| Composition | Limited | Shell pipes |

## Install

### Linux

```bash
sudo apt-get install fuse3

curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-linux-x86_64 -o modixfs
chmod +x modixfs && sudo mv modixfs /usr/local/bin/
```

### macOS

Install [macFUSE](https://osxfuse.github.io) first, then:

```bash
# Apple Silicon
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-macos-aarch64 -o modixfs

# Intel
curl -L https://github.com/natanloterio/modixfs/releases/latest/download/modixfs-macos-x86_64 -o modixfs

chmod +x modixfs && sudo mv modixfs /usr/local/bin/
```

### From source

```bash
sudo apt-get install libfuse3-dev pkg-config  # Linux only
cargo install --git https://github.com/natanloterio/modixfs
```

## Quick start

```bash
# 1. Create a tools.yaml in your project
modixfs init

# 2. Edit tools.yaml to enable the tools you want
# 3. Set any required tokens
export GITHUB_TOKEN=your_token

# 4. Mount
modixfs mount
```

The filesystem is now live. Point your LLM agent at it.

## Configuration

`tools.yaml` (created by `modixfs init`):

```yaml
mount: /tmp/modixfs   # where to mount

tools:
  - name: echo        # always available, useful for testing

  - name: github
    token_env: GITHUB_TOKEN   # env var holding the token (this is the default)
```

Override the mount path at runtime:

```bash
modixfs mount /my/custom/path
modixfs mount --config /path/to/other.yaml
```

## How it works

Each tool exposes a directory under `/tools/<name>/`:

```
/tools/
├── index.md              ← lists all available tools
├── github/
│   ├── how_to.md         ← usage instructions (LLM reads this)
│   ├── search_repos      ← write query → read results
│   └── search_code
└── echo/
    ├── how_to.md
    └── send
```

Write to an endpoint to invoke it. Read to get the result. The result is cleared after reading, ready for the next invocation.

```bash
echo "tokio async runtime stars:>1000" > /tools/github/search_repos
sleep 2
cat /tools/github/search_repos
```

## Built-in tools

### `echo`
Reflects input back as output. Useful for verifying the filesystem is working.

### `github`
Searches GitHub using the [GitHub Search API](https://docs.github.com/en/search-github).

| Endpoint | Description |
|---|---|
| `search_repos` | Search repositories |
| `search_code` | Search code across GitHub |

Requires `GITHUB_TOKEN`.

## External tools

Any developer can build a ModixFS tool without writing Rust. Create a directory
in your `tools_dir` and add scripts — no recompile, no restart required.

### Directory convention

```
~/.config/modixfs/tools/
└── mytool/
    ├── how_to.md        ← LLM reads this to learn the tool
    ├── search           ← executable: write to invoke, read for result
    ├── output.csv       ← passthrough: LLM reads this file directly
    └── config.json      ← passthrough: LLM can write to configure the tool
```

### File behavior

| File | Behavior |
|---|---|
| `how_to.md` | Read-only docs served directly from disk |
| Executable (`chmod +x`) | Write → stdin. Stdout → result on next read. |
| Regular file | Passthrough to disk. Reads and writes go directly to the file. |

### Subprocess environment

Scripts receive:
- `stdin` — what the LLM wrote to the endpoint
- `MODIXFS_TOOL` — tool name
- `MODIXFS_ENDPOINT` — endpoint name
- All env vars set when `modixfs` was launched

### Enable in tools.yaml

```yaml
tools_dir: ~/.config/modixfs/tools
timeout: 30
```

### The LLM can create tools too

```bash
mkdir /tools/mytool
echo "# My Tool" > /tools/mytool/how_to.md
printf '#!/bin/bash\ncurl -s https://api.example.com -d "$(cat -)"\n' > /tools/mytool/fetch
chmod +x /tools/mytool/fetch
# tool is immediately live — no restart needed
```

## Adding tools

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

## Architecture

```
LLM Agent
    │ read/write syscalls
ModixFS (FUSE)
    ├── Virtual File Router   (path → inode mapping)
    └── Tool Registry         (Tool trait + Session state)
            │
    Tool Implementations
    (async HTTP, shell, anything)
```

State machine per endpoint file:

```
IDLE → write(input) → invoke() → COMPLETE → read() → IDLE
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
