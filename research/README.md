# LiveFoldersFS vs MCP — Research Experiments

Each subdirectory of `criteria/` is a self-contained experiment for one evaluation criterion.

## Running all experiments

```bash
bash research/run-all.sh
```

## Running one experiment

```bash
bash research/criteria/01-setup-complexity/run.sh
```

## Requirements

- `livefolders` binary on PATH
- Python 3 with `mcp` and `httpx` (`pip install mcp httpx`)
- FUSE3 (`sudo apt-get install fuse3` on Ubuntu)

## Paper

The full analysis is written up as an arXiv preprint in `paper/`.

Build the LaTeX source (PDF requires `texlive-xetex`):

```bash
bash paper/build.sh
```

The paper covers 7 systems across 10 criteria using three evidence tiers. Tier 2 structured assessments for ToolFS, AgentFS, and llm9p are in `research/criteria/*/toolfs.md`, `agentfs.md`, and `llm9p.md`.
