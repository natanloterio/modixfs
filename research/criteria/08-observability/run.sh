#!/usr/bin/env bash
CRITERION="08-observability"
echo "[$CRITERION]"
echo "  LiveFoldersFS: stderr → log file; errors returned as plain text to LLM"
echo "  MCP: structured error objects; Python exceptions auto-converted"
echo "  Winner: MCP (marginal) — structured errors easier to handle programmatically"
