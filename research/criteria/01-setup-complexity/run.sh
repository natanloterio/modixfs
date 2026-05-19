#!/usr/bin/env bash
set -e
CRITERION="01-setup-complexity"
cd "$(dirname "$0")"

lf_lines=$(grep -cve '^\s*$\|^\s*#' livefolders/folder.yaml 2>/dev/null || echo 0)
mcp_lines=$(grep -cve '^\s*$\|^\s*#' mcp/server.py 2>/dev/null || echo 0)

echo "[$CRITERION]"
echo "  LiveFoldersFS: ${lf_lines} lines (folder.yaml only, no Python)"
echo "  MCP (Python):  ${mcp_lines} lines (server.py)"
echo "  Winner: $([ "$lf_lines" -lt "$mcp_lines" ] && echo 'LiveFoldersFS' || echo 'MCP')"
