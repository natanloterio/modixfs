# ToolFS — Setup Complexity

**Evidence source:** GitHub README, `examples/skill_registration_example.go`, `skill_api.go`
**Rating:** ~ partial

To ship a working tool, a developer must implement the `SkillExecutor` Go interface (four methods: `Name`, `Version`, `Init`, `Execute`) and then call `executorManager.InjectSkill()` on a running `ToolFS` instance — roughly 30–50 lines of Go plus wiring into the host process. Filesystem-based skills (the lighter path) require only a directory with a SKILL.md header; no Go code is needed, but the skill can only serve documentation and static scripts, not live computation. There is no scaffolding CLI or template generator, so the developer must manually replicate the directory layout shown in the example. Overall the entry bar is moderate for Go developers but nontrivial for those outside the Go ecosystem.
