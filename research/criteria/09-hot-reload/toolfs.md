# ToolFS — Hot Reload

**Evidence source:** `skill_api.go` (`SkillRegistry.RegisterCodeSkill`, `LoadSkillsFromDirectory`), `fuse_adapter.go`
**Rating:** ✗ weak

ToolFS offers no hot-reload mechanism for skills. Filesystem-based skills can be loaded at startup via `LoadSkillsFromDirectory`, and code-based skills are injected programmatically via `InjectSkill` — both are one-shot registration calls with no file-watcher or inotify integration. The FUSE adapter (`fuse_adapter.go`) builds the virtual directory tree once at mount time (`OnAdd`) with no provision for adding or replacing nodes at runtime. Reloading a changed skill requires restarting the host process and re-registering all skills from scratch; this is not documented as a limitation, but no workaround is provided.
