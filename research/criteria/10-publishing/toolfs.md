# ToolFS — Publishing

**Evidence source:** `go.mod`, `examples/skill_registration_example.go`, GitHub README
**Rating:** ~ partial

ToolFS is distributed as a standard Go module (`go get github.com/IceWhaleTech/toolfs`), so a skill author can publish a custom `SkillExecutor` as any ordinary Go package and have consumers import it. There is no dedicated skill registry, marketplace, or discovery index — users must know the package import path in advance. Filesystem-based skills (directories with SKILL.md) can be distributed as zip archives or Git repos and loaded via `LoadSkillsFromDirectory`, but the project provides no install command, no versioning convention for skill directories, and no signature or integrity verification for loaded skill code.
