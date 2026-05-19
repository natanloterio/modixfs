# AgentFS — LLM Compatibility

**Evidence source:** https://github.com/tursodatabase/agentfs README, examples/ (Mastra, Claude Agent SDK, OpenAI Agents)
**Rating:** ~ partial

AgentFS ships examples for Mastra, Anthropic Claude Agent SDK, and OpenAI Agents SDK, demonstrating it is LLM-framework-agnostic at the SDK level. However, AgentFS does not expose tools to the LLM directly; instead, the agent code calls `agent.tools.record(...)` to log tool invocations after the fact. No MCP adapter, OpenAI function schema, or Anthropic tool-use manifest is generated automatically, so an LLM cannot discover AgentFS capabilities through its native tool protocol. Compatibility is therefore broad in the sense that any LLM framework can use the SDK for storage, but narrow in that the LLM itself receives no structured tool definitions from AgentFS.
