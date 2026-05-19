#!/usr/bin/env bash
CRITERION="03-discoverability"
echo "[$CRITERION]"

echo ""
echo "--- LiveFoldersFS: LLM reads index.md (plain text) ---"
cat << 'EOF'
# Tools

## shout
Echoes input in uppercase.

Files: shout
EOF

echo ""
echo "--- MCP: LLM receives list_tools JSON response ---"
cat << 'EOF'
{
  "tools": [
    {
      "name": "shout",
      "description": "Echoes input in uppercase.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "text": {"type": "string"}
        },
        "required": ["text"]
      }
    }
  ]
}
EOF

echo ""
echo "  LiveFoldersFS: human-readable markdown, LLM reads it naturally"
echo "  MCP: structured JSON schema, enables parameter validation"
echo "  Winner: Tie — MCP wins on schema strictness; LiveFoldersFS wins on readability"
