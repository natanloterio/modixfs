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
        "Echoes input back. Useful for testing LiveFolders is working."
    }

    fn how_to(&self) -> &str {
        r#"# echo

Write anything to `send` and read it back.

## Example

```bash
echo "hello livefolders" > /tools/echo/send
cat /tools/echo/send
# → hello livefolders
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echoes_input_back() {
        let session = Session::new();
        let result = EchoTool.invoke("send", b"hello livefolders", &session).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"hello livefolders");
    }

    #[tokio::test]
    async fn echoes_empty_input() {
        let session = Session::new();
        let result = EchoTool.invoke("send", b"", &session).await;
        assert!(!result.is_error());
        assert_eq!(result.output, b"");
    }

    #[tokio::test]
    async fn name_and_endpoints() {
        assert_eq!(EchoTool.name(), "echo");
        assert_eq!(EchoTool.endpoints(), vec!["send"]);
    }
}
