from mcp.server.fastmcp import FastMCP

mcp = FastMCP("counter")
_count = 0

@mcp.tool()
def increment() -> int:
    """Increments and returns the counter."""
    global _count
    _count += 1
    return _count

if __name__ == "__main__":
    mcp.run()
