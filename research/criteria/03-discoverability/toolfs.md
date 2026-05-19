# ToolFS — Discoverability

**Evidence source:** `skills/SKILL.md`, `skill_doc.go`, `skill_api.go`
**Rating:** ~ partial

ToolFS has a deliberate discoverability mechanism: each skill can expose a `SKILL.md` document (YAML front-matter + Markdown body) that describes its name, description, available operations, and usage examples; these are surfaced under the `/toolfs/skills/<name>` virtual path and aggregated by `SkillDocumentManager`. An LLM agent can `LIST /toolfs/skills` to enumerate available skills and `READ /toolfs/skills/<name>/SKILL.md` to retrieve per-skill documentation. The mechanism is filesystem-native and works without any structured schema (no JSON Schema, no OpenAPI), which makes it human-readable but less machine-parseable; there is no formal capability advertisement format that LLM tool-use APIs can consume natively.
