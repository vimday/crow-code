//! JSON-RPC 2.0 protocol types for the Crow IDE integration server.
//!
//! This module defines the wire-level protocol used by `crow serve` to
//! communicate with IDE extensions and other tooling over stdin/stdout
//! or a TCP socket. All types follow the JSON-RPC 2.0 specification.

use serde::{Deserialize, Serialize};

// ─── JSON-RPC 2.0 Wire Types ──────────────────────────────────────

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

impl JsonRpcRequest {
    /// Validate that this is a valid JSON-RPC 2.0 request.
    pub fn validate(&self) -> Result<(), JsonRpcError> {
        if self.jsonrpc != "2.0" {
            return Err(JsonRpcError::invalid_request("jsonrpc must be \"2.0\""));
        }
        if self.method.is_empty() {
            return Err(JsonRpcError::invalid_request("method must not be empty"));
        }
        Ok(())
    }
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    /// Construct a success response with the given result payload.
    pub fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Construct an error response.
    pub fn error(id: Option<serde_json::Value>, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcError {
    /// Parse error (-32700): Invalid JSON was received.
    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: msg.into(),
            data: None,
        }
    }

    /// Invalid request (-32600): The JSON sent is not a valid request object.
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: msg.into(),
            data: None,
        }
    }

    /// Method not found (-32601): The method does not exist or is not available.
    pub fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {method}"),
            data: None,
        }
    }

    /// Invalid params (-32602): Invalid method parameter(s).
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
            data: None,
        }
    }

    /// Internal error (-32603): Internal JSON-RPC error.
    pub fn internal_error(msg: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: msg.into(),
            data: None,
        }
    }
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for JsonRpcError {}

// ─── Crow-Specific Method Types ───────────────────────────────────

/// Known server methods exposed by the Crow JSON-RPC server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerMethod {
    /// Submit a prompt for the agent to process.
    SubmitPrompt,
    /// Cancel the currently-running agent turn.
    CancelTurn,
    /// Query the server's current status (idle, streaming, etc.).
    GetStatus,
    /// List recent conversation history entries.
    ListHistory,
    /// Retrieve the current server configuration.
    GetConfig,
    /// Update server configuration at runtime.
    SetConfig,
    /// Health-check ping.
    Ping,
}

impl ServerMethod {
    /// Parse a method string into a known `ServerMethod`.
    pub fn from_method_str(s: &str) -> Option<Self> {
        match s {
            "crow/submitPrompt" => Some(Self::SubmitPrompt),
            "crow/cancelTurn" => Some(Self::CancelTurn),
            "crow/getStatus" => Some(Self::GetStatus),
            "crow/listHistory" => Some(Self::ListHistory),
            "crow/getConfig" => Some(Self::GetConfig),
            "crow/setConfig" => Some(Self::SetConfig),
            "crow/ping" => Some(Self::Ping),
            _ => None,
        }
    }

    /// Return the canonical method string for this method.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SubmitPrompt => "crow/submitPrompt",
            Self::CancelTurn => "crow/cancelTurn",
            Self::GetStatus => "crow/getStatus",
            Self::ListHistory => "crow/listHistory",
            Self::GetConfig => "crow/getConfig",
            Self::SetConfig => "crow/setConfig",
            Self::Ping => "crow/ping",
        }
    }
}

// ─── Typed Parameters ─────────────────────────────────────────────

/// Parameters for `crow/submitPrompt`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitPromptParams {
    /// The user prompt to submit.
    pub prompt: String,
    /// Optional model override for this turn.
    #[serde(default)]
    pub model: Option<String>,
    /// Optional workspace root override.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Parameters for `crow/cancelTurn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelTurnParams {
    /// Optional reason for cancellation (logged for diagnostics).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Response payload for `crow/getStatus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// Current agent state: `"idle"`, `"streaming"`, `"tool_executing"`.
    pub state: String,
    /// Active model identifier.
    pub model: String,
    /// Current workspace root path.
    pub workspace: String,
    /// Number of completed turns in this session.
    pub turn_count: u32,
    /// Cumulative token usage across all turns.
    pub total_tokens: u64,
    /// Server uptime in seconds.
    pub uptime_secs: u64,
}

/// A single entry in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Role: `"user"`, `"assistant"`, or `"tool"`.
    pub role: String,
    /// Message content.
    pub content: String,
    /// ISO 8601 timestamp.
    pub timestamp: String,
}

/// Parameters for `crow/listHistory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListHistoryParams {
    /// Maximum number of entries to return.
    #[serde(default = "default_limit")]
    pub limit: u32,
    /// Offset for pagination.
    #[serde(default)]
    pub offset: u32,
}

fn default_limit() -> u32 {
    50
}

// ─── Routing ──────────────────────────────────────────────────────

/// Route a JSON-RPC request to the appropriate [`ServerMethod`].
///
/// Validates the request envelope first, then resolves the method name.
pub fn route_request(request: &JsonRpcRequest) -> Result<ServerMethod, JsonRpcError> {
    request.validate()?;
    ServerMethod::from_method_str(&request.method)
        .ok_or_else(|| JsonRpcError::method_not_found(&request.method))
}

