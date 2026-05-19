# AgentFS — Hot Reload

**Evidence source:** https://github.com/tursodatabase/agentfs README
**Rating:** — N/A

AgentFS stores agent data, not tool definitions or executable handlers; there is nothing analogous to a plugin or tool binary that could be hot-reloaded. The FUSE mount reflects the current SQLite state and changes to files on the mount are persisted immediately, but this is data persistence, not capability registration. Hot reload of tool implementations is not a concept that applies to AgentFS's storage-layer role.
