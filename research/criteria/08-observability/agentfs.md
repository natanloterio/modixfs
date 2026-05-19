# AgentFS — Observability

**Evidence source:** https://github.com/tursodatabase/agentfs README (`agentfs timeline` command, SPEC.md audit schema)
**Rating:** ✓ strong

Observability is AgentFS's primary design goal. Every file operation and tool call is written to a structured SQLite audit table, queryable with arbitrary SQL after the fact. The `agentfs timeline <agent>` CLI command renders a human-readable chronological log of tool invocations with name, status, duration, and timestamp. Because all state lives in a single portable `.db` file, the entire agent history can be inspected offline, shared, or ingested by external analytics tools. This is the strongest observability story of any system surveyed: the audit trail is comprehensive, persistent, and directly queryable without any additional infrastructure.
