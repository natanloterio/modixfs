"""MCP server for the users tool — mirrors worked-example/mcp/server.py."""
import httpx
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("users")

@mcp.tool()
def list_users() -> str:
    """Fetches all users from the mock API."""
    response = httpx.get("https://6a0b5d085aa893e1015a2d32.mockapi.io/users")
    response.raise_for_status()
    users = response.json()
    lines = ["# Users", ""]
    for u in users:
        lines.append(f"## {u['name']}")
        lines.append(f"ID: {u['id']}")
        lines.append(f"Created: {u['createdAt']}")
        lines.append("")
    return "\n".join(lines)

if __name__ == "__main__":
    mcp.run()
