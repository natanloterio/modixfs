from mcp.server.fastmcp import FastMCP

mcp = FastMCP("reflect")

@mcp.tool()
def reflect(text: str) -> str:
    """Reflects back whatever text is passed."""
    return text

@mcp.tool()
def json_reflect(data: dict) -> dict:
    """Reflects back a JSON object."""
    return data

if __name__ == "__main__":
    mcp.run()
