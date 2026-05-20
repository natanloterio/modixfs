"""MCP server for the users tool — mirrors the LF users/folder.yaml endpoints."""
import httpx
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("users")
API = "https://6a0b5d085aa893e1015a2d32.mockapi.io/users"


def _fetch() -> list[dict]:
    response = httpx.get(API)
    response.raise_for_status()
    return response.json()


@mcp.tool()
def list_users() -> str:
    """Fetches all users. Returns full markdown with name, ID, created date, avatar."""
    users = _fetch()
    lines = ["# Users", ""]
    for u in users:
        lines += [f"## {u['name']}", f"ID: {u['id']}", f"Created: {u['createdAt']}", ""]
    return "\n".join(lines)


@mcp.tool()
def list_users_compact() -> str:
    """Fetches all users. Returns compact id:name lines, one per user."""
    return "\n".join(f"{u['id']}:{u['name']}" for u in _fetch())


@mcp.tool()
def count_users() -> str:
    """Returns the total number of users as a plain integer."""
    return str(len(_fetch()))


@mcp.tool()
def search_user(name: str) -> str:
    """Find users by name substring (case-insensitive). Returns id:name lines. Use for lookup or bulk filtering."""
    users = _fetch()
    matches = [u for u in users if name.lower() in u['name'].lower()]
    return "\n".join(f"{u['id']}:{u['name']}" for u in matches)


@mcp.tool()
def get_user(user_id: str) -> str:
    """Fetch full details for one user by ID. Returns name, id, createdAt, avatar."""
    response = httpx.get(f"{API}/{user_id}")
    if response.status_code == 404:
        return "not found"
    response.raise_for_status()
    u = response.json()
    return f"name: {u['name']}\nid: {u['id']}\ncreatedAt: {u['createdAt']}\navatar: {u['avatar']}"


@mcp.tool()
def filter_users(query: str) -> str:
    """Filter users by name substring. Returns 'count: N' on first line, then matching id:name pairs. Use for aggregate/filter tasks."""
    users = _fetch()
    matches = [u for u in users if query.lower() in u['name'].lower()]
    lines = [f"count: {len(matches)}"] + [f"{u['id']}:{u['name']}" for u in matches]
    return "\n".join(lines)


if __name__ == "__main__":
    mcp.run()
