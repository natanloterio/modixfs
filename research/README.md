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
