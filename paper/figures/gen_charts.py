"""Generate token-efficiency benchmark figures for the paper."""
import json
from collections import defaultdict
from pathlib import Path

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import matplotlib.patches as mpatches
import numpy as np

RESULTS = Path(__file__).parent.parent.parent / "research/criteria/11-token-efficiency/results.json"
OUT = Path(__file__).parent

# ── Load and average ──────────────────────────────────────────────────────────
with open(RESULTS) as f:
    data = json.load(f)

stats = defaultdict(lambda: {"total": 0, "n": 0})
for r in data:
    k = (r["task_id"], r["backend"])
    stats[k]["total"] += r["total_tokens"]
    stats[k]["n"] += 1

def avg(task, backend):
    k = (task, backend)
    return stats[k]["total"] / stats[k]["n"] if stats[k]["n"] else 0

TASKS_ORDER = [
    ("count_users",     "count\nusers"),
    ("count_composed",  "count\ncomposed"),
    ("single_user",     "single\nuser"),
    ("list_users",      "list\nusers"),
    ("cross_reference", "cross\nreference"),
    ("count_and_find",  "count &\nfind"),
    ("filter_and_count","filter &\ncount"),
    ("list_compact",    "list\ncompact"),
]

BACKENDS = [
    ("livefolders-native",          "LF-native",   "#1a7abf"),
    ("livefolders-unified",         "LF-unified",  "#4db8ff"),
    ("livefolders-manifest",        "LF-manifest", "#99d6ff"),
    ("mcp",                         "MCP",         "#e05c2a"),
]

TASK_IDS   = [t[0] for t in TASKS_ORDER]
TASK_LABELS= [t[1] for t in TASKS_ORDER]

# ── Figure 1: grouped bar — absolute token counts ────────────────────────────
fig, ax = plt.subplots(figsize=(11, 5))

n_tasks    = len(TASKS_ORDER)
n_backends = len(BACKENDS)
group_w    = 0.72
bar_w      = group_w / n_backends
x          = np.arange(n_tasks)

for i, (bid, blabel, bcolor) in enumerate(BACKENDS):
    vals = [avg(tid, bid) for tid in TASK_IDS]
    offset = (i - (n_backends - 1) / 2) * bar_w
    bars = ax.bar(x + offset, vals, bar_w * 0.92, label=blabel, color=bcolor, zorder=3)

ax.set_xticks(x)
ax.set_xticklabels(TASK_LABELS, fontsize=9)
ax.set_ylabel("Total tokens (avg, 5 runs)", fontsize=10)
ax.set_title("Token consumption per task and backend", fontsize=12, fontweight="bold")
ax.legend(fontsize=9, framealpha=0.9)
ax.yaxis.grid(True, linestyle="--", alpha=0.5, zorder=0)
ax.set_axisbelow(True)
ax.spines[["top", "right"]].set_visible(False)

plt.tight_layout()
plt.savefig(OUT / "token_absolute.pdf", bbox_inches="tight")
plt.close()
print("Wrote token_absolute.pdf")

# ── Figure 2: delta vs MCP (% cheaper) ───────────────────────────────────────
DELTA_BACKENDS = [
    ("livefolders-native",   "LF-native",   "#1a7abf"),
    ("livefolders-unified",  "LF-unified",  "#4db8ff"),
    ("livefolders-manifest", "LF-manifest", "#99d6ff"),
]

fig, ax = plt.subplots(figsize=(11, 4.5))

n_tasks    = len(TASKS_ORDER)
n_backends = len(DELTA_BACKENDS)
group_w    = 0.65
bar_w      = group_w / n_backends
x          = np.arange(n_tasks)

for i, (bid, blabel, bcolor) in enumerate(DELTA_BACKENDS):
    deltas = []
    for tid in TASK_IDS:
        mcp_avg = avg(tid, "mcp")
        lf_avg  = avg(tid, bid)
        pct = (lf_avg - mcp_avg) / mcp_avg * 100 if mcp_avg else 0
        deltas.append(pct)
    offset = (i - (n_backends - 1) / 2) * bar_w
    colors = [bcolor if d <= 0 else "#cc3333" for d in deltas]
    ax.bar(x + offset, deltas, bar_w * 0.92, label=blabel, color=colors, zorder=3)

ax.axhline(0, color="#e05c2a", linewidth=1.5, linestyle="-", label="MCP baseline", zorder=4)
ax.set_xticks(x)
ax.set_xticklabels(TASK_LABELS, fontsize=9)
ax.set_ylabel("Token delta vs MCP (%)", fontsize=10)
ax.set_title("Token efficiency relative to MCP (negative = fewer tokens than MCP)", fontsize=12, fontweight="bold")

# Custom legend — bars use per-bar colors, so build patches manually
legend_patches = [mpatches.Patch(color=c, label=l) for _, l, c in DELTA_BACKENDS]
legend_patches.append(mpatches.Patch(color="#cc3333", label="worse than MCP"))
legend_patches.append(plt.Line2D([0], [0], color="#e05c2a", linewidth=1.5, label="MCP baseline"))
ax.legend(handles=legend_patches, fontsize=9, framealpha=0.9)
ax.yaxis.grid(True, linestyle="--", alpha=0.5, zorder=0)
ax.set_axisbelow(True)
ax.spines[["top", "right"]].set_visible(False)

plt.tight_layout()
plt.savefig(OUT / "token_delta.pdf", bbox_inches="tight")
plt.close()
print("Wrote token_delta.pdf")
