# AgentFS — I/O Expressiveness

**Evidence source:** https://github.com/tursodatabase/agentfs README, SDK TypeScript source
**Rating:** ~ partial

AgentFS exposes three I/O surfaces: a POSIX-like filesystem (read/write/readdir on arbitrary paths), a key-value store (get/set arbitrary JSON values), and a tool-call audit log (record start/end/inputs/outputs). This covers binary blobs, structured JSON state, and event streams well. However, streaming output (e.g., reading partial results as a tool executes) is not addressed — tool calls are recorded atomically after completion. There is also no mechanism for passing large inputs by reference across agent boundaries or signalling progress mid-execution. The I/O model is expressive for post-hoc audit but limited for real-time, streaming, or multi-agent handoff scenarios.
