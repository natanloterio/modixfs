# AgentFS — Composability

**Evidence source:** https://github.com/tursodatabase/agentfs README
**Rating:** — N/A

AgentFS does not define a composable tool or capability graph; it is a storage substrate that individual agent frameworks sit on top of. There is no mechanism for chaining AgentFS-registered tools, piping the output of one into another, or building hierarchical tool directories analogous to a UNIX pipeline. Multiple agents can each have their own `.db` file, but AgentFS provides no inter-agent composition primitives. Composability as a criterion for tool-integration systems does not apply to AgentFS's storage-focused design.
