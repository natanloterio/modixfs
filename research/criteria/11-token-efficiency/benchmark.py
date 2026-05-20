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

    # Add optimised LF variant (short prompt + prompt caching):
    python benchmark.py --backend both --cache --mcp-cmd "python mcp/server.py"

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
MAX_TURNS = 10                        # safety cap per task

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
    def total_cache_read(self) -> int:
        return sum(t.cache_read_tokens for t in self.turns)

    @property
    def total_cache_write(self) -> int:
        return sum(t.cache_write_tokens for t in self.turns)

    @property
    def total_tokens(self) -> int:
        return self.total_input + self.total_output

    @property
    def effective_tokens(self) -> float:
        """Billing-weighted token count: cache reads cost 10%, writes cost 125%."""
        return (
            self.total_input
            + self.total_output
            - self.total_cache_read * 0.90   # saved 90% on reads
            + self.total_cache_write * 0.25  # paid 25% extra to write
        )

    def to_dict(self) -> dict:
        return {
            "task_id": self.task_id,
            "backend": self.backend,
            "completed": self.completed,
            "error": self.error,
            "total_input_tokens": self.total_input,
            "total_output_tokens": self.total_output,
            "total_cache_read_tokens": self.total_cache_read,
            "total_cache_write_tokens": self.total_cache_write,
            "total_tokens": self.total_tokens,
            "effective_tokens": round(self.effective_tokens),
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

# Verbose prompt used in the baseline run.
LF_SYSTEM_PROMPT = """You have a bash tool. Tools are files at {mount}/tools/.

Available tools:
  users — list users from the mock API

Invoke directly — do NOT read index.md, how_to.md, or any other docs first:
  cat {mount}/tools/<name>/<endpoint>                                          # read_invoke
  echo "input" > {mount}/tools/<name>/<endpoint> && cat {mount}/tools/<name>/<endpoint>  # write_invoke

Return only the final answer.
"""

# Short prompt used with --cache. Relies on model's bash knowledge.
LF_SYSTEM_PROMPT_SHORT = (
    "Tools are at {mount}/tools/. "
    "cat <tool>/how_to.md to learn usage. "
    "cat or echo to invoke. "
    "Reply with only the final answer."
)

# v2: explicitly lists user-facing endpoints (mirrors improved system_prompt.md)
LF_SYSTEM_PROMPT_V2 = (
    "Bash tools at {mount}/tools/. cat = read; echo X > path && cat path = write. "
    "Return only the final answer.\n\n"
    "users — List users from the mock API.\n"
    "  cat {mount}/tools/users/count          # integer count\n"
    "  cat {mount}/tools/users/list_compact   # id:name per line (use for name/ID tasks)\n"
    "  cat {mount}/tools/users/list_full      # full details: name, id, createdAt, avatar\n"
    "  echo NAME > {mount}/tools/users/search && cat {mount}/tools/users/search  # find by name → id:name\n"
)


def read_system_prompt_from_mount(mount_path: str) -> str | None:
    """Read system_prompt.md synthesized by LiveFoldersFS, or None if not present."""
    p = Path(mount_path) / "system_prompt.md"
    if p.exists():
        return p.read_text()
    return None


@weave.op()
def run_livefolders_task(
    task: dict,
    mount_path: str,
    use_cache: bool = False,
    system_prompt_override: str | None = None,
    v2: bool = False,
) -> dict:
    client = anthropic.Anthropic()
    if system_prompt_override is not None:
        backend_label = "livefolders-manifest"
    elif v2:
        backend_label = "livefolders-v2"
    elif use_cache:
        backend_label = "livefolders-cached"
    else:
        backend_label = "livefolders"
    result = TaskResult(task_id=task["id"], backend=backend_label)

    if system_prompt_override is not None:
        prompt_text = system_prompt_override
    elif v2:
        prompt_text = LF_SYSTEM_PROMPT_V2.format(mount=mount_path)
    elif use_cache:
        prompt_text = LF_SYSTEM_PROMPT_SHORT.format(mount=mount_path)
    else:
        prompt_text = LF_SYSTEM_PROMPT.format(mount=mount_path)

    # Wrap in a cacheable content block when --cache is active.
    if use_cache:
        system: str | list = [{"type": "text", "text": prompt_text, "cache_control": {"type": "ephemeral"}}]
    else:
        system = prompt_text

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

        text_parts = [b.text for b in response.content if hasattr(b, "text")]
        full_text = " ".join(text_parts)

        # End-of-turn: model stopped generating — always terminate the loop.
        # Mark completed based on whether the answer satisfies stop_when.
        if response.stop_reason == "end_turn":
            result.completed = task["stop_when"](full_text)
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

        # Mid-loop: model answered in a text block before/alongside a tool call.
        if not tool_results:
            result.completed = task["stop_when"](full_text)
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

                if response.stop_reason == "end_turn":
                    result.completed = task["stop_when"](full_text)
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
                    result.completed = task["stop_when"](full_text)
                    break

                messages.append({"role": "user", "content": tool_results})

    return result.to_dict()


# ── LiveFoldersFS native backend ─────────────────────────────────────────────

NATIVE_SYSTEM_PROMPT = "Use the tools to answer. Return only the final answer."


# ── Unified-tool helpers ───────────────────────────────────────────────────────

def _load_native_tools(mount_path: str) -> tuple[list[dict], dict[str, str]]:
    """Read anthropic_tools.json from each tool dir; return (tools, kind_map)."""
    tools_root = Path(mount_path) / "tools"
    combined: list[dict] = []
    kinds: dict[str, str] = {}
    for td in sorted(tools_root.iterdir()):
        if not td.is_dir():
            continue
        try:
            tools_data = json.loads((td / "anthropic_tools.json").read_text())
        except Exception:
            continue
        for t in tools_data:
            combined.append(t)
            props = t.get("input_schema", {}).get("properties", {})
            kinds[t["name"]] = "write" if "input" in props else "read"
    return combined, kinds


def _build_unified_tools(native_tools: list[dict]) -> tuple[list[dict], dict[str, dict[str, str]]]:
    """
    Collapse N per-endpoint tools into 1 tool per namespace with an action enum.

    Returns (unified_tools, dispatch_map) where dispatch_map maps
    namespace → {action_name: original_tool_name}.
    """
    from collections import defaultdict
    by_ns: dict[str, list[dict]] = defaultdict(list)
    for t in native_tools:
        ns, ep = t["name"].split("__", 1)
        by_ns[ns].append({"ep": ep, "tool": t})

    unified: list[dict] = []
    dispatch: dict[str, dict[str, str]] = {}
    for ns, entries in by_ns.items():
        actions = [e["ep"] for e in entries]
        has_write = any(
            "input" in e["tool"].get("input_schema", {}).get("properties", {})
            for e in entries
        )
        # Build per-action description lines for the enum description
        action_lines = []
        for e in entries:
            desc = e["tool"].get("description", "")
            line = f"{e['ep']}: {desc}" if desc else e["ep"]
            action_lines.append(line)
        action_desc = "; ".join(action_lines)
        props: dict = {
            "action": {
                "type": "string",
                "enum": actions,
                "description": action_desc,
            },
        }
        if has_write:
            props["query"] = {"type": "string", "description": "input for write actions (search query, etc.)"}
        unified.append({
            "name": ns,
            "description": f"{ns} API. Actions: {action_desc}",
            "input_schema": {"type": "object", "properties": props, "required": ["action"]},
        })
        dispatch[ns] = {e["ep"]: e["tool"]["name"] for e in entries}
    return unified, dispatch


def _exec_native_tool(block, mount_path: str, kinds: dict[str, str]) -> str:
    """Execute a native (non-unified) LF tool call via bash."""
    sep_idx = block.name.find("__")
    if sep_idx == -1:
        return f"[ERROR] unexpected tool name: {block.name}"
    tool_name = block.name[:sep_idx]
    ep_name = block.name[sep_idx + 2:]
    kind = kinds.get(block.name, "read")
    if kind == "write":
        raw_input = block.input.get("input", "")
        cmd = (
            f"printf '%s' {repr(raw_input)} "
            f"> {mount_path}/tools/{tool_name}/{ep_name} && "
            f"cat {mount_path}/tools/{tool_name}/{ep_name}"
        )
    else:
        cmd = f"cat {mount_path}/tools/{tool_name}/{ep_name}"
    try:
        proc = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=15)
        return proc.stdout + proc.stderr
    except subprocess.TimeoutExpired:
        return "[ERROR:TIMEOUT] command exceeded 15s"


