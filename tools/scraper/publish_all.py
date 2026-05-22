#!/usr/bin/env python3
"""
Publish all reviewed drafts to natanloterio/livefolders-tools as a monorepo.

Usage:
    python3 publish_all.py [--drafts-dir drafts/] [--min-score N] [--dry-run]

Steps:
  1. Create natanloterio/livefolders-tools on GitHub (if it doesn't exist)
  2. Clone it, copy each passing draft into its own subdir, commit & tag, push
  3. POST to the LiveFolders registry for each tool
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import httpx
import yaml

GITHUB_API = "https://api.github.com"
REGISTRY_URL = "https://registry.livefoldersfs.org"
MONOREPO_OWNER = "natanloterio"
MONOREPO_NAME = "livefolders-tools"
MONOREPO_SLUG = f"{MONOREPO_OWNER}/{MONOREPO_NAME}"

# Load .env from repo root
_env_path = Path(__file__).parent.parent.parent / ".env"
if _env_path.exists():
    for _line in _env_path.read_text().splitlines():
        _line = _line.strip()
        if _line and not _line.startswith("#") and "=" in _line:
            _k, _, _v = _line.partition("=")
            os.environ.setdefault(_k.strip(), _v.strip().strip('"').strip("'"))


def gh_token() -> str:
    result = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True)
    token = result.stdout.strip()
    if not token:
        print("ERROR: not logged in to gh. Run: gh auth login", file=sys.stderr)
        sys.exit(1)
    return token


def gh_headers(token: str) -> dict:
    return {
        "Authorization": f"Bearer {token}",
        "User-Agent": "livefolders-publish/1.0",
        "Accept": "application/vnd.github+json",
    }


def ensure_monorepo(token: str, dry_run: bool) -> str:
    """Create the monorepo if it doesn't exist. Returns the clone URL."""
    r = httpx.get(f"{GITHUB_API}/repos/{MONOREPO_SLUG}", headers=gh_headers(token), timeout=10)
    if r.status_code == 200:
        clone_url = r.json()["clone_url"]
        print(f"Monorepo already exists: https://github.com/{MONOREPO_SLUG}")
        return clone_url
    if r.status_code != 404:
        r.raise_for_status()

    print(f"Creating https://github.com/{MONOREPO_SLUG}...")
    if dry_run:
        print("  [dry-run] skipping repo creation")
        return f"https://github.com/{MONOREPO_SLUG}.git"

    r = httpx.post(
        f"{GITHUB_API}/user/repos",
        headers=gh_headers(token),
        json={
            "name": MONOREPO_NAME,
            "description": "LiveFolders tool wrappers for popular MCP servers",
            "private": False,
            "auto_init": True,
        },
        timeout=15,
    )
    r.raise_for_status()
    clone_url = r.json()["clone_url"]
    print(f"Created: {clone_url}")
    return clone_url


def load_passing_drafts(drafts_dir: Path, min_score: int) -> list[dict]:
    """Import review logic inline to avoid subprocess overhead."""
    sys.path.insert(0, str(Path(__file__).parent))
    from review import review_draft

    results = []
    for manifest in sorted(drafts_dir.rglob("folder.yaml")):
        r = review_draft(manifest)
        if r.score >= min_score and not r.errors:
            results.append({"result": r, "manifest": manifest})
    return results


def next_scoped_tag(existing_tags: list[str], prefix: str) -> str:
    pfx = f"{prefix}-v"
    versions = []
    for t in existing_tags:
        if t.startswith(pfx):
            bare = t[len(pfx):]
            parts = bare.split(".")
            if len(parts) == 3 and all(p.isdigit() for p in parts):
                versions.append(tuple(int(p) for p in parts))
    if not versions:
        return f"{prefix}-v0.1.0"
    maj, minor, patch = max(versions)
    return f"{prefix}-v{maj}.{minor}.{patch + 1}"


def run(cmd: list[str], cwd: Path, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=str(cwd), check=check, capture_output=True, text=True)


def publish_to_registry(token: str, subdir: str, dry_run: bool) -> bool:
    payload = {"token": token, "repo": MONOREPO_SLUG, "subdir": subdir}
    if dry_run:
        print(f"  [dry-run] POST {REGISTRY_URL}/api/publish {json.dumps(payload)}")
        return True
    try:
        r = httpx.post(
            f"{REGISTRY_URL}/api/publish",
            json=payload,
            headers={"User-Agent": "livefolders-publish/1.0"},
            timeout=20,
        )
        body = r.json()
        if r.status_code == 200:
            print(f"  Registered: {body.get('url', 'ok')}")
            return True
        print(f"  Registry error ({r.status_code}): {body.get('error', body)}", file=sys.stderr)
        return False
    except Exception as e:
        print(f"  Registry request failed: {e}", file=sys.stderr)
        return False


