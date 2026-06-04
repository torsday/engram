//! Integration test: invoke `due_flashcards` over the full MCP wire protocol.

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
         VALUES ('n1', 'ml/intro.md', 'N', 'evergreen', 'x', 'human')",
        [],
    )
    .unwrap();
    // One overdue card, one scheduled far in the future.
    conn.execute(
        "INSERT INTO flashcards (id, note_id, question, answer, created_at, last_review_at, next_review_at, stability, review_count) \
         VALUES ('c-due', 'n1', 'What is RRF?', 'Reciprocal rank fusion.', '2024-01-01T00:00:00Z', '2024-01-01T00:00:00Z', '2024-02-01T00:00:00Z', 31.0, 3)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO flashcards (id, note_id, question, answer, created_at, next_review_at, review_count) \
         VALUES ('c-future', 'n1', 'Al dente?', 'Firm.', '2024-01-01T00:00:00Z', '2999-01-01T00:00:00Z', 5)",
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
async fn due_flashcards_returns_overdue_excludes_future() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;
    let result = call_tool(&mut client, 2, "due_flashcards", json!({})).await;
    let cards = result["cards"].as_array().expect("cards array");
    assert_eq!(cards.len(), 1, "only the overdue card is due");
    let c = &cards[0];
    assert_eq!(c["id"], "c-due");
    assert_eq!(c["front"], "What is RRF?");
    assert_eq!(c["back"], "Reciprocal rank fusion.");
    assert_eq!(c["source_note_id"], "n1");
    assert_eq!(c["interval_days"], 31);
    assert_eq!(c["reps"], 3);
}

#[tokio::test]
async fn due_flashcards_deck_filter() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;
    let hit = call_tool(&mut client, 2, "due_flashcards", json!({ "deck": "ml/" })).await;
    assert_eq!(hit["cards"].as_array().unwrap().len(), 1);
    let miss = call_tool(
        &mut client,
        3,
        "due_flashcards",
        json!({ "deck": "other/" }),
    )
    .await;
    assert!(miss["cards"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn due_flashcards_is_listed() {
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
            assert!(list.tools.iter().any(|t| t.name == "due_flashcards"));
        }
    }
}
