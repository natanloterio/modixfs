"""
Token-efficiency benchmark: LiveFoldersFS vs MCP.

Runs identical tasks against both backends, records per-turn token usage via
the Anthropic API, and logs everything to Weights & Biases Weave.

Usage:
    # LiveFoldersFS backend (requires livefolders mounted at $LF_MOUNT_PATH):
    LF_MOUNT_PATH=.livefolders python benchmark.py --backend livefolders

    # MCP backend (requires MCP server running; pass stdio command):
    python benchmark.py --backend mcp --mcp-cmd "python mcp/server.py"

    # Both in sequence:
    python benchmark.py --backend both --mcp-cmd "python mcp/server.py"

Requirements:
    pip install anthropic weave mcp httpx
    export ANTHROPIC_API_KEY=...
    export WANDB_API_KEY=...         # or: wandb login
"""

import argparse
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

import anthropic
import weave
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from tasks import TASKS

MODEL = "claude-haiku-4-5-20251001"  # cheapest model; swap to sonnet for production
MAX_TURNS = 6                         # safety cap per task

WEAVE_PROJECT = "livefolders-token-efficiency"


# ── Data model ────────────────────────────────────────────────────────────────

@dataclass
class TurnRecord:
    turn: int
    input_tokens: int
    output_tokens: int
    cache_read_tokens: int
    cache_write_tokens: int

    @property
    def total(self) -> int:
        return self.input_tokens + self.output_tokens


@dataclass
class TaskResult:
    task_id: str
    backend: str
    turns: list[TurnRecord] = field(default_factory=list)
    completed: bool = False
    error: str | None = None

    @property
    def total_input(self) -> int:
        return sum(t.input_tokens for t in self.turns)

    @property
    def total_output(self) -> int:
        return sum(t.output_tokens for t in self.turns)

    @property
    def total_tokens(self) -> int:
        return self.total_input + self.total_output

    def to_dict(self) -> dict:
        return {
            "task_id": self.task_id,
            "backend": self.backend,
            "completed": self.completed,
            "error": self.error,
            "total_input_tokens": self.total_input,
            "total_output_tokens": self.total_output,
            "total_tokens": self.total_tokens,
            "turns": [vars(t) for t in self.turns],
        }


# ── Anthropic helpers ─────────────────────────────────────────────────────────

def _record_usage(result: TaskResult, turn_idx: int, usage) -> None:
    result.turns.append(TurnRecord(
        turn=turn_idx,
        input_tokens=usage.input_tokens,
        output_tokens=usage.output_tokens,
        cache_read_tokens=getattr(usage, "cache_read_input_tokens", 0) or 0,
        cache_write_tokens=getattr(usage, "cache_creation_input_tokens", 0) or 0,
    ))


# ── LiveFoldersFS backend ─────────────────────────────────────────────────────

LF_SYSTEM_PROMPT = """You have access to a bash tool. The filesystem at {mount}/tools/
exposes tools as plain files. Use shell commands to interact with them:

  cat {mount}/tools/index.md          # discover available tools
  cat {mount}/tools/<name>/how_to.md  # read usage instructions
  cat {mount}/tools/<name>/<endpoint> # invoke a read_invoke endpoint
  echo "input" > {mount}/tools/<name>/<endpoint>  # invoke a write_invoke endpoint
  cat {mount}/tools/<name>/<endpoint> # read the result

Always read how_to.md before using a tool you haven't used before.
Return only the final answer to the user — no internal monologue.
"""


@weave.op()
def run_livefolders_task(task: dict, mount_path: str) -> dict:
    client = anthropic.Anthropic()
    result = TaskResult(task_id=task["id"], backend="livefolders")

    system = LF_SYSTEM_PROMPT.format(mount=mount_path)
    bash_tool = {
        "name": "bash",
        "description": "Run a bash command and return stdout+stderr.",
        "input_schema": {
            "type": "object",
            "properties": {"command": {"type": "string"}},
            "required": ["command"],
        },
    }

    messages = [{"role": "user", "content": task["user_turn"]}]

    for turn_idx in range(MAX_TURNS):
        response = client.messages.create(
            model=MODEL,
            max_tokens=1024,
            system=system,
            tools=[bash_tool],
            messages=messages,
        )
        _record_usage(result, turn_idx, response.usage)

        # Collect assistant message
        messages.append({"role": "assistant", "content": response.content})

        # Check stop condition on text blocks
        text_parts = [b.text for b in response.content if hasattr(b, "text")]
        full_text = " ".join(text_parts)
        if response.stop_reason == "end_turn" and task["stop_when"](full_text):
            result.completed = True
            break

        # Execute any tool uses
        tool_results = []
        for block in response.content:
            if block.type != "tool_use":
                continue
            cmd = block.input.get("command", "")
            try:
                proc = subprocess.run(
                    cmd, shell=True, capture_output=True, text=True, timeout=15
                )
                output = proc.stdout + proc.stderr
            except subprocess.TimeoutExpired:
                output = "[ERROR:TIMEOUT] command exceeded 15s"
            tool_results.append({
                "type": "tool_result",
                "tool_use_id": block.id,
                "content": output or "(no output)",
            })

        if not tool_results:
            # No tool calls and stop condition not met — model is done
            result.completed = True
            break

        messages.append({"role": "user", "content": tool_results})

    return result.to_dict()


# ── MCP backend ───────────────────────────────────────────────────────────────

