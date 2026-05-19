#!/usr/bin/env bash
CRITERION="02-llm-compatibility"
echo "[$CRITERION]"
echo "  LiveFoldersFS: works on any host with bash/shell tool access"
echo "  MCP:           works on any host that implements MCP client protocol"
echo "  Winner: MCP for cross-host portability (MCP-native hosts); LiveFoldersFS for shell-capable agents."
