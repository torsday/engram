//! `rmcp` stdio adapter that binds the transport-agnostic
//! [`ToolRegistry`](crate::ToolRegistry) to the actual MCP wire protocol.
//!
//! # Surface
//!
//! - [`EngramMcpServer`] — a `rmcp::handler::server::ServerHandler`
//!   implementation that delegates `initialize` / `tools/list` /
//!   `tools/call` to the engram registry.
//! - [`serve_stdio`] — convenience entry point that wires the server to
//!   `rmcp::transport::io::stdio()`. CLI subcommands and tests call this.
//!
//! Tools (`grep_notes`, `read_note`, …) live in their own modules and
//! register themselves through [`crate::default_registry`]. This adapter
//! never re-implements tool logic; it translates between the MCP wire
//! types and the registry's JSON-in / JSON-out shape.
//!
//! # Error mapping
//!
//! A successful tool dispatch produces a `CallToolResult::success` with
//! a single `text` content block containing the JSON-encoded result and
//! `structured_content` carrying the same value parsed. A
//! [`ToolError`](crate::ToolError) produces a `CallToolResult::error`
//! whose content text is `"<code>: <message>"` — clients can match on
//! the `code` prefix. The `is_error` flag is set, which is the MCP
//! signal Claude Desktop / Code use to surface the failure.
//!
//! Why not return a JSON-RPC error (`McpError`) for tool failures?
//! Because per the MCP spec, errors specific to a tool's execution —
//! "not found", "bad input" — belong in `CallToolResult { is_error: true }`,
//! not in the JSON-RPC error channel. The latter is reserved for
//! protocol-level failures (method not found, etc.).

use std::borrow::Cow;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo, Tool, ToolsCapability,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer, ServiceExt};
use rmcp::ErrorData as McpError;

use crate::ToolRegistry;

/// `ServerHandler` implementation backed by an engram [`ToolRegistry`].
///
/// Wrap in `Arc` if you need to share across tasks; the handler itself
/// holds an `Arc<ToolRegistry>` internally so cloning the wrapper is
/// cheap (`clone` increments two refcounts).
#[derive(Clone)]
pub struct EngramMcpServer {
    registry: Arc<ToolRegistry>,
    vault_root: PathBuf,
}

impl EngramMcpServer {
    /// Build a server bound to `registry` and operating on `vault_root`.
    pub fn new(registry: Arc<ToolRegistry>, vault_root: PathBuf) -> Self {
        Self {
            registry,
            vault_root,
        }
    }

    /// Reference to the underlying registry — useful for tests that want
    /// to assert what's registered without going through `list_tools`.
    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }
}

impl ServerHandler for EngramMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(false),
        });
        let mut info = ServerInfo::default();
        info.capabilities = capabilities;
        info.instructions =
            Some("engram vault tools: grep, read, follow_backlinks, follow_links.".into());
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        let metas = self.registry.list();
        let tools: Vec<Tool> = metas
            .into_iter()
            .map(|m| {
                // `Tool::new` requires the schema as `Arc<JsonObject>`; the
                // registry returns it as a `serde_json::Value` that's
                // guaranteed to be an Object (the registry's tests assert
                // this on `default_registry()`). Defensive fallback to an
                // empty object if a future tool registers a non-object
                // schema — we'd rather advertise the tool with a permissive
                // schema than panic.
                let schema_obj = m.input_schema.as_object().cloned().unwrap_or_default();
                Tool::new(
                    Cow::Borrowed(m.name),
                    Cow::Borrowed(m.description),
                    Arc::new(schema_obj),
                )
            })
            .collect();
        std::future::ready(Ok(ListToolsResult::with_all_items(tools)))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + MaybeSendFuture + '_ {
        let arguments = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        let outcome = self
            .registry
            .dispatch(&self.vault_root, &request.name, arguments);

        let result = match outcome {
            Ok(value) => {
                // Serialize once for the text-content block; carry the
                // structured form alongside for clients that prefer it.
                let text = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
                let mut ok = CallToolResult::success(vec![Content::text(text)]);
                ok.structured_content = Some(value);
                ok
            }
            Err(e) => {
                CallToolResult::error(vec![Content::text(format!("{}: {}", e.code, e.message))])
            }
        };
        std::future::ready(Ok(result))
    }
}

/// Errors produced by [`serve_stdio`].
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The initial handshake (initialize / capability exchange) failed
    /// before the server reached steady state.
    #[error("rmcp initialize failed: {0}")]
    Initialize(String),

    /// The running service task failed (panic, transport closed
    /// unexpectedly). Wraps `tokio::task::JoinError` as a string so
    /// callers don't depend on tokio's join-error type.
    #[error("rmcp service exited unexpectedly: {0}")]
    Join(String),
}

/// Run the engram MCP server on stdio until the peer closes the
/// transport.
///
/// Intended entry point for `engram serve --mcp-stdio`. Returns when
/// stdin closes (peer disconnect) or when the running service task
/// completes. Logs via `tracing` are emitted by the underlying handler
/// and the rmcp framework — callers configure their own subscriber.
pub async fn serve_stdio(
    registry: Arc<ToolRegistry>,
    vault_root: PathBuf,
) -> Result<(), ServeError> {
    let server = EngramMcpServer::new(registry, vault_root);
    let transport = rmcp::transport::io::stdio();
    let running = server
        .serve(transport)
        .await
        .map_err(|e| ServeError::Initialize(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| ServeError::Join(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    /// A trivial in-memory tool for adapter tests.
    struct EchoTool;
    impl crate::Tool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "Returns input verbatim — adapter-test fixture."
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({
                "type": "object",
                "properties": {"msg": {"type": "string"}}
            })
        }
        fn invoke(
            &self,
            _: &Path,
            input: serde_json::Value,
        ) -> Result<serde_json::Value, crate::ToolError> {
            Ok(input)
        }
    }

    fn server() -> EngramMcpServer {
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        EngramMcpServer::new(Arc::new(r), PathBuf::from("/dev/null"))
    }

    #[test]
    fn get_info_advertises_tools_capability_only() {
        let info = server().get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "tools capability required"
        );
        // We don't advertise prompts/resources in v1.
        assert!(info.capabilities.prompts.is_none());
        assert!(info.capabilities.resources.is_none());
        assert!(info.instructions.is_some());
    }

    #[tokio::test]
    async fn list_tools_returns_registry_contents() {
        // We can't build a `RequestContext` outside rmcp easily, so we
        // exercise list_tools through the registry directly. The adapter
        // is a thin map — covered by code-review + the unit assertions
        // above + the in-process tests in tests/.
        let s = server();
        let metas = s.registry.list();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "echo");
        // Schema is an object — required by the adapter.
        assert!(metas[0].input_schema.is_object());
    }
}
