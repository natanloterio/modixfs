# AgentFS — Stateful Tools

**Evidence source:** https://github.com/tursodatabase/agentfs README, SDK tool-call recording API
**Rating:** ~ partial

AgentFS excels at recording and persisting the outcomes of stateful tool invocations: `agent.tools.record(name, start, end, inputs, outputs)` stores a complete audit entry per call in SQLite. The KV store allows agents to maintain arbitrary structured state across calls, and the filesystem layer preserves files produced by tools. What AgentFS does not do is manage the execution of stateful tools — it records results but does not orchestrate, retry, or resume long-running tool processes. An agent framework must still handle tool lifecycle; AgentFS only provides the durable memory layer beneath it.
