# Files All the Way Down: Why I Stopped Writing MCP Servers

I was setting up another [MCP](https://modelcontextprotocol.io/specification/2025-11-25) server last year when it finally got to me.

The task: expose a single REST endpoint to Claude — fetch a list of users, format it as markdown, return it. Thirty seconds of work if you're just writing a shell script. Instead I was writing the import block, the `FastMCP("users")` init, the `@mcp.tool()` decorator, the docstring (required, because that's how MCP surfaces the description), the `httpx` call, the response handling, the `if __name__ == "__main__": mcp.run()` at the bottom. Eighteen lines for something that calls one URL.

And then I thought: if this server crashes, I have to kill it, restart it, and wait for Claude Code to finish the reconnect handshake. If I want someone else to use this tool, I have to either point them at a registry that barely exists or have them manually edit their config file.

I'm not writing a protocol server. I'm fetching some JSON.

---

## The Insight

LLMs already know how to use files. They've been trained on decades of shell scripts, man pages, Unix philosophy. When an agent runs `cat README.md`, it doesn't need a protocol. When it runs `ls tools/`, it doesn't need a schema document. File I/O is the most universal interface in computing.

So I started wondering: what if tools were just files?

Not metaphorically — literally. A file you write to invokes the tool. A file you read gives you the result. The entire interface is `echo` and `cat`.

```bash
echo "London" > .livefolders/tools/weather/forecast
cat .livefolders/tools/weather/forecast
# → Weather report for London, United Kingdom:
# →    ☁️   Overcast
# →   15 °C
```

That's it. No client library. No JSON-RPC. No tool-call response envelope to unwrap.

---

## What LiveFolders Is

LiveFolders is a FUSE filesystem. When you mount it, a `.livefolders/` directory appears in your project. Inside, every tool is a directory. Every endpoint is a file.

When you write to an endpoint file, the FUSE layer intercepts the `release()` call — the moment the file descriptor closes — and runs the tool's handler with your input on stdin. When you read the file back, you get the output. The write blocks until the handler finishes, so by the time `cat` runs, the result is already there.

Tools are defined in a `folder.yaml`. Here's the full definition for a users tool that calls a REST API:

```yaml
name: users
description: Fetch users from the API.
files:
  - name: list
    type: read_invoke
    handler: >-
      curl -s https://api.example.com/users
      | jq -r '"# Users\n", (.[] | "## \(.name)\nID: \(.id)\n")'
```

Ten lines. No runtime to install, no language to pick, no server process to manage. The handler is a shell command. If you can write a one-liner in bash, you can write a LiveFolders tool.

Each tool directory also gets a `how_to.md` and a `schema.json` generated automatically from the manifest — so the agent can discover what's available and how to call it with a plain `cat`, no protocol negotiation required.

---

## The Contrast

Here's the same tool as an MCP server:

```python
import httpx
from mcp.server.fastmcp import FastMCP
mcp = FastMCP("users")

@mcp.tool()
def list_users() -> str:
    """Fetches all users from the API."""
    response = httpx.get("https://api.example.com/users")
    response.raise_for_status()
    users = response.json()
    lines = ["# Users", ""]
    for u in users:
        lines.append(f"## {u['name']}")
        lines.append(f"ID: {u['id']}")
        lines.append("")
    return "\n".join(lines)

if __name__ == "__main__":
    mcp.run()
```

Eighteen lines, one dependency, a running process to manage.

The line count is the smallest part of the gap. The bigger differences:

**Hot-reload.** Change a handler in LiveFolders and it's live in under a second — an inotify watcher picks up the edit automatically. Change `server.py` in MCP and you kill the process, restart it, wait for Claude Code to finish reconnecting. For fast iteration this is death by a thousand cuts.

**Publishing.** Any GitHub repo with a `folder.yaml` is installable with one command:

```bash
livefolders install github.com/you/repo/tree/main/my-tool
```

No registry. No package upload. No config editing. Just a URL.

**Discoverability.** Every tool ships with a `how_to.md` that the agent reads with `cat`. It's auto-generated from the manifest — always accurate, always in sync. The agent doesn't need to know it's talking to LiveFolders. It just reads a file.

