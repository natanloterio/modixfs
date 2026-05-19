#!/usr/bin/env bash
CRITERION="05-security"
echo "[$CRITERION]"
echo "  Both: run as user process, no OS sandboxing"
echo "  LiveFoldersFS: shell handler = injection risk if input not sanitized in handler"
echo "  MCP: schema validation provides input sanitization layer"
echo "  Winner: MCP (marginal) — schema reduces injection surface"
