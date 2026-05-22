#!/usr/bin/env python3
"""
Validate and score draft folder.yaml files before publishing.

Usage:
    python3 review.py [--drafts-dir drafts/] [--min-score N] [--json]
"""
import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

# Load .env from repo root
_env_path = Path(__file__).parent.parent.parent / ".env"
if _env_path.exists():
    for _line in _env_path.read_text().splitlines():
        _line = _line.strip()
        if _line and not _line.startswith("#") and "=" in _line:
            _k, _, _v = _line.partition("=")
            os.environ.setdefault(_k.strip(), _v.strip().strip('"').strip("'"))

try:
    import httpx
    import yaml
except ImportError as e:
    print(f"Missing dependency: {e}", file=sys.stderr)
    print("Install with: pip install -r requirements.txt", file=sys.stderr)
    sys.exit(1)

MAX_SCORE = 10


@dataclass
class ReviewResult:
    name: str
    path: Path
    errors: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    score: int = 0
    endpoint_count: int = 0
    pkg_ok: bool | None = None  # None = not checked
    has_env: bool = False
    command: str = ""
    description: str = ""


def extract_npm_package(args: list) -> str | None:
    """Extract npm package name from npx args like ['-y', '@scope/pkg', ...]."""
    for a in args:
        if isinstance(a, str) and not a.startswith("-"):
            return a
    return None


def check_npm_package(package: str) -> bool:
    encoded = package.replace("/", "%2F")
    try:
        r = httpx.get(
            f"https://registry.npmjs.org/{encoded}",
            timeout=8,
            follow_redirects=True,
            headers={"Accept": "application/json"},
        )
        return r.status_code == 200
    except Exception:
        return False


def check_pypi_package(package: str) -> bool:
    try:
        r = httpx.get(f"https://pypi.org/pypi/{package}/json", timeout=8)
        return r.status_code == 200
    except Exception:
        return False


def review_draft(folder_yaml: Path) -> ReviewResult:
    result = ReviewResult(name=folder_yaml.parent.name, path=folder_yaml)

    # 1. YAML parse
    try:
        data = yaml.safe_load(folder_yaml.read_text())
    except yaml.YAMLError as e:
        result.errors.append(f"YAML parse error: {e}")
        return result

    if not isinstance(data, dict):
        result.errors.append("YAML root must be a mapping")
        return result

    # 2. Required fields
    for f in ("name", "description", "files"):
        if not data.get(f):
            result.errors.append(f"Missing required field: {f}")

    mcp = data.get("mcp", {})
    if not isinstance(mcp, dict):
        result.errors.append("mcp: must be a mapping")
        mcp = {}
    for f in ("server", "command", "args"):
        if not mcp.get(f):
            result.errors.append(f"Missing mcp.{f}")

    if result.errors:
        return result

    result.description = data.get("description", "")
    result.command = mcp.get("command", "")
    result.has_env = bool(data.get("env"))

    # 3. Endpoint check
    files = data.get("files", [])
    tool_files = [f for f in files if isinstance(f, dict) and f.get("type") != "readonly"]
    result.endpoint_count = len(tool_files)

    if result.endpoint_count == 0:
        result.errors.append("No tool endpoints defined")
        return result

    # 4. Handler format check
    for f in tool_files:
        handler = f.get("handler", "")
        if not handler:
            result.warnings.append(f"Endpoint '{f.get('name')}' has no handler")
            continue
        if not re.match(r"livefolders mcp call \S+ \S+", handler):
            result.warnings.append(
                f"Endpoint '{f.get('name')}' handler doesn't follow "
                f"'livefolders mcp call <server> <tool>' pattern: {handler!r}"
            )

    # 5. Package existence check
    args = mcp.get("args", [])
    cmd = mcp.get("command", "")
    if cmd == "npx":
        pkg = extract_npm_package(args)
        if pkg:
            result.pkg_ok = check_npm_package(pkg)
            if not result.pkg_ok:
                result.warnings.append(f"npm package not found: {pkg}")
        else:
            result.warnings.append("Could not extract npm package name from mcp.args")
    elif cmd in ("uvx", "pipx", "python", "python3"):
        # Try to extract package from args
        for a in args:
            if isinstance(a, str) and not a.startswith("-") and not a.endswith(".py"):
                result.pkg_ok = check_pypi_package(a)
                if not result.pkg_ok:
                    result.warnings.append(f"PyPI package not found: {a}")
                break

    # 6. Score (out of MAX_SCORE)
    score = 0

    # Base: parses and has required fields (+2)
    score += 2

    # Package verified on registry (+3)
    if result.pkg_ok is True:
        score += 3
    elif result.pkg_ok is None:
        score += 1  # non-npm/pypi, can't verify but don't penalise

    # Endpoint count
    if result.endpoint_count >= 5:
        score += 2
    elif result.endpoint_count >= 2:
        score += 1

    # Descriptions on endpoints (+1)
    described = sum(1 for f in tool_files if f.get("input", {}) and f["input"].get("description"))
    if described == len(tool_files) and len(tool_files) > 0:
        score += 1

    # has_env declared (+1 if env present and makes sense for an API-backed service)
    if result.has_env:
        score += 1

    # No warnings (+1 bonus)
    if not result.warnings:
        score += 1

    result.score = min(score, MAX_SCORE)
    return result


