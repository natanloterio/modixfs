#!/usr/bin/env bash
# Smoke-check only (no live API calls in CI).
# To run the full benchmark:
#   pip install anthropic weave mcp httpx
#   export ANTHROPIC_API_KEY=...
#   python benchmark.py --backend both --mcp-cmd "python mcp/server.py"
CRITERION="11-token-efficiency"
cd "$(dirname "$0")"
echo "[$CRITERION]"
echo "  Benchmark: benchmark.py (Option A — API-level token counting via Weave)"
echo "  Backends:  LiveFoldersFS (bash tool + FUSE mount) vs MCP (stdio server)"
echo "  Tasks:     $(python3 -c 'from tasks import TASKS; print(len(TASKS))' 2>/dev/null || echo "?") tasks defined in tasks.py"
echo "  Run with:  python benchmark.py --backend both --mcp-cmd 'python mcp/server.py'"
echo "  Results:   results.json + Weave traces at wandb.ai/livefolders-token-efficiency"
