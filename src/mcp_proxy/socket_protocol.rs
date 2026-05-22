use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ProxyRequest {
    Call { server: String, tool: String, args: Value },
    Status,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProxyResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProxyResponse {
    pub fn success(result: String) -> Self {
        Self { ok: true, result: Some(result), error: None }
    }

    pub fn failure(msg: impl Into<String>) -> Self {
        Self { ok: false, result: None, error: Some(msg.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_request_roundtrip() {
        let req = ProxyRequest::Call {
            server: "brave".to_string(),
            tool: "web_search".to_string(),
            args: serde_json::json!({"query": "hello"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: ProxyRequest = serde_json::from_str(&json).unwrap();
        match decoded {
            ProxyRequest::Call { server, tool, args } => {
                assert_eq!(server, "brave");
                assert_eq!(tool, "web_search");
                assert_eq!(args["query"], "hello");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn success_response_roundtrip() {
        let resp = ProxyResponse { ok: true, result: Some("output".to_string()), error: None };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ProxyResponse = serde_json::from_str(&json).unwrap();
        assert!(decoded.ok);
        assert_eq!(decoded.result.as_deref(), Some("output"));
    }

    #[test]
    fn error_response_roundtrip() {
        let resp = ProxyResponse { ok: false, result: None, error: Some("SPAWN: fail".to_string()) };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ProxyResponse = serde_json::from_str(&json).unwrap();
        assert!(!decoded.ok);
        assert_eq!(decoded.error.as_deref(), Some("SPAWN: fail"));
    }
}
