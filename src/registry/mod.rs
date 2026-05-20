mod session;
mod tool;

pub use session::Session;
pub use tool::{Tool, ToolResult};

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn unregister(&mut self, name: &str) {
        self.tools.remove(name);
    }

    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Generates a compact LLM system prompt that inlines every tool's endpoints
    /// so the model never needs to read `how_to.md` or `index.md` at runtime.
    /// Synthesized fresh on each read so hot-reloaded tools are always current.
    /// Hidden endpoints (spec.hidden == true) are excluded from the output.
    pub fn system_prompt(&self, mount_path: &str, tools_dir: Option<&Path>) -> String {
        use crate::manifest::{FileKind, Manifest};

        let mut out = format!(
            "Bash tools at {mount}/tools/. cat = read-invoke; echo X > path && cat path = write-invoke. Return only the final answer.\n",
            mount = mount_path,
        );

        for name in self.list() {
            let Some(tool) = self.get(name) else { continue };
            out.push_str(&format!("\n{} — {}\n", name, tool.description()));

            let manifest = tools_dir
                .and_then(|d| Manifest::load(&d.join(name)).ok().flatten());

            if let Some(ref m) = manifest {
                for spec in &m.files {
                    if spec.hidden {
                        continue;
                    }
                    let desc_suffix = spec.description.as_deref()
                        .map(|d| format!("  # {}", d))
                        .unwrap_or_default();
                    match spec.kind {
                        FileKind::ReadInvoke => out.push_str(&format!(
                            "  cat {mount}/tools/{name}/{ep}{desc}\n",
                            mount = mount_path, name = name, ep = spec.name, desc = desc_suffix,
                        )),
                        FileKind::WriteInvoke => out.push_str(&format!(
                            "  echo NAME > {mount}/tools/{name}/{ep} && cat {mount}/tools/{name}/{ep}{desc}\n",
                            mount = mount_path, name = name, ep = spec.name, desc = desc_suffix,
                        )),
                        FileKind::Passthrough | FileKind::Readonly => {}
                    }
                }
            } else {
                // Fallback for built-in tools: list endpoints without kind detail.
                for ep in tool.endpoints() {
                    out.push_str(&format!(
                        "  {mount}/tools/{name}/{ep}\n",
                        mount = mount_path, name = name, ep = ep,
                    ));
                }
            }
        }
        out
    }

    pub fn root_index(&self) -> String {
        let mut lines = vec!["# LiveFolders Tools\n".to_string()];
        for name in self.list() {
            if let Some(tool) = self.get(name) {
                lines.push(format!("- **{}** — {}", name, tool.description()));
            }
        }
        lines.push("\nRead `<tool>/how_to.md` to learn how to invoke each tool.".to_string());
        lines.join("\n")
    }
}
