import httpx
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("users")

@mcp.tool()
def list_users() -> str:
    """Fetches all users from the JSONPlaceholder API."""
    response = httpx.get("https://jsonplaceholder.typicode.com/users")
    response.raise_for_status()
    users = response.json()
    lines = ["# Users", ""]
    for u in users:
        lines.append(f"## {u['name']}")
        lines.append(f"ID: {u['id']}")
        lines.append(f"Email: {u['email']}")
        lines.append("")
    return "\n".join(lines)

if __name__ == "__main__":
    mcp.run()