// ─── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_request(method: &str) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params: json!({}),
        }
    }

    #[test]
    fn validate_correct_jsonrpc_version() {
        let req = valid_request("crow/ping");
        assert!(req.validate().is_ok());
    }

    #[test]
    fn reject_invalid_jsonrpc_version() {
        let req = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            id: Some(json!(1)),
            method: "crow/ping".to_string(),
            params: json!({}),
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.code, -32600);
        assert!(err.message.contains("2.0"));
    }

    #[test]
    fn reject_empty_method() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: String::new(),
            params: json!({}),
        };
        let err = req.validate().unwrap_err();
        assert_eq!(err.code, -32600);
        assert!(err.message.contains("method"));
    }

    #[test]
    fn route_known_methods() {
        let cases = [
            ("crow/submitPrompt", ServerMethod::SubmitPrompt),
            ("crow/cancelTurn", ServerMethod::CancelTurn),
            ("crow/getStatus", ServerMethod::GetStatus),
            ("crow/listHistory", ServerMethod::ListHistory),
            ("crow/getConfig", ServerMethod::GetConfig),
            ("crow/setConfig", ServerMethod::SetConfig),
            ("crow/ping", ServerMethod::Ping),
        ];
        for (method_str, expected) in cases {
            let req = valid_request(method_str);
            let routed = route_request(&req).unwrap();
            assert_eq!(routed, expected, "Failed for method: {method_str}");
        }
    }

    #[test]
    fn route_unknown_method_returns_method_not_found() {
        let req = valid_request("crow/nonexistent");
        let err = route_request(&req).unwrap_err();
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("nonexistent"));
    }

    #[test]
    fn serialize_deserialize_request_roundtrip() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(42)),
            method: "crow/ping".to_string(),
            params: json!({"key": "value"}),
        };
        let serialized = serde_json::to_string(&req).unwrap();
        let deserialized: JsonRpcRequest = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.jsonrpc, "2.0");
        assert_eq!(deserialized.id, Some(json!(42)));
        assert_eq!(deserialized.method, "crow/ping");
        assert_eq!(deserialized.params["key"], "value");
    }

    #[test]
    fn serialize_deserialize_response_success_roundtrip() {
        let resp = JsonRpcResponse::success(Some(json!(1)), json!({"pong": true}));
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: JsonRpcResponse = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.jsonrpc, "2.0");
        assert_eq!(deserialized.id, Some(json!(1)));
        assert_eq!(deserialized.result.unwrap()["pong"], true);
        assert!(deserialized.error.is_none());
    }

    #[test]
    fn serialize_deserialize_response_error_roundtrip() {
        let resp =
            JsonRpcResponse::error(Some(json!(1)), JsonRpcError::internal_error("boom"));
        let serialized = serde_json::to_string(&resp).unwrap();
        let deserialized: JsonRpcResponse = serde_json::from_str(&serialized).unwrap();

        assert!(deserialized.result.is_none());
        let err = deserialized.error.unwrap();
        assert_eq!(err.code, -32603);
        assert_eq!(err.message, "boom");
    }

    #[test]
    fn error_codes_match_jsonrpc_spec() {
        assert_eq!(JsonRpcError::parse_error("x").code, -32700);
        assert_eq!(JsonRpcError::invalid_request("x").code, -32600);
        assert_eq!(JsonRpcError::method_not_found("x").code, -32601);
        assert_eq!(JsonRpcError::invalid_params("x").code, -32602);
        assert_eq!(JsonRpcError::internal_error("x").code, -32603);
    }

    #[test]
    fn submit_prompt_params_deserialization() {
        let json_str = r#"{"prompt": "fix the bug", "model": "gpt-4o"}"#;
        let params: SubmitPromptParams = serde_json::from_str(json_str).unwrap();
        assert_eq!(params.prompt, "fix the bug");
        assert_eq!(params.model.as_deref(), Some("gpt-4o"));
        assert!(params.workspace.is_none());
    }

    #[test]
    fn submit_prompt_params_minimal() {
        let json_str = r#"{"prompt": "hello"}"#;
        let params: SubmitPromptParams = serde_json::from_str(json_str).unwrap();
        assert_eq!(params.prompt, "hello");
        assert!(params.model.is_none());
        assert!(params.workspace.is_none());
    }

    #[test]
    fn list_history_params_defaults() {
        let params: ListHistoryParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.limit, 50);
        assert_eq!(params.offset, 0);
    }

    #[test]
    fn cancel_turn_params_optional_reason() {
        let with_reason: CancelTurnParams =
            serde_json::from_str(r#"{"reason": "user pressed Ctrl+C"}"#).unwrap();
        assert_eq!(with_reason.reason.as_deref(), Some("user pressed Ctrl+C"));

        let without_reason: CancelTurnParams = serde_json::from_str("{}").unwrap();
        assert!(without_reason.reason.is_none());
    }

    #[test]
    fn server_method_roundtrip_via_str() {
        let methods = [
            ServerMethod::SubmitPrompt,
            ServerMethod::CancelTurn,
            ServerMethod::GetStatus,
            ServerMethod::ListHistory,
            ServerMethod::GetConfig,
            ServerMethod::SetConfig,
            ServerMethod::Ping,
        ];
        for method in methods {
            let s = method.as_str();
            let parsed = ServerMethod::from_method_str(s).unwrap();
            assert_eq!(parsed, method, "Roundtrip failed for {s}");
        }
    }

    #[test]
    fn jsonrpc_error_display_format() {
        let err = JsonRpcError::internal_error("something broke");
        let display = format!("{err}");
        assert_eq!(display, "JSON-RPC error -32603: something broke");
    }

    #[test]
    fn request_with_null_id_is_notification() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "crow/ping".to_string(),
            params: json!(null),
        };
        assert!(req.validate().is_ok());
        assert!(req.id.is_none());
    }

    #[test]
    fn status_response_serialization() {
        let status = StatusResponse {
            state: "idle".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            workspace: "/home/user/project".to_string(),
            turn_count: 5,
            total_tokens: 12345,
            uptime_secs: 3600,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["state"], "idle");
        assert_eq!(json["turn_count"], 5);
        assert_eq!(json["total_tokens"], 12345);
        assert_eq!(json["uptime_secs"], 3600);
    }
}
