from mcp.server.fastmcp import FastMCP

mcp = FastMCP("shout")

@mcp.tool()
def shout(text: str) -> str:
    """Echoes input in uppercase."""
    return text.upper()

if __name__ == "__main__":
    mcp.run()
