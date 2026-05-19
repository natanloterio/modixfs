## LiveFoldersFS

- Handler stdout → returned to LLM as file content
- Handler stderr → written to ~/.local/share/livefolders/livefolders.log
- Timeout errors surface as a string returned to the LLM ("handler timed out")
- No structured tracing — logs are raw stderr lines
- `livefolders doctor` catches config errors before mount

## MCP

- Server can log to stderr (surfaced by Claude Code in debug mode)
- Errors returned as structured MCP error objects (code + message)
- Python exceptions → MCP error response automatically via FastMCP
- No distributed tracing built-in; add manually via logging/OpenTelemetry

## Verdict

MCP has a slight edge: structured error objects are more actionable than raw log lines.
LiveFoldersFS's `doctor` command compensates for config-time errors.
