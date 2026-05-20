#!/usr/bin/env bash
# Smoke-check in CI; full benchmark requires API keys and livefolders binary.
#
# Full usage:
#   pip install anthropic weave mcp httpx
#   export ANTHROPIC_API_KEY=...
#   export WANDB_API_KEY=...
#   bash run.sh --full
CRITERION="11-token-efficiency"
cd "$(dirname "$0")"

# Load secrets from .env if present
if [[ -f .env ]]; then
  set -a; source .env; set +a
fi

echo "[$CRITERION]"

# Collect extra args to forward to benchmark.py (everything after --full)
BENCH_ARGS=()
FULL=false
for arg in "$@"; do
  if [[ "$arg" == "--full" ]]; then FULL=true; continue; fi
  BENCH_ARGS+=("$arg")
done

if [[ "$FULL" != "true" ]]; then
  echo "  Benchmark: benchmark.py (Option A — API-level token counting via Weave)"
  echo "  Backends:  LiveFoldersFS (livefolders/folder.yaml) vs MCP (mcp/server.py)"
  echo "  Same service: both backends call the same mock API endpoint"
  echo "  Tasks:     $(python3 -c 'from tasks import TASKS; print(len(TASKS))' 2>/dev/null || echo "?") tasks defined in tasks.py"
  echo "  Run with:  bash run.sh --full"
  exit 0
fi

# ── Full benchmark ─────────────────────────────────────────────────────────────

WORK_DIR="$(mktemp -d)"
MOUNT_DIR="$WORK_DIR/mnt"
TOOLS_DIR="$(pwd)/tools"   # tools are in subdirs: tools/users/folder.yaml
CONFIG_FILE="$WORK_DIR/livefolders.yaml"

mkdir -p "$MOUNT_DIR"

# Write a minimal config pointing at the experiment's tools dir
cat > "$CONFIG_FILE" <<EOF
mount: $MOUNT_DIR
tools_dir: $TOOLS_DIR
EOF

echo "  Mounting LiveFoldersFS at $MOUNT_DIR (tools: $TOOLS_DIR) ..."
# Prefer the locally-built binary so we always run the current version.
LF_BIN="${HOME}/.cargo/bin/livefolders"
if [[ ! -x "$LF_BIN" ]]; then LF_BIN="livefolders"; fi
"$LF_BIN" mount "$MOUNT_DIR" --config "$CONFIG_FILE" --foreground &
LF_PID=$!
sleep 2  # wait for FUSE mount to settle

cleanup() {
  echo "  Unmounting $MOUNT_DIR ..."
  kill "$LF_PID" 2>/dev/null
  fusermount3 -u "$MOUNT_DIR" 2>/dev/null || true
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

if ! mountpoint -q "$MOUNT_DIR"; then
  echo "  [error] FUSE mount failed — is livefolders on PATH and FUSE3 available?"
  exit 1
fi

echo "  Running benchmark..."
python3 benchmark.py \
  --backend both \
  --manifest \
  --native \
  --mount "$MOUNT_DIR" \
  --mcp-cmd "python3 mcp/server.py" \
  "${BENCH_ARGS[@]}"
