//! Integration test: invoke the `read_biography` tool via the full MCP wire
//! protocol. Uses the same in-memory duplex transport pattern as
//! `search_notes_integration.rs`.

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

const BIO_BODY: &str =
    "## Identity\nA systems thinker.\n\n## Domains of expertise\nKnowledge tools.\n";

fn setup_vault(with_bio: bool) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let engram_dir = dir.path().join(".engram");
    std::fs::create_dir_all(&engram_dir).unwrap();
    let db_path = engram_dir.join("engram.db");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::new(&conn).apply_all().unwrap();
    if with_bio {
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, content, modified_at, created_by, frontmatter) \
             VALUES (?1, ?2, ?3, 'moc', ?4, ?5, 'biographer', ?6)",
            params![
                "bio-1",
                "meta/biography.md",
                "Biography",
                BIO_BODY,
                "2024-06-01T00:00:00Z",
                r#"{"confidence": 0.86}"#
            ],
        )
        .unwrap();
    }
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
) -> (Value, Option<bool>) {
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
                let value = out
                    .structured_content
                    .clone()
                    .or_else(|| {
                        out.content.first().and_then(|c| {
                            serde_json::to_value(c)
                                .ok()
                                .and_then(|v| v.get("text").cloned())
                        })
                    })
                    .unwrap_or(Value::Null);
                return (value, out.is_error);
            }
        }
    }
}

#[tokio::test]
async fn read_biography_returns_body_sections_confidence() {
    let (dir, _db) = setup_vault(true);
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let (result, is_error) = call_tool(&mut client, 2, "read_biography", json!({})).await;
    assert_ne!(is_error, Some(true), "present biography is not an error");

    assert_eq!(result["body"], BIO_BODY);
    assert_eq!(result["last_updated"], "2024-06-01T00:00:00Z");
    assert!((result["confidence"].as_f64().unwrap() - 0.86).abs() < 1e-6);
    let sections: Vec<&str> = result["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(sections, vec!["Identity", "Domains of expertise"]);
}

#[tokio::test]
async fn read_biography_missing_is_error() {
    let (dir, _db) = setup_vault(false);
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let (_result, is_error) = call_tool(&mut client, 2, "read_biography", json!({})).await;
    assert_eq!(
        is_error,
        Some(true),
        "a vault with no biography returns an error result"
    );
}

#[tokio::test]
async fn read_biography_is_listed_with_empty_schema() {
    let (dir, _db) = setup_vault(true);
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
                .find(|t| t.name == "read_biography")
                .expect("read_biography in tool list");
            assert!(tool.description.is_some() && !tool.description.as_deref().unwrap().is_empty());
            let schema = &*tool.input_schema;
            assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
        }
    }
}