MCP_SYSTEM_PROMPT = (
    "You have access to tools via function calls. "
    "Use them to answer the user's question. "
    "Return only the final answer — no internal monologue."
)


@weave.op()
async def run_mcp_task(task: dict, mcp_cmd: str) -> dict:
    import asyncio

    client = anthropic.Anthropic()
    result = TaskResult(task_id=task["id"], backend="mcp")

    cmd_parts = mcp_cmd.split()
    server_params = StdioServerParameters(command=cmd_parts[0], args=cmd_parts[1:])

    async with stdio_client(server_params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()

            # Fetch tool schemas from the MCP server
            tools_response = await session.list_tools()
            anthropic_tools = [
                {
                    "name": t.name,
                    "description": t.description or "",
                    "input_schema": t.inputSchema,
                }
                for t in tools_response.tools
            ]

            messages = [{"role": "user", "content": task["user_turn"]}]

            for turn_idx in range(MAX_TURNS):
                response = client.messages.create(
                    model=MODEL,
                    max_tokens=1024,
                    system=MCP_SYSTEM_PROMPT,
                    tools=anthropic_tools,
                    messages=messages,
                )
                _record_usage(result, turn_idx, response.usage)

                messages.append({"role": "assistant", "content": response.content})

                text_parts = [b.text for b in response.content if hasattr(b, "text")]
                full_text = " ".join(text_parts)
                if response.stop_reason == "end_turn" and task["stop_when"](full_text):
                    result.completed = True
                    break

                tool_results = []
                for block in response.content:
                    if block.type != "tool_use":
                        continue
                    call_result = await session.call_tool(block.name, block.input)
                    content = (
                        call_result.content[0].text
                        if call_result.content
                        else "(no output)"
                    )
                    tool_results.append({
                        "type": "tool_result",
                        "tool_use_id": block.id,
                        "content": content,
                    })

                if not tool_results:
                    result.completed = True
                    break

                messages.append({"role": "user", "content": tool_results})

    return result.to_dict()


# ── Comparison report ─────────────────────────────────────────────────────────

def print_report(results: list[dict]) -> None:
    print("\n" + "=" * 72)
    print(f"{'Task':<20} {'Backend':<14} {'Input':>8} {'Output':>8} {'Total':>8}  Done")
    print("-" * 72)

    by_task: dict[str, dict] = {}
    for r in results:
        by_task.setdefault(r["task_id"], {})[r["backend"]] = r
        status = "✓" if r["completed"] else "✗"
        print(
            f"{r['task_id']:<20} {r['backend']:<14} "
            f"{r['total_input_tokens']:>8} {r['total_output_tokens']:>8} "
            f"{r['total_tokens']:>8}  {status}"
        )

    print("=" * 72)
    print("\nDelta (livefolders − mcp), positive = LiveFoldersFS costs more tokens\n")
    for task_id, backends in by_task.items():
        if "livefolders" in backends and "mcp" in backends:
            lf = backends["livefolders"]["total_tokens"]
            mcp = backends["mcp"]["total_tokens"]
            delta = lf - mcp
            pct = (delta / mcp * 100) if mcp else float("nan")
            print(f"  {task_id:<20}  Δ {delta:+6}  ({pct:+.1f}%)")
    print()


# ── Entry point ───────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Token-efficiency benchmark")
    parser.add_argument(
        "--backend",
        choices=["livefolders", "mcp", "both"],
        default="both",
    )
    parser.add_argument(
        "--mcp-cmd",
        default="python mcp/server.py",
        help="Shell command to launch the MCP server via stdio",
    )
    parser.add_argument(
        "--mount",
        default=os.environ.get("LF_MOUNT_PATH", ".livefolders"),
        help="LiveFoldersFS mount point (default: $LF_MOUNT_PATH or .livefolders)",
    )
    parser.add_argument(
        "--tasks",
        nargs="*",
        help="Subset of task IDs to run (default: all)",
    )
    args = parser.parse_args()

    weave.init(WEAVE_PROJECT)

    tasks = TASKS
    if args.tasks:
        tasks = [t for t in TASKS if t["id"] in args.tasks]

    results: list[dict] = []

    run_lf = args.backend in ("livefolders", "both")
    run_mcp = args.backend in ("mcp", "both")

    if run_lf and not Path(args.mount).exists():
        print(
            f"[warn] LiveFoldersFS mount not found at '{args.mount}'. "
            "Set --mount or $LF_MOUNT_PATH.",
            file=sys.stderr,
        )
        run_lf = False

    for task in tasks:
        print(f"[task:{task['id']}] {task['description']}")

        if run_lf:
            print(f"  running livefolders...")
            r = run_livefolders_task(task, args.mount)
            results.append(r)
            print(f"  → {r['total_tokens']} tokens, completed={r['completed']}")

        if run_mcp:
            import asyncio
            print(f"  running mcp...")
            r = asyncio.run(run_mcp_task(task, args.mcp_cmd))
            results.append(r)
            print(f"  → {r['total_tokens']} tokens, completed={r['completed']}")

        time.sleep(0.5)  # avoid rate-limit bursts between tasks

    # Write raw results
    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(results, indent=2))
    print(f"\nRaw results written to {out_path}")

    print_report(results)
    print(f"Traces available at: https://wandb.ai/{WEAVE_PROJECT}")


if __name__ == "__main__":
    main()