def _exec_unified_tool(block, mount_path: str, dispatch: dict[str, dict[str, str]],
                       kinds: dict[str, str]) -> str:
    """Execute a unified tool call by resolving action → native tool, then running bash."""
    ns = block.name
    action = block.input.get("action", "")
    ep_map = dispatch.get(ns, {})
    full_name = ep_map.get(action)
    if not full_name:
        return f"[ERROR] unknown action '{action}' for tool '{ns}'"
    kind = kinds.get(full_name, "read")
    ep_name = full_name.split("__", 1)[1]
    if kind == "write":
        raw_input = block.input.get("query", "")
        cmd = (
            f"printf '%s' {repr(raw_input)} "
            f"> {mount_path}/tools/{ns}/{ep_name} && "
            f"cat {mount_path}/tools/{ns}/{ep_name}"
        )
    else:
        cmd = f"cat {mount_path}/tools/{ns}/{ep_name}"
    try:
        proc = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=15)
        return proc.stdout + proc.stderr
    except subprocess.TimeoutExpired:
        return "[ERROR:TIMEOUT] command exceeded 15s"


def _run_native_loop(task: dict, result: TaskResult, tools: list[dict],
                     executor, mount_path: str) -> None:
    """Shared agentic loop for all native-tool backends."""
    client = anthropic.Anthropic()
    messages = [{"role": "user", "content": task["user_turn"]}]
    system: str | list = NATIVE_SYSTEM_PROMPT

    for turn_idx in range(MAX_TURNS):
        response = client.messages.create(
            model=MODEL,
            max_tokens=1024,
            system=system,
            tools=tools,
            messages=messages,
        )
        _record_usage(result, turn_idx, response.usage)
        messages.append({"role": "assistant", "content": response.content})

        text_parts = [b.text for b in response.content if hasattr(b, "text")]
        full_text = " ".join(text_parts)

        if response.stop_reason == "end_turn":
            result.completed = task["stop_when"](full_text)
            break

        tool_results = []
        for block in response.content:
            if block.type != "tool_use":
                continue
            output = executor(block)
            tool_results.append({
                "type": "tool_result",
                "tool_use_id": block.id,
                "content": output or "(no output)",
            })

        if not tool_results:
            result.completed = task["stop_when"](full_text)
            break

        messages.append({"role": "user", "content": tool_results})


