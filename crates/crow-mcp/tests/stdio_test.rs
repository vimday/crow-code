#![allow(clippy::unwrap_used, clippy::expect_used)]
use crow_mcp::McpClient;
use std::io::Write;
use tempfile::NamedTempFile;

#[tokio::test]
async fn test_mcp_client_spawn_and_initialize() {
    // Rust script that acts as a dummy MCP server.
    // It reads one JSON-RPC request and responds with a mocked initialize result.
    let script = r#"
import sys
import json

# Read the line from stdin
line = sys.stdin.readline().strip()
if not line:
    sys.exit(0)

req = json.loads(line)
sys.stderr.write("Received request: " + str(req) + "\n")

# If it's an initialize request, we reply with a mock initialize result
if req.get("method") == "initialize":
    req_id = req.get("id")
    resp = {
        "jsonrpc": "2.0",
        "id": req_id,
        "result": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "serverInfo": {
                "name": "mock-mcp-server",
                "version": "1.0.0"
            }
        }
    }
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()

# Read the initialized notification
notify = sys.stdin.readline().strip()
if notify:
    sys.stderr.write("Received notify: " + str(notify) + "\n")

    "#;

    let mut tmp_file = NamedTempFile::new().unwrap();
    tmp_file.write_all(script.as_bytes()).unwrap();
    let tmp_path = tmp_file.path().to_str().unwrap();

    // Spawn the client targeting our dummy Python MCP server
    let client =
        McpClient::spawn("python3", &[tmp_path]).expect("Failed to spawn dummy python server");

    // Perform initialize handshake
    let result = client.initialize().await.expect("Initialize failed");

    assert_eq!(result.server_info.name, "mock-mcp-server");
    assert_eq!(result.server_info.version, "1.0.0");
    assert_eq!(result.protocol_version, "2024-11-05");
}