def fmt_bool(v: bool | None) -> str:
    if v is True:
        return "✓"
    if v is False:
        return "✗"
    return "~"


def fmt_score(score: int) -> str:
    bar = "█" * score + "░" * (MAX_SCORE - score)
    return f"{score:2d}/{MAX_SCORE} [{bar}]"


def main() -> None:
    parser = argparse.ArgumentParser(description="Review draft folder.yaml files")
    parser.add_argument("--drafts-dir", default="drafts", help="Directory containing draft subdirs")
    parser.add_argument("--min-score", type=int, default=0, help="Only show drafts with score >= N")
    parser.add_argument("--json", action="store_true", help="Output JSON")
    args = parser.parse_args()

    drafts_dir = Path(args.drafts_dir)
    if not drafts_dir.exists():
        print(f"Drafts directory not found: {drafts_dir}", file=sys.stderr)
        sys.exit(1)

    manifests = sorted(drafts_dir.rglob("folder.yaml"))
    if not manifests:
        print("No folder.yaml files found.", file=sys.stderr)
        sys.exit(1)

    print(f"Reviewing {len(manifests)} drafts...\n", file=sys.stderr)

    results = []
    for m in manifests:
        sys.stderr.write(f"  {m.parent.name}... ")
        sys.stderr.flush()
        r = review_draft(m)
        results.append(r)
        sys.stderr.write("done\n")

    results.sort(key=lambda r: r.score, reverse=True)

    if args.json:
        out = []
        for r in results:
            out.append({
                "name": r.name,
                "score": r.score,
                "endpoint_count": r.endpoint_count,
                "pkg_ok": r.pkg_ok,
                "has_env": r.has_env,
                "command": r.command,
                "description": r.description,
                "errors": r.errors,
                "warnings": r.warnings,
                "path": str(r.path),
            })
        print(json.dumps(out, indent=2))
        return

    # Table output
    col_name = max(len(r.name) for r in results) + 2
    header = (
        f"{'Name':<{col_name}} {'Score':>18}  {'Endpoints':>9}  {'Pkg':>3}  {'Env':>3}  {'Cmd':<8}  Description"
    )
    print(header)
    print("-" * len(header))

    shown = 0
    for r in results:
        if r.score < args.min_score:
            continue
        shown += 1
        status = fmt_score(r.score)
        pkg = fmt_bool(r.pkg_ok)
        env = "✓" if r.has_env else " "
        desc = r.description[:50] + ("…" if len(r.description) > 50 else "")
        print(f"{r.name:<{col_name}} {status}  {r.endpoint_count:>9}  {pkg:>3}  {env:>3}  {r.command:<8}  {desc}")

        for e in r.errors:
            print(f"  {'':>{col_name}} {'':>18}  ERROR: {e}")
        for w in r.warnings:
            print(f"  {'':>{col_name}} {'':>18}  warn:  {w}")

    print(f"\n{shown}/{len(results)} drafts shown (min-score={args.min_score})")
    ready = sum(1 for r in results if r.score >= 6 and not r.errors)
    print(f"{ready} ready to publish (score ≥ 6, no errors)")


if __name__ == "__main__":
    main()