@weave.op()
def run_livefolders_native_task(task: dict, mount_path: str) -> dict:
    """LF endpoints as separate native Anthropic tools — no bash tool."""
    native_tools, kinds = _load_native_tools(mount_path)
    result = TaskResult(task_id=task["id"], backend="livefolders-native")
    _run_native_loop(task, result, native_tools,
                     lambda b: _exec_native_tool(b, mount_path, kinds), mount_path)
    return result.to_dict()


@weave.op()
def run_livefolders_unified_task(task: dict, mount_path: str) -> dict:
    """LF endpoints collapsed into one tool per namespace with an action enum."""
    native_tools, kinds = _load_native_tools(mount_path)
    unified_tools, dispatch = _build_unified_tools(native_tools)
    result = TaskResult(task_id=task["id"], backend="livefolders-unified")
    _run_native_loop(task, result, unified_tools,
                     lambda b: _exec_unified_tool(b, mount_path, dispatch, kinds), mount_path)
    return result.to_dict()


@weave.op()
def run_livefolders_unified_cached_task(task: dict, mount_path: str) -> dict:
    """Unified tool + Anthropic prompt caching on the (tiny) tool schemas."""
    native_tools, kinds = _load_native_tools(mount_path)
    unified_tools, dispatch = _build_unified_tools(native_tools)

    # Wrap the last tool with cache_control so the tools block is cached after turn 0.
    cached_tools = unified_tools[:-1] + [
        {**unified_tools[-1], "cache_control": {"type": "ephemeral"}}
    ] if unified_tools else unified_tools

    client = anthropic.Anthropic()
    result = TaskResult(task_id=task["id"], backend="livefolders-unified-cached")
    messages = [{"role": "user", "content": task["user_turn"]}]

    for turn_idx in range(MAX_TURNS):
        response = client.messages.create(
            model=MODEL,
            max_tokens=1024,
            system=[{"type": "text", "text": NATIVE_SYSTEM_PROMPT,
                      "cache_control": {"type": "ephemeral"}}],
            tools=cached_tools,
            messages=messages,
        )
        _record_usage(result, turn_idx, response.usage)
        messages.append({"role": "assistant", "content": response.content})

        text_parts = [b.text for b in response.content if hasattr(b, "text")]
        full_text = " ".join(text_parts)

        if response.stop_reason == "end_turn":
            result.completed = task["stop_when"](full_text)
            break

        tool_results = []
        for block in response.content:
            if block.type != "tool_use":
                continue
            output = _exec_unified_tool(block, mount_path, dispatch, kinds)
            tool_results.append({
                "type": "tool_result",
                "tool_use_id": block.id,
                "content": output or "(no output)",
            })

        if not tool_results:
            result.completed = task["stop_when"](full_text)
            break

        messages.append({"role": "user", "content": tool_results})

    return result.to_dict()


