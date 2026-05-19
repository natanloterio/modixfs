# AgentFS — Security

**Evidence source:** https://github.com/tursodatabase/agentfs README (FAQ: isolation section), agentfs run sandbox description
**Rating:** ~ partial

AgentFS provides filesystem-level copy-on-write isolation through its overlay approach: an agent's writes go into its own SQLite database and cannot affect other agents' databases by design. The `agentfs run` sandbox mounts the agent filesystem at `/agent` inside a subprocess, constraining writes to that path. However, the sandbox does not enforce network isolation, capability dropping, or seccomp filtering — it is explicitly described as "experimental". The FUSE mount itself runs with user-level privileges, so the underlying SQLite file must be protected by standard OS permissions. Overall, isolation is meaningful for filesystem state but incomplete as a security boundary against malicious agents.
