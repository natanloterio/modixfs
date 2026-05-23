# 🗂️ LiveFoldersFS

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

# 2. Install a tool from the registry
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

Or register LiveFolders as an MCP server so Claude Code picks it up automatically:

```bash
livefolders mcp register
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

By the time `cat` returns, the handler has finished. Internally the runtime spawns each handler onto a Tokio task and the `cat` blocks on a notification rather than the FUSE thread, so unrelated tools stay responsive while a slow handler runs.

### Concurrency

Two shells can run `echo X > ep && cat ep` against the same endpoint in parallel and each will get its own correct result. State is scoped per shell session (via `getsid`), so `echo` and `cat` from the same pipeline share a slot and pipelines from different shells don't clobber each other. Handlers themselves run in parallel on the Tokio runtime — slow handlers don't queue.

The boundary is the *shell session*, not the user. Two pipelines launched from the same shell will still serialise on the slot; for cross-session isolation across the same endpoint, use distinct shells (`setsid bash -c …` or just two terminal windows). Handlers that share external state (a file, a database, an API quota) should declare a `state_file` — see [Building tools](docs/building-tools.md).

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
```
echo "" > .livefolders/tools/weather/forecast
cat .livefolders/tools/weather/forecast
# → [ERROR:INVALID_INPUT] input too short: minimum 1 characters required
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

---

## Installing tools

```bash
# From the registry (short form)
livefolders install owner/name
livefolders install owner/name@v1.2.0   # pin to a version

# From GitHub directly
livefolders install github.com/owner/repo
livefolders install github.com/owner/repo/tree/main/mytool
```

If a tool requires secrets, you'll be prompted for them on install.

---

## Tool registry

The [LiveFolders registry](https://registry.livefolders.org) is a public index of tools.

```bash
livefolders search weather        # search for tools
livefolders info owner/name       # show details for a tool
livefolders publish               # publish this repo to the registry
```

---

## MCP server

LiveFolders can act as an MCP server, exposing all mounted tools to any MCP-aware client (Claude Desktop, Claude Code, etc.).

```bash
livefolders mcp                              # start MCP server over stdio
livefolders mcp register                     # register with Claude Code (~/.claude.json)
livefolders mcp register /path/to/livefolders.yaml  # register a named project
```

Each endpoint becomes an MCP tool named `<tool>__<endpoint>` (e.g. `weather__forecast`).

---

## Example tools

| Tool | Install | What it does |
|------|---------|--------------|
| `demo` | `livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/demo` | Demonstrates all file types and input schema validation |
| `users` | `livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users` | Fetches users from a REST API via GET |

---

## Going further

- [Building tools](docs/building-tools.md) — `folder.yaml` reference, file types, input validation, stateful tools, concurrency model, pipelines, secrets, hot-reload
- [Security](docs/security.md) — sandbox model, Landlock/seccomp, network access, strict mode, session scoping, known limitations
- [LiveFoldersFS vs MCP](docs/livefoldersfs-vs-mcp.md) — comparison framework across 10 criteria

---

## License

Apache 2.0 — see [LICENSE](LICENSE).