# ── Comparison report ─────────────────────────────────────────────────────────

def print_report(results: list[dict]) -> None:
    # Group by (task_id, backend) and average across runs.
    from collections import defaultdict
    groups: dict[tuple, list] = defaultdict(list)
    for r in results:
        groups[(r["task_id"], r["backend"])].append(r)

    def avg(rs, key):
        return sum(r.get(key, 0) for r in rs) / len(rs)

    W = 96
    print("\n" + "=" * W)
    print(
        f"{'Task':<20} {'Backend':<20} {'Input':>8} {'Output':>8} "
        f"{'CacheR':>7} {'CacheW':>7} {'Total':>8} {'Effective':>10}  {'Done':>6}"
    )
    print("-" * W)

    by_task: dict[str, dict] = {}
    for (task_id, backend), rs in sorted(groups.items()):
        by_task.setdefault(task_id, {})[backend] = {
            "total_tokens": avg(rs, "total_tokens"),
            "effective_tokens": avg(rs, "effective_tokens"),
            "total_input_tokens": avg(rs, "total_input_tokens"),
            "total_output_tokens": avg(rs, "total_output_tokens"),
            "total_cache_read_tokens": avg(rs, "total_cache_read_tokens"),
            "total_cache_write_tokens": avg(rs, "total_cache_write_tokens"),
        }
        completed_rate = sum(1 for r in rs if r["completed"]) / len(rs)
        status = f"{completed_rate:.0%}"
        r = by_task[task_id][backend]
        print(
            f"{task_id:<20} {backend:<20} "
            f"{r['total_input_tokens']:>8.0f} {r['total_output_tokens']:>8.0f} "
            f"{r['total_cache_read_tokens']:>7.0f} "
            f"{r['total_cache_write_tokens']:>7.0f} "
            f"{r['total_tokens']:>8.0f} {r['effective_tokens']:>10.0f}  {status:>6}"
        )

    print("=" * W)

    lf_variants = ["livefolders", "livefolders-manifest", "livefolders-v2", "livefolders-cached",
                   "livefolders-native", "livefolders-unified", "livefolders-unified-cached"]
    print("\nEffective-token delta vs MCP (negative = LF cheaper)\n")
    for task_id, backends in sorted(by_task.items()):
        if "mcp" not in backends:
            continue
        mcp_eff = backends["mcp"]["effective_tokens"]
        for variant in lf_variants:
            if variant not in backends:
                continue
            lf_eff = backends[variant]["effective_tokens"]
            delta = lf_eff - mcp_eff
            pct = (delta / mcp_eff * 100) if mcp_eff else float("nan")
            print(f"  {task_id:<20} {variant:<20}  Δ {delta:+7.0f}  ({pct:+.1f}%)")
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
    parser.add_argument(
        "--cache",
        action="store_true",
        help="Also run an optimised LF variant: short system prompt + prompt caching",
    )
    parser.add_argument(
        "--manifest",
        action="store_true",
        help="Also run livefolders-manifest variant: system prompt read from mount's system_prompt.md",
    )
    parser.add_argument(
        "--v2",
        action="store_true",
        help="Also run livefolders-v2 variant: minimal prompt that explicitly lists search endpoint",
    )
    parser.add_argument(
        "--native",
        action="store_true",
        help="Also run livefolders-native variant: LF endpoints as native Anthropic tools (no bash tool)",
    )
    parser.add_argument(
        "--unified",
        action="store_true",
        help="Also run livefolders-unified: one tool per namespace with action enum (smaller schema)",
    )
    parser.add_argument(
        "--unified-cached",
        action="store_true",
        dest="unified_cached",
        help="Also run livefolders-unified-cached: unified tool + prompt caching on schemas",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=1,
        help="Number of times to repeat each task (results are averaged; default: 1)",
    )
    args = parser.parse_args()

    try:
        weave.init(WEAVE_PROJECT)
    except Exception as e:
        print(f"[warn] Weave init failed ({e}); traces will not be recorded")

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

    manifest_prompt = read_system_prompt_from_mount(args.mount) if run_lf and args.manifest else None
    if args.manifest and manifest_prompt is None:
        print(f"[warn] system_prompt.md not found at {args.mount} — skipping manifest variant")

    for task in tasks:
        print(f"[task:{task['id']}] {task['description']}")
        for run_i in range(args.runs):
            if args.runs > 1:
                print(f"  run {run_i + 1}/{args.runs}")

            if run_lf:
                r = run_livefolders_task(task, args.mount, use_cache=False)
                results.append(r)
                print(f"  livefolders        → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and manifest_prompt:
                r = run_livefolders_task(task, args.mount, system_prompt_override=manifest_prompt)
                results.append(r)
                print(f"  livefolders-manifest → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and args.v2:
                r = run_livefolders_task(task, args.mount, v2=True)
                results.append(r)
                print(f"  livefolders-v2       → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and args.native:
                r = run_livefolders_native_task(task, args.mount)
                results.append(r)
                print(f"  livefolders-native   → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and args.unified:
                r = run_livefolders_unified_task(task, args.mount)
                results.append(r)
                print(f"  livefolders-unified  → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and args.unified_cached:
                r = run_livefolders_unified_cached_task(task, args.mount)
                results.append(r)
                print(f"  livefolders-unified-cached → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_lf and args.cache:
                r = run_livefolders_task(task, args.mount, use_cache=True)
                results.append(r)
                print(f"  livefolders-cached → {r['total_tokens']} tokens, completed={r['completed']}")

            if run_mcp:
                import asyncio
                r = asyncio.run(run_mcp_task(task, args.mcp_cmd))
                results.append(r)
                print(f"  mcp                → {r['total_tokens']} tokens, completed={r['completed']}")

            time.sleep(0.5)  # avoid rate-limit bursts

    # Write raw results
    out_path = Path(__file__).parent / "results.json"
    out_path.write_text(json.dumps(results, indent=2))
    print(f"\nRaw results written to {out_path}")

    print_report(results)
    print(f"Traces available at: https://wandb.ai/{WEAVE_PROJECT}")


if __name__ == "__main__":
    main()
