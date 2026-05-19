## LiveFoldersFS compatibility

Requires the LLM host to expose bash tools (Read, Write, Bash/shell).
- Claude Code: YES (has Bash tool)
- OpenAI Assistants: NO (no shell access by default)
- Gemini CLI: PARTIAL (depends on tool config)
- Custom agents with shell access: YES

## MCP compatibility

Requires the host to implement the MCP client protocol.
- Claude Code: YES (native MCP client)
- Claude.ai (web): YES (remote MCP)
- OpenAI: NO (not MCP-compatible as of 2026-05)
- Gemini: NO
- Custom agents via SDK: YES (mcp Python/TS client)

## Verdict

MCP wins for cross-host portability among MCP-adopting hosts.
LiveFoldersFS wins for any agent that has shell/filesystem access.
