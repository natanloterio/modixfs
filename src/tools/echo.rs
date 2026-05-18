use async_trait::async_trait;

use crate::registry::{Session, Tool, ToolResult};

/// Minimal tool for testing: echoes input back as output.
pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes input back. Useful for testing ModixFS is working."
    }

    fn how_to(&self) -> &str {
        r#"# echo

Write anything to `send` and read it back.

## Example

```bash
echo "hello modixfs" > /tools/echo/send
cat /tools/echo/send
# → hello modixfs
```
"#
    }

    fn endpoints(&self) -> Vec<&str> {
        vec!["send"]
    }

    async fn invoke(&self, _endpoint: &str, input: &[u8], _session: &Session) -> ToolResult {
        ToolResult::ok(input.to_vec())
    }
}
