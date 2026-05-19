#!/usr/bin/env bash
CRITERION="worked-example (users REST API)"
cd "$(dirname "$0")"
echo "[$CRITERION]"

lf_lines=$(grep -cve '^\s*$\|^\s*#' livefolders/folder.yaml 2>/dev/null || echo 0)
mcp_lines=$(grep -cve '^\s*$\|^\s*#' mcp/server.py 2>/dev/null || echo 0)

echo "  LiveFoldersFS: ${lf_lines} lines (folder.yaml)"
echo "  MCP (Python):  ${mcp_lines} lines (server.py)"
echo ""
echo "  LiveFoldersFS install: livefolders install github.com/natanloterio/LiveFolders/tree/master/examples/users"
echo "  MCP install: pip install mcp httpx && configure server in claude_desktop_config.json"
echo ""
echo "  Winner: LiveFoldersFS — zero-dependency install vs multi-step MCP setup"
