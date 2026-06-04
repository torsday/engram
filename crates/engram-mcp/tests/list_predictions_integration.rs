//! Integration test: invoke `list_predictions` over the full MCP wire protocol.

use std::path::PathBuf;
use std::sync::Arc;

use engram_index::sqlite::Migrator;
use engram_mcp::{default_registry, EngramMcpServer};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage, ServerResult};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::ServiceExt;
use rusqlite::Connection;
use serde_json::{json, Value};
use tempfile::TempDir;

fn setup_vault() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let engram_dir = dir.path().join(".engram");
    std::fs::create_dir_all(&engram_dir).unwrap();
    let db_path = engram_dir.join("engram.db");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::new(&conn).apply_all().unwrap();
    conn.execute(
        "INSERT INTO notes (id, path, title, note_type, content, created_by) \
         VALUES ('n1', 'n1.md', 'N', 'fleeting', 'x', 'human')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO predictions (id, note_id, excerpt, claimed_at, due_at, confidence, topic, status) \
         VALUES ('p-open', 'n1', 'Rates will rise', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z', 0.7, 'econ', 'pending')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO predictions (id, note_id, excerpt, claimed_at, confidence, topic, status, resolved_at, resolution_note) \
         VALUES ('p-done', 'n1', 'It rained', '2024-01-10T00:00:00Z', 0.9, 'weather', 'resolved', '2024-01-21T00:00:00Z', 'Confirmed')",
        [],
    )
    .unwrap();
    (dir, db_path)
}

fn build_server(vault_root: PathBuf) -> EngramMcpServer {
    EngramMcpServer::new(Arc::new(default_registry()), vault_root)
}

fn parse_msg(raw: &str) -> ClientJsonRpcMessage {
    serde_json::from_str(raw).expect("test message JSON")
}

async fn initialize(client: &mut impl Transport<rmcp::RoleClient>) {
    client
        .send(parse_msg(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        ))
        .await
        .unwrap();
    let _ = client.receive().await.unwrap();
    client
        .send(parse_msg(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        ))
        .await
        .unwrap();
}

async fn call_tool(
    client: &mut impl Transport<rmcp::RoleClient>,
    id: u64,
    name: &str,
    args: Value,
) -> Value {
    let msg = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":args}});
    client
        .send(serde_json::from_value(msg).unwrap())
        .await
        .unwrap();
    loop {
        let m = client.receive().await.unwrap();
        if let ServerJsonRpcMessage::Response(r) = m {
            if let ServerResult::CallToolResult(out) = r.result {
                return out
                    .structured_content
                    .or_else(|| {
                        out.content.first().and_then(|c| {
                            serde_json::to_value(c)
                                .ok()
                                .and_then(|v| v.get("text").cloned())
                        })
                    })
                    .unwrap_or(Value::Null);
            }
        }
    }
}

#[tokio::test]
async fn list_predictions_open_default_with_summary() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(&mut client, 2, "list_predictions", json!({})).await;
    let preds = result["predictions"].as_array().expect("predictions array");
    assert_eq!(preds.len(), 1, "default open excludes the resolved one");
    assert_eq!(preds[0]["id"], "p-open");
    assert_eq!(preds[0]["claim"], "Rates will rise");
    assert!((preds[0]["confidence_at_claim"].as_f64().unwrap() - 0.7).abs() < 1e-6);

    let summary = &result["calibration_summary"];
    assert_eq!(summary["total"], 2);
    assert_eq!(summary["resolved"], 1);
    assert_eq!(summary["open"], 1);
}

#[tokio::test]
async fn list_predictions_resolved_filter() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;
    let result = call_tool(
        &mut client,
        2,
        "list_predictions",
        json!({ "status": "resolved" }),
    )
    .await;
    let preds = result["predictions"].as_array().unwrap();
    assert_eq!(preds.len(), 1);
    assert_eq!(preds[0]["id"], "p-done");
    assert_eq!(preds[0]["resolution"], "Confirmed");
}

#[tokio::test]
async fn list_predictions_is_listed() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;
    client
        .send(parse_msg(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ))
        .await
        .unwrap();
    let resp = loop {
        let m = client.receive().await.unwrap();
        if matches!(m, ServerJsonRpcMessage::Response(_)) {
            break m;
        }
    };
    if let ServerJsonRpcMessage::Response(r) = resp {
        if let ServerResult::ListToolsResult(list) = r.result {
            assert!(list.tools.iter().any(|t| t.name == "list_predictions"));
        }
    }
}
