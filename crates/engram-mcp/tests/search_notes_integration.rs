//! Integration test: invoke the `search_notes` tool via the full MCP wire
//! protocol and assert the output matches a golden snapshot.
//!
//! The test uses the same duplex transport pattern as `rmcp_in_memory.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use engram_index::sqlite::Migrator;
use engram_mcp::{default_registry, EngramMcpServer};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage, ServerResult};
use rmcp::transport::{IntoTransport, Transport};
use rmcp::ServiceExt;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;

// ─── vault fixture ──────────────────────────────────────────────────────────

fn setup_vault() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let engram_dir = dir.path().join(".engram");
    std::fs::create_dir_all(&engram_dir).unwrap();

    let db_path = engram_dir.join("engram.db");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::new(&conn).apply_all().unwrap();

    conn.execute(
        "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "note-golden-001",
            "notes/golden.md",
            "Golden Ratio",
            "evergreen",
            "The golden ratio phi ≈ 1.618 appears in spirals, art, and architecture.",
            "2024-09-01T00:00:00Z",
            "human"
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "note-unrelated-002",
            "notes/unrelated.md",
            "Unrelated Topic",
            "evergreen",
            "Completely different content about something else.",
            "2024-09-02T00:00:00Z",
            "human"
        ],
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
            match r.result {
                ServerResult::CallToolResult(out) => {
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
                other => panic!("unexpected result: {other:?}"),
            }
        }
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn search_notes_returns_matching_result() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(
        &mut client,
        2,
        "search_notes",
        json!({ "query": "golden ratio" }),
    )
    .await;

    let results = result.get("results").expect("results key");
    let arr = results.as_array().expect("results is array");
    assert!(!arr.is_empty(), "should have at least one result");

    let first = &arr[0];
    assert_eq!(first["note_id"], "note-golden-001");
    assert_eq!(first["title"], "Golden Ratio");
    assert_eq!(first["path"], "notes/golden.md");
    assert!(first["score"].as_f64().unwrap() > 0.0);
    assert!(["bm25", "dense", "both"].contains(&first["provenance"].as_str().unwrap()));
}

#[tokio::test]
async fn search_notes_empty_query_returns_error() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    // Send empty query — should produce is_error=true result
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "search_notes", "arguments": { "query": "" } }
    });
    client
        .send(serde_json::from_value(msg).unwrap())
        .await
        .unwrap();

    loop {
        let m = client.receive().await.unwrap();
        if let ServerJsonRpcMessage::Response(r) = m {
            match r.result {
                ServerResult::CallToolResult(out) => {
                    assert_eq!(out.is_error, Some(true), "empty query must be an error");
                    break;
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn search_notes_no_match_returns_empty_array() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(
        &mut client,
        2,
        "search_notes",
        json!({ "query": "zzzyyyxxx" }),
    )
    .await;

    let results = result.get("results").expect("results key");
    assert!(results.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_notes_schema_validates() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    // List tools and check search_notes schema is an object with required fields.
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
                .find(|t| t.name == "search_notes")
                .expect("search_notes in tool list");
            assert!(tool.description.is_some() && !tool.description.as_deref().unwrap().is_empty());
            // input_schema is Arc<serde_json::Map<String, Value>> in rmcp
            let schema = &*tool.input_schema;
            assert_eq!(
                schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "schema type must be object"
            );
            let required = schema
                .get("required")
                .and_then(|v| v.as_array())
                .expect("required field");
            assert!(
                required.iter().any(|v| v.as_str() == Some("query")),
                "query must be listed as required"
            );
        }
    }
}
