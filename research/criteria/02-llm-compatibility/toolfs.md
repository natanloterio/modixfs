# ToolFS — LLM Compatibility

**Evidence source:** GitHub README, `skills/SKILL.md`, `toolfs.go`
**Rating:** ~ partial

ToolFS exposes tools as virtual filesystem paths (e.g., `GET /toolfs/memory/<id>`, `GET /toolfs/rag/query?text=...`) rather than as MCP tool definitions or OpenAI function-call schemas, so any LLM host that can read files or issue HTTP-style path requests can use it in principle. However, there is no built-in MCP server, no OpenAI function-call schema export, and no Anthropic tool-use adapter; integration with hosted LLM APIs requires the calling agent framework to translate path-based operations into the LLM's native tool protocol. The repository description mentions "flexible MCP/tool chaining" but provides no concrete MCP adapter code or documentation, leaving actual MCP compatibility as aspirational rather than implemented.