def main() -> None:
    parser = argparse.ArgumentParser(description="Publish all passing drafts to natanloterio/livefolders-tools")
    parser.add_argument("--drafts-dir", default="drafts", help="Directory containing draft subdirs")
    parser.add_argument("--min-score", type=int, default=6, help="Minimum review score to publish (default: 6)")
    parser.add_argument("--dry-run", action="store_true", help="Show what would happen without making changes")
    args = parser.parse_args()

    drafts_dir = Path(args.drafts_dir)
    if not drafts_dir.exists():
        print(f"Drafts directory not found: {drafts_dir}", file=sys.stderr)
        sys.exit(1)

    token = gh_token()

    print(f"Reviewing drafts (min-score={args.min_score})...")
    passing = load_passing_drafts(drafts_dir, args.min_score)
    if not passing:
        print("No drafts meet the minimum score. Run review.py to check scores.")
        sys.exit(0)

    print(f"\n{len(passing)} draft(s) will be published:")
    for item in passing:
        r = item["result"]
        print(f"  {r.name:30s}  score={r.score}/{10}  endpoints={r.endpoint_count}")

    print()
    clone_url = ensure_monorepo(token, args.dry_run)

    with tempfile.TemporaryDirectory() as tmp:
        clone_dir = Path(tmp) / "repo"

        print(f"\nCloning {MONOREPO_SLUG}...")
        if not args.dry_run:
            # Embed token in URL for push access
            auth_url = clone_url.replace("https://", f"https://x-access-token:{token}@")
            run(["git", "clone", auth_url, str(clone_dir)], cwd=Path(tmp))
            run(["git", "config", "user.email", "publish@livefolders"], cwd=clone_dir)
            run(["git", "config", "user.name", "LiveFolders Publisher"], cwd=clone_dir)
        else:
            clone_dir.mkdir(parents=True)

        # Get existing tags to compute next version per tool
        existing_tags: list[str] = []
        if not args.dry_run and clone_dir.exists():
            result = run(["git", "tag", "--list"], cwd=clone_dir, check=False)
            existing_tags = result.stdout.splitlines()

        published = []
        skipped = []

        for item in passing:
            r = item["result"]
            src_dir = item["manifest"].parent
            dest_subdir = clone_dir / r.name

            if dest_subdir.exists() and not args.dry_run:
                # Overwrite: remove old content then re-copy
                shutil.rmtree(dest_subdir)

            if args.dry_run:
                print(f"  [dry-run] would copy {src_dir} → {clone_dir}/{r.name}/")
                published.append(r.name)
                continue

            dest_subdir.mkdir(parents=True, exist_ok=True)
            shutil.copy2(src_dir / "folder.yaml", dest_subdir / "folder.yaml")
            published.append(r.name)

        if not args.dry_run and published:
            # Validate all folder.yamls parse correctly before committing
            for name in published:
                yaml_path = clone_dir / name / "folder.yaml"
                try:
                    yaml.safe_load(yaml_path.read_text())
                except yaml.YAMLError as e:
                    print(f"  SKIP {name}: invalid YAML: {e}", file=sys.stderr)
                    published.remove(name)
                    skipped.append(name)

            run(["git", "add", "."], cwd=clone_dir)
            status = run(["git", "status", "--porcelain"], cwd=clone_dir)
            if not status.stdout.strip():
                print("No changes to commit — all tools already up to date.")
            else:
                run(["git", "commit", "-m", f"feat: publish {len(published)} MCP tool wrapper(s)"], cwd=clone_dir)

                # Create a scoped tag for each tool
                for name in published:
                    tag = next_scoped_tag(existing_tags, name)
                    run(["git", "tag", tag], cwd=clone_dir)
                    print(f"  Tagged {tag}")

                print(f"\nPushing to https://github.com/{MONOREPO_SLUG}...")
                run(["git", "push", "--follow-tags"], cwd=clone_dir)
                print("Pushed.")

        # Register each tool in the registry
        print(f"\nRegistering {len(published)} tool(s) in the registry...")
        ok = 0
        for name in published:
            print(f"  {name}...", end=" ", flush=True)
            if publish_to_registry(token, name, args.dry_run):
                ok += 1
            else:
                skipped.append(name)

        print(f"\n{'='*50}")
        print(f"Published:  {ok}/{len(passing)}")
        if skipped:
            print(f"Skipped:    {', '.join(skipped)}")
        print(f"\nInstall any tool with:")
        print(f"  livefolders install {MONOREPO_OWNER}/<name>")
        print(f"\nOr browse: https://registry.livefoldersfs.org/{MONOREPO_OWNER}")


if __name__ == "__main__":
    main()
