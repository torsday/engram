//! In-memory rmcp client ↔ engram-mcp server round-trip.
//!
//! Wires the [`EngramMcpServer`] to one half of a `tokio::io::duplex` pair
//! and drives the other half as a raw JSON-RPC client. Exercises the three
//! handler paths: `initialize`, `tools/list`, `tools/call`.
//!
//! Requires the `client` feature on `rmcp`, which dev-deps unify in.

use std::path::PathBuf;
use std::sync::Arc;

use engram_mcp::{EngramMcpServer, Tool, ToolError, ToolRegistry};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage, ServerResult};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::ServiceExt;
use serde_json::{json, Value};

// ─── fixture tool ────────────────────────────────────────────────────────

struct EchoTool;
impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "Returns the input verbatim — adapter integration-test fixture."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"msg": {"type": "string"}}
        })
    }
    fn invoke(&self, _: &std::path::Path, input: Value) -> Result<Value, ToolError> {
        Ok(input)
    }
}

fn build_server() -> EngramMcpServer {
    let mut r = ToolRegistry::new();
    r.register(EchoTool);
    EngramMcpServer::new(Arc::new(r), PathBuf::from("/dev/null"))
}

fn parse(raw: &str) -> ClientJsonRpcMessage {
    serde_json::from_str(raw).expect("test message JSON")
}

async fn do_initialize(client: &mut impl Transport<rmcp::RoleClient>) -> ServerJsonRpcMessage {
    client
        .send(parse(
            r#"{
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "engram-test-client", "version": "0" }
                }
            }"#,
        ))
        .await
        .expect("send initialize");
    let init_resp = client.receive().await.expect("initialize response");
    // Notify the server we're initialized so it transitions out of init state.
    client
        .send(parse(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ))
        .await
        .expect("send initialized");
    init_resp
}

#[tokio::test]
async fn initialize_advertises_tools_capability() {
    let (server_t, client_t) = tokio::io::duplex(4096);
    let server = build_server();
    let _server_task = tokio::spawn(async move { server.serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    let resp = do_initialize(&mut client).await;
    match resp {
        ServerJsonRpcMessage::Response(r) => match r.result {
            ServerResult::InitializeResult(init) => {
                assert!(
                    init.capabilities.tools.is_some(),
                    "tools capability must be advertised"
                );
                assert!(init.capabilities.prompts.is_none());
                assert!(init.capabilities.resources.is_none());
            }
            other => panic!("expected InitializeResult, got {other:?}"),
        },
        other => panic!("expected Response, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_list_returns_registered_tools() {
    let (server_t, client_t) = tokio::io::duplex(4096);
    let server = build_server();
    let _server_task = tokio::spawn(async move { server.serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    let _ = do_initialize(&mut client).await;
    client
        .send(parse(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#))
        .await
        .unwrap();

    // Skip notifications until we get the matching response.
    let resp = loop {
        let m = client.receive().await.unwrap();
        if matches!(m, ServerJsonRpcMessage::Response(_)) {
            break m;
        }
    };
    match resp {
        ServerJsonRpcMessage::Response(r) => match r.result {
            ServerResult::ListToolsResult(list) => {
                assert_eq!(list.tools.len(), 1);
                assert_eq!(list.tools[0].name, "echo");
                assert!(list.tools[0].description.is_some());
            }
            other => panic!("expected ListToolsResult, got {other:?}"),
        },
        other => panic!("expected Response, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_call_round_trips_through_registry() {
    let (server_t, client_t) = tokio::io::duplex(4096);
    let server = build_server();
    let _server_task = tokio::spawn(async move { server.serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    let _ = do_initialize(&mut client).await;
    client
        .send(parse(
            r#"{
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "echo",
                    "arguments": { "msg": "hello" }
                }
            }"#,
        ))
        .await
        .unwrap();

    let resp = loop {
        let m = client.receive().await.unwrap();
        if matches!(m, ServerJsonRpcMessage::Response(_)) {
            break m;
        }
    };
    match resp {
        ServerJsonRpcMessage::Response(r) => match r.result {
            ServerResult::CallToolResult(out) => {
                assert_eq!(out.is_error, Some(false));
                // Structured content carries the echoed value.
                let echoed = out.structured_content.expect("structured_content present");
                assert_eq!(echoed, json!({"msg": "hello"}));
            }
            other => panic!("expected CallToolResult, got {other:?}"),
        },
        other => panic!("expected Response, got {other:?}"),
    }
}

#[tokio::test]
async fn tools_call_on_unknown_tool_returns_error_result() {
    let (server_t, client_t) = tokio::io::duplex(4096);
    let server = build_server();
    let _server_task = tokio::spawn(async move { server.serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    let _ = do_initialize(&mut client).await;
    client
        .send(parse(
            r#"{
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "no_such_tool",
                    "arguments": {}
                }
            }"#,
        ))
        .await
        .unwrap();

    let resp = loop {
        let m = client.receive().await.unwrap();
        if matches!(m, ServerJsonRpcMessage::Response(_)) {
            break m;
        }
    };
    match resp {
        ServerJsonRpcMessage::Response(r) => match r.result {
            ServerResult::CallToolResult(out) => {
                assert_eq!(out.is_error, Some(true), "unknown tool must set is_error");
                // Error text begins with the code prefix.
                let first = &out.content[0];
                let text = serde_json::to_value(first).unwrap();
                let t = text.get("text").and_then(|v| v.as_str()).unwrap_or("");
                assert!(t.starts_with("unknown_tool:"), "got: {t:?}");
            }
            other => panic!("expected CallToolResult, got {other:?}"),
        },
        other => panic!("expected Response, got {other:?}"),
    }
}
