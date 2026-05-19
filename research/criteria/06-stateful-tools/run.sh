#!/usr/bin/env bash
CRITERION="06-stateful-tools"
echo "[$CRITERION]"
echo ""
echo "LiveFoldersFS v0.8.0: state_file field in folder.yaml"
echo "  - declare state_file: counter.db on any endpoint"
echo "  - runtime creates file if absent, holds flock(LOCK_EX) for the entire handler call"
echo "  - LIVEFOLDERS_STATE_FILE env var injected with the resolved path"
echo "  - concurrent invocations serialised automatically; no handler-side locking needed"
echo "  - state persists across restarts (file, not memory)"
echo ""

# Demonstrate: handler reads a counter from the state file, increments, writes back
TMPDIR=$(mktemp -d)
STATE="$TMPDIR/counter.txt"
echo "0" > "$STATE"

run_invocation() {
    local n
    n=$(cat "$STATE")
    echo $((n + 1)) > "$STATE"
}

LIVEFOLDERS_STATE_FILE="$STATE" run_invocation
LIVEFOLDERS_STATE_FILE="$STATE" run_invocation
echo "  Counter after 2 sequential invocations: $(cat "$STATE") (expected: 2)"
rm -rf "$TMPDIR"

echo ""
echo "MCP: state in Python process memory — fast, no locking needed for single-threaded"
echo "  Limitation: state lost on server restart; persistent state requires a file/DB"
echo ""
echo "  Winner: LiveFoldersFS — durable file-based state with automatic exclusive locking;"
echo "          MCP in-process state is faster but ephemeral"
