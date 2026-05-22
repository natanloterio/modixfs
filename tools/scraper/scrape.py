#!/usr/bin/env python3
"""
Scrape mcpservers.org and use Claude to draft folder.yaml wrappers.

Usage:
    python3 scrape.py [--limit N] [--output-dir drafts/]
    python3 scrape.py --help
"""
import argparse
import base64
import os
import sys
from pathlib import Path

# Handle --help before loading heavy dependencies so it always works
if "--help" in sys.argv or "-h" in sys.argv:
    parser = argparse.ArgumentParser(description="Draft LiveFolders MCP wrappers from mcpservers.org")
    parser.add_argument("--limit", type=int, default=20, help="Number of top servers to process")
    parser.add_argument("--output-dir", default="drafts", help="Directory for draft folder.yaml files")
    parser.parse_args()  # prints help and exits

try:
    import anthropic
    import httpx
    import yaml
    from bs4 import BeautifulSoup
except ImportError as e:
    print(f"Missing dependency: {e}", file=sys.stderr)
    print("Install with: pip install -r requirements.txt", file=sys.stderr)
    sys.exit(1)

REGISTRY_URL = "https://mcpservers.org"
GITHUB_API = "https://api.github.com"
EXAMPLES_DIR = Path(__file__).parent / "prompts" / "examples"
SYSTEM_PROMPT_PATH = Path(__file__).parent / "prompts" / "system.txt"


def github_headers() -> dict:
    token = os.environ.get("GITHUB_TOKEN", "")
    h = {"User-Agent": "livefolders-scraper/1.0"}
    if token:
        h["Authorization"] = f"Bearer {token}"
    return h


def scrape_server_list(limit: int) -> list[dict]:
    """Scrape mcpservers.org and return top servers sorted by GitHub stars."""
    print(f"Fetching {REGISTRY_URL}...")
    resp = httpx.get(REGISTRY_URL, follow_redirects=True, timeout=30)
    resp.raise_for_status()
    soup = BeautifulSoup(resp.text, "html.parser")

    servers = []
    # mcpservers.org lists servers — try several selector patterns
    for card in soup.select("a[href*='github.com']"):
        href = card.get("href", "")
        if "github.com" not in href:
            continue
        # Normalize to github.com/owner/repo
        parts = href.rstrip("/").replace("https://github.com/", "").split("/")
        if len(parts) < 2:
            continue
        github_url = f"https://github.com/{parts[0]}/{parts[1]}"
        name = parts[1]
        if any(s["github_url"] == github_url for s in servers):
            continue
        servers.append({"name": name, "description": "", "github_url": github_url, "stars": 0})

    print(f"Found {len(servers)} candidate servers. Fetching star counts...")
    headers = github_headers()
    for s in servers:
        repo = s["github_url"].replace("https://github.com/", "")
        try:
            r = httpx.get(f"{GITHUB_API}/repos/{repo}", headers=headers, timeout=10)
            if r.status_code == 200:
                data = r.json()
                s["stars"] = data.get("stargazers_count", 0)
                s["description"] = data.get("description") or ""
                s["name"] = data.get("name", s["name"])
        except Exception as e:
            print(f"  Warning: could not fetch stars for {repo}: {e}", file=sys.stderr)

    servers.sort(key=lambda x: x["stars"], reverse=True)
    return servers[:limit]


def fetch_github_file(github_url: str, filename: str) -> str:
    """Fetch a file from a GitHub repo via the API. Returns content or empty string."""
    repo = github_url.rstrip("/").replace("https://github.com/", "")
    url = f"{GITHUB_API}/repos/{repo}/contents/{filename}"
    try:
        r = httpx.get(url, headers=github_headers(), timeout=15)
        if r.status_code == 200:
            content = r.json().get("content", "")
            return base64.b64decode(content).decode("utf-8", errors="replace")[:8000]
    except Exception:
        pass
    return ""


def fetch_server_context(github_url: str) -> str:
    """Fetch README and package manifest for an MCP server."""
    parts = []
    for name in ["README.md", "readme.md", "README.txt"]:
        content = fetch_github_file(github_url, name)
        if content:
            parts.append(f"## README\n{content[:5000]}")
            break
    for name in ["package.json", "pyproject.toml"]:
        content = fetch_github_file(github_url, name)
        if content:
            parts.append(f"## {name}\n{content[:2000]}")
            break
    return "\n\n".join(parts)


def load_examples() -> str:
    """Load few-shot folder.yaml examples."""
    examples = []
    if EXAMPLES_DIR.exists():
        for f in sorted(EXAMPLES_DIR.glob("*.yaml"))[:3]:
            examples.append(f"### Example: {f.stem}\n```yaml\n{f.read_text().strip()}\n```")
    return "\n\n".join(examples)


def generate_folder_yaml(server: dict, system_prompt: str, examples: str) -> str:
    """Use Claude to generate a draft folder.yaml for an MCP server."""
    context = fetch_server_context(server["github_url"])
    user_content = (
        f"Generate a folder.yaml wrapper for this MCP server.\n\n"
        f"Name: {server['name']}\n"
        f"Description: {server['description']}\n"
        f"GitHub: {server['github_url']}\n"
        f"Stars: {server['stars']}\n\n"
        f"{context}\n\n"
        f"Few-shot examples:\n{examples}"
    )
    client = anthropic.Anthropic()
    message = client.messages.create(
        model="claude-sonnet-4-6",
        max_tokens=2048,
        system=system_prompt,
        messages=[{"role": "user", "content": user_content}],
    )
    return message.content[0].text.strip()


def main() -> None:
    parser = argparse.ArgumentParser(description="Draft LiveFolders MCP wrappers from mcpservers.org")
    parser.add_argument("--limit", type=int, default=20, help="Number of top servers to process")
    parser.add_argument("--output-dir", default="drafts", help="Directory for draft folder.yaml files")
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    system_prompt = SYSTEM_PROMPT_PATH.read_text()
    examples = load_examples()

    servers = scrape_server_list(args.limit)
    print(f"\nProcessing top {len(servers)} servers...\n")

    for i, server in enumerate(servers, 1):
        slug = server["name"].lower().replace(" ", "-").replace("/", "-").replace("_", "-")
        draft_dir = output_dir / slug
        out_path = draft_dir / "folder.yaml"

        if out_path.exists():
            print(f"[{i}/{len(servers)}] {server['name']}: already drafted, skipping")
            continue

        print(f"[{i}/{len(servers)}] {server['name']} ({server['stars']} ★)...", end=" ", flush=True)
        try:
            yaml_content = generate_folder_yaml(server, system_prompt, examples)
            draft_dir.mkdir(parents=True, exist_ok=True)
            out_path.write_text(yaml_content + "\n")
            print("done")
        except Exception as e:
            print(f"ERROR: {e}", file=sys.stderr)

    print(f"\nDrafts written to {output_dir}/")
    print("Review each draft, test it, then: livefolders publish drafts/<name>")


if __name__ == "__main__":
    main()