**Stateful tools.** Declare `state_file: /var/data/my-tool.db` in the manifest and the runtime passes `LIVEFOLDERS_STATE_FILE` to your handler while holding an exclusive `flock`. Concurrent invocations serialize automatically. No locking code to write.

**Pipelines.** Declare `pipe: [fetch, format, cache]` and a single write invocation chains all three handlers, stdout to stdin, with per-stage schema validation. The LLM sees one atomic operation.

---

## Where It Falls Short

I'm not going to pretend this is better than MCP in every way, because it isn't.

**Shell access required.** If your LLM is API-only with no shell, LiveFolders doesn't work. MCP runs as a separate process that the client connects to over stdio or HTTP — it doesn't need the agent to have filesystem access. That's a real architectural advantage in constrained environments.

**Opt-in validation.** MCP enforces input schemas unconditionally at the protocol layer — malformed inputs are rejected before any handler code runs, always. LiveFolders supports structural validation (`min_length`, `max_length`, regex `pattern`, JSON schema subsets) but you have to declare it per endpoint. If a tool author skips it, there's no safety net. This is a gap worth being honest about.

**Linux and macOS only.** FUSE is not Windows. If your agents run on Windows, this is a blocker today.

**It's alpha.** I'm at v0.10. Things work, but the edges are rough. I've been running it in my own workflow for months, but I wouldn't call it production-ready.

I wrote a research paper that goes deeper on the tradeoffs — comparing LiveFolders against MCP and five other filesystem-based tool integration systems ([ToolFS](https://github.com/IceWhaleTech/toolfs), [AgentFS](https://github.com/tursodatabase/agentfs), [llm9p](https://github.com/NERVsystems/llm9p), [InferNode](https://github.com/NERVsystems/infernode), and [Quine](https://arxiv.org/abs/2603.18030)) across ten criteria. If you want the full picture, [it's in the repo](https://github.com/natanloterio/LiveFolders/tree/master/paper). The short version: LiveFolders wins on ergonomics and MCP wins on safety guarantees, and I think the right long-term answer pulls from both.

---

## Try It

If you're building AI agents on Linux or macOS and you've felt the MCP boilerplate tax, I'd genuinely love for you to try this.

```bash
curl -fsSL https://raw.githubusercontent.com/natanloterio/LiveFolders/master/install.sh | bash
```

Then:

```bash
livefolders init
livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users
livefolders mount
cat .livefolders/tools/users/list
```

Build a tool. Break something. [Open an issue](https://github.com/natanloterio/LiveFolders/issues). Tell me what's missing.

The repo is at [github.com/natanloterio/LiveFolders](https://github.com/natanloterio/LiveFolders). If you want to write a tool from scratch, there's a `create_tool.md` guide in the repo that walks through the full `folder.yaml` format — it's designed to be readable by both humans and LLMs.

I'm building this in public. Feedback at this stage shapes the project more than anything else will later.

---

*Further reading:*
- *[Model Context Protocol Specification](https://modelcontextprotocol.io/specification/2025-11-25) — the standard LiveFolders is compared against; [donated to the Linux Foundation](https://www.anthropic.com/news/mcp-linux-foundation) in December 2025*
- *[ReAct: Synergizing Reasoning and Acting in Language Models](https://arxiv.org/abs/2210.03629) — foundational paper on LLM tool-use as interleaved reasoning and action*
- *[Toolformer: Language Models Can Teach Themselves to Use Tools](https://arxiv.org/abs/2302.04761) — showed LLMs can learn tool use without task-specific fine-tuning*
- *[From Commands to Prompts: LLM-based Semantic File System for AIOS](https://arxiv.org/abs/2410.11843) — a third paradigm: making the filesystem itself LLM-aware (part of [AIOS](https://arxiv.org/abs/2403.16971))*
- *[The UNIX Time-Sharing System](https://doi.org/10.1145/361011.361061) (Ritchie & Thompson, 1974) — the origin of "everything is a file"*
- *[Plan 9 from Bell Labs](https://dl.acm.org/doi/10.5555/1268708) — where the file philosophy was taken to its logical extreme*
- *[The Use of Name Spaces in Plan 9](https://doi.org/10.1145/155848.155861) (Pike et al., 1993) — the namespace model that Quine and InferNode build on*
