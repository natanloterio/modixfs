mod session;
mod tool;

pub use session::Session;
pub use tool::{Tool, ToolResult};

use std::collections::HashMap;
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

    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tools.keys().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    pub fn root_index(&self) -> String {
        let mut lines = vec!["# ModixFS Tools\n".to_string()];
        for name in self.list() {
            if let Some(tool) = self.get(name) {
                lines.push(format!("- **{}** — {}", name, tool.description()));
            }
        }
        lines.push("\nRead `<tool>/how_to.md` to learn how to invoke each tool.".to_string());
        lines.join("\n")
    }
}
