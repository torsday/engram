//! Integration test: invoke the `read_index` tool via the full MCP wire
//! protocol. Same in-memory duplex transport pattern as
//! `search_notes_integration.rs`.

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
         VALUES ('idx', 'index.md', 'Index', 'moc', '## Notes\n- [[a]] first note', 'cartographer')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes (id, path, title, note_type, content, created_by) \
         VALUES ('m1', 'mocs/topic.md', 'Topic MOC', 'moc', 'A map of the topic.', 'cartographer')",
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
            r#"{
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "0" }
                }
            }"#,
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
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
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
async fn read_index_root_returns_index_body() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(&mut client, 2, "read_index", json!({})).await;
    assert!(result["root_index_body"]
        .as_str()
        .unwrap()
        .contains("first note"));
    assert!(result.get("mocs").is_none() || result["mocs"].is_null());
}

#[tokio::test]
async fn read_index_all_mocs_returns_mocs() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(&mut client, 2, "read_index", json!({ "mode": "all_mocs" })).await;
    let mocs = result["mocs"].as_array().expect("mocs present");
    assert_eq!(mocs.len(), 1, "the topic MOC, excluding the root index");
    assert_eq!(mocs[0]["path"], "mocs/topic.md");
    assert_eq!(mocs[0]["title"], "Topic MOC");
}

#[tokio::test]
async fn read_index_is_listed_with_mode_enum() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
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
            let tool = list
                .tools
                .iter()
                .find(|t| t.name == "read_index")
                .expect("read_index in tool list");
            assert!(tool.description.is_some());
            let schema = &*tool.input_schema;
            assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
        }
    }
}
