# ToolFS — I/O Expressiveness

**Evidence source:** `skill_api.go` (`SkillExecutor.Execute(input []byte) ([]byte, error)`), `skills.go`, `skills/SKILL.md`
**Rating:** ~ partial

The core skill interface accepts and returns raw `[]byte`, so binary data, plain text, and JSON are all technically possible at the Go level; the `SkillResponse` envelope wraps results as JSON with a typed `Result` field. In practice all documented examples exchange JSON or query strings (e.g., `?text=query&top_k=3`), and there is no explicit support for streaming responses, multipart payloads, or structured multiline text beyond embedding strings in JSON. RAG and memory results are always delivered as JSON-encoded structs, so an LLM consuming outputs natively sees a JSON blob rather than a clean text answer, requiring the agent to parse the envelope before use.
