//! Integration test: invoke the `trace_concept` tool via the full MCP wire
//! protocol and assert the output shape.
//!
//! Uses the same in-memory duplex transport pattern as
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

// ─── vault fixture ──────────────────────────────────────────────────────────

/// A vault with three notes engaging "compression" across time (one of them
/// `contested`) plus one unrelated note.
fn setup_vault() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let engram_dir = dir.path().join(".engram");
    std::fs::create_dir_all(&engram_dir).unwrap();

    let db_path = engram_dir.join("engram.db");
    let conn = Connection::open(&db_path).unwrap();
    Migrator::new(&conn).apply_all().unwrap();

    let rows = [
        (
            "trace-draft",
            "notes/draft.md",
            "Compression draft",
            "draft",
            "Compression drops detail to save space.",
            "2024-01-01T00:00:00Z",
        ),
        (
            "trace-contested",
            "notes/contested.md",
            "Compression as loss",
            "contested",
            "Maybe compression is not lossy after all; the claim is contested.",
            "2024-04-01T00:00:00Z",
        ),
        (
            "trace-current",
            "notes/current.md",
            "Editing as compression",
            "evergreen",
            "Editing is the editor's compression of intent.",
            "2024-06-01T00:00:00Z",
        ),
        (
            "trace-unrelated",
            "notes/unrelated.md",
            "Woodworking joinery",
            "draft",
            "Dovetail joints resist pull-apart forces.",
            "2024-05-01T00:00:00Z",
        ),
    ];
    for (id, path, title, status, content, modified) in rows {
        conn.execute(
            "INSERT INTO notes (id, path, title, note_type, status, content, modified_at, created_at, created_by) \
             VALUES (?1, ?2, ?3, 'evergreen', ?4, ?5, ?6, ?6, 'human')",
            params![id, path, title, status, content, modified],
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
async fn trace_concept_returns_chronological_excerpts_with_roles() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let result = call_tool(
        &mut client,
        2,
        "trace_concept",
        json!({ "concept": "compression" }),
    )
    .await;

    assert_eq!(result["concept"], "compression");
    assert!(result["narrative"].is_null(), "v1 narrative is null");

    let excerpts = result["excerpts"].as_array().expect("excerpts array");
    assert!(
        excerpts.len() >= 3,
        "the three compression notes are traced"
    );

    // No unrelated note.
    assert!(
        excerpts
            .iter()
            .all(|e| e["note_id"].as_str() != Some("trace-unrelated")),
        "unrelated notes are not traced"
    );

    // Chronological by `at`.
    let times: Vec<&str> = excerpts.iter().map(|e| e["at"].as_str().unwrap()).collect();
    let mut sorted = times.clone();
    sorted.sort_unstable();
    assert_eq!(times, sorted, "excerpts are chronological");

    // Roles: earliest draft, latest current, contested = reversal.
    assert_eq!(excerpts.first().unwrap()["role"], "draft");
    assert_eq!(excerpts.last().unwrap()["role"], "current");
    assert!(
        excerpts
            .iter()
            .any(|e| e["note_id"] == "trace-contested" && e["role"] == "reversal"),
        "a contested note is a reversal"
    );
}

#[tokio::test]
async fn trace_concept_empty_concept_is_error() {
    let (dir, _db) = setup_vault();
    let vault_root = dir.path().to_path_buf();
    let (server_t, client_t) = tokio::io::duplex(8192);
    let _server_task = tokio::spawn(async move { build_server(vault_root).serve(server_t).await });
    let mut client = IntoTransport::<rmcp::RoleClient, _, _>::into_transport(client_t);

    initialize(&mut client).await;

    let msg = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "trace_concept", "arguments": { "concept": "" } }
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
                    assert_eq!(out.is_error, Some(true), "empty concept must be an error");
                    break;
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn trace_concept_schema_lists_concept_required() {
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
                .find(|t| t.name == "trace_concept")
                .expect("trace_concept in tool list");
            assert!(tool.description.is_some() && !tool.description.as_deref().unwrap().is_empty());
            let schema = &*tool.input_schema;
            assert_eq!(schema.get("type").and_then(|v| v.as_str()), Some("object"));
            let required = schema
                .get("required")
                .and_then(|v| v.as_array())
                .expect("required field");
            assert!(
                required.iter().any(|v| v.as_str() == Some("concept")),
                "concept must be required"
            );
        }
    }
}
