# AgentFS — Discoverability

**Evidence source:** https://github.com/tursodatabase/agentfs README, SPEC.md
**Rating:** — N/A

AgentFS is a state-storage system, not a tool-invocation interface; it does not expose callable tools or a schema that an LLM can enumerate at runtime. The mounted FUSE filesystem contains the agent's own data files (output, KV state, audit log), not tool definitions. There is no manifest, directory listing, or protocol message that advertises available capabilities to a model. Discoverability is not applicable to AgentFS's design goals.
