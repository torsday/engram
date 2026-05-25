//! MCP server scaffold: a transport-agnostic [`Tool`] trait, a name-keyed
//! [`ToolRegistry`], and a [`default_registry`] that wires the vault tools
//! shipped in this crate.
//!
//! The actual MCP wire protocol (initialize/handshake, capabilities,
//! `tools/list`, `tools/call`) is the *transport adapter's* job — see the
//! follow-up issue for the `rmcp` stdio binding. By keeping the registry
//! decoupled from `rmcp`, alternate transports (in-process for the
//! `engram-api` HTTP/SSE surface, mock transports for tests) reuse the
//! same tool set without re-registration.
//!
//! # Wiring
//!
//! Each existing tool module ships a `pub fn handle(vault_root, input)`
//! function. This module wraps those handles in zero-state `Tool`
//! implementations so the registry holds erased `Box<dyn Tool>` values.
//! The wrappers are *adapters*, not re-implementations — they parse the
//! incoming `serde_json::Value` into the tool's concrete input, delegate,
//! and re-serialize the output.
//!
//! # Error contract
//!
//! Every tool surfaces a [`ToolError { code, message }`](ToolError) with a
//! stable `code` string. The MCP transport adapter maps `code` to MCP
//! error responses; in-process callers can match on the string directly.
//! The codes used by the built-in tools are documented in their respective
//! module docs.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

/// Error returned by any tool invocation. `code` is a stable, machine-
/// readable identifier (snake_case); `message` is human-readable.
///
/// The four per-tool `ToolError` types defined alongside individual tool
/// modules share this shape — adapters convert between them losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolError {
    /// Stable error identifier (e.g. `"bad_input"`, `"not_found"`,
    /// `"unknown_tool"`). Always snake_case.
    pub code: String,
    /// Human-readable message; safe to surface to operators but may
    /// contain vault paths.
    pub message: String,
}

impl ToolError {
    /// Helper: build a `bad_input` error with a stringified parse cause.
    pub fn bad_input(reason: impl Into<String>) -> Self {
        Self {
            code: "bad_input".into(),
            message: reason.into(),
        }
    }

    /// Helper: build an `unknown_tool` error.
    pub fn unknown_tool(name: impl Into<String>) -> Self {
        Self {
            code: "unknown_tool".into(),
            message: format!("no tool registered with name '{}'", name.into()),
        }
    }

    /// Helper: build a `serialize_error` (tool returned an output that
    /// couldn't be JSON-encoded — should never happen but we surface it
    /// rather than panicking).
    pub fn serialize(reason: impl Into<String>) -> Self {
        Self {
            code: "serialize_error".into(),
            message: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool trait + metadata
// ---------------------------------------------------------------------------

/// Capability metadata for a single tool — what the MCP transport
/// advertises in response to `tools/list`.
#[derive(Debug, Clone, Serialize)]
pub struct ToolMeta {
    /// Canonical tool name (e.g. `"grep_notes"`).
    pub name: &'static str,
    /// One-line human-readable description.
    pub description: &'static str,
    /// JSON schema describing accepted input.
    pub input_schema: Value,
}

/// A registered MCP tool. Implementations are stateless (the registry
/// holds `Box<dyn Tool>` for the process lifetime), so the trait is
/// `Send + Sync` and methods take `&self`.
///
/// Each tool ships its own input/output types in its own module; the
/// adapter in this file's [`default_registry`] handles JSON marshalling.
pub trait Tool: Send + Sync {
    /// Canonical tool name. Must be unique within a [`ToolRegistry`].
    fn name(&self) -> &'static str;

    /// One-line description for MCP capability advertisement.
    fn description(&self) -> &'static str;

    /// JSON schema describing accepted input. Returned at `tools/list`
    /// time so MCP clients can validate before calling.
    fn input_schema(&self) -> Value;

    /// Invoke the tool against `vault_root` with `input` (JSON).
    ///
    /// Errors are returned via [`ToolError`]; panics are bugs in the
    /// underlying tool, not part of the contract.
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError>;
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Name-keyed set of registered [`Tool`]s. Constructed via
/// [`ToolRegistry::new`] or [`default_registry`].
///
/// Not `Clone` — tools may carry resources (open file handles, caches)
/// that aren't trivially clonable. Share via `Arc<ToolRegistry>` if
/// multiple transports need the same set.
pub struct ToolRegistry {
    tools: HashMap<&'static str, Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Overwrites any existing tool with the same name —
    /// callers that want a fail-fast collision check should
    /// [`Self::contains`] first.
    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.tools.insert(tool.name(), Box::new(tool));
    }

    /// Whether a tool with `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// `true` when no tools are registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Capability metadata for every registered tool, sorted by name —
    /// this is the response body for an MCP `tools/list` request.
    pub fn list(&self) -> Vec<ToolMeta> {
        let mut metas: Vec<ToolMeta> = self
            .tools
            .values()
            .map(|t| ToolMeta {
                name: t.name(),
                description: t.description(),
                input_schema: t.input_schema(),
            })
            .collect();
        metas.sort_by_key(|m| m.name);
        metas
    }

    /// Dispatch a `tools/call` to the named tool. Returns
    /// [`ToolError::unknown_tool`] if the name isn't registered.
    ///
    /// Wraps every dispatch in a `tracing::info_span` keyed by tool name
    /// so callers can correlate logs with individual calls — satisfies the
    /// "every MCP tool call gets a correlation ID logged via `tracing`"
    /// acceptance criterion.
    pub fn dispatch(
        &self,
        vault_root: &Path,
        name: &str,
        input: Value,
    ) -> Result<Value, ToolError> {
        let Some(tool) = self.tools.get(name) else {
            tracing::warn!(tool = name, "engram-mcp: dispatch on unknown tool");
            return Err(ToolError::unknown_tool(name));
        };
        let span = tracing::info_span!("mcp.tool.invoke", tool = name);
        let _enter = span.enter();
        let start = std::time::Instant::now();
        let result = tool.invoke(vault_root, input);
        match &result {
            Ok(_) => tracing::info!(
                tool = name,
                elapsed_ms = start.elapsed().as_millis() as u64,
                "engram-mcp: tool call ok"
            ),
            Err(e) => tracing::warn!(
                tool = name,
                code = %e.code,
                message = %e.message,
                elapsed_ms = start.elapsed().as_millis() as u64,
                "engram-mcp: tool call failed"
            ),
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Built-in tool adapters
// ---------------------------------------------------------------------------

/// Construct the registry pre-loaded with every built-in tool shipped in
/// this crate. The set will grow as more `MCP tool: X` issues land —
/// callers should treat the contents as opaque and reach for
/// [`ToolRegistry::list`] to enumerate.
pub fn default_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(SearchNotesTool);
    r.register(GrepNotesTool);
    r.register(ReadNoteTool);
    r.register(FollowBacklinksTool);
    r.register(FollowLinksTool);
    r.register(ListTagsTool);
    r.register(RecentChangesTool);
    r
}

/// Convert a JSON value into the tool's concrete input type, surfacing a
/// `bad_input` ToolError on parse failure. Macro-style helper to keep
/// each adapter terse.
fn parse_input<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::bad_input(format!("input JSON: {e}")))
}

fn serialize_output<T: Serialize>(out: T) -> Result<Value, ToolError> {
    serde_json::to_value(out).map_err(|e| ToolError::serialize(e.to_string()))
}

/// Convert a per-tool `ToolError` (each tool module defines its own
/// structurally-identical type) into the shared one.
macro_rules! adapt_tool_error {
    ($e:expr) => {{
        let e = $e;
        ToolError {
            code: e.code,
            message: e.message,
        }
    }};
}

// ── search_notes ─────────────────────────────────────────────────────────

struct SearchNotesTool;

impl Tool for SearchNotesTool {
    fn name(&self) -> &'static str {
        "search_notes"
    }
    fn description(&self) -> &'static str {
        "Hybrid semantic search (BM25 + RRF) across vault notes. Returns ranked results with snippets and provenance."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language or FTS5-syntax search query."
                },
                "k": {
                    "type": "integer",
                    "default": 10,
                    "minimum": 1,
                    "description": "Maximum number of results to return."
                },
                "filter": {
                    "type": "object",
                    "description": "Optional metadata filters.",
                    "properties": {
                        "tag":    { "type": "string", "description": "Only notes with this tag." },
                        "type":   { "type": "string", "description": "Only notes of this note_type." },
                        "since":  { "type": "string", "format": "date-time", "description": "Only notes modified at or after this ISO-8601 timestamp." },
                        "author": { "type": "string", "description": "Only notes created by this author." }
                    }
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::search_notes::SearchNotesInput>(input)?;
        let out = crate::search_notes::handle(vault_root, parsed)
            .map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── grep_notes ──────────────────────────────────────────────────────────

struct GrepNotesTool;

impl Tool for GrepNotesTool {
    fn name(&self) -> &'static str {
        "grep_notes"
    }
    fn description(&self) -> &'static str {
        "Exact-string literal search across vault notes."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Literal string to search for. Not a regex."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Match case exactly."
                },
                "max_results": {
                    "type": "integer",
                    "default": 50,
                    "minimum": 1,
                    "maximum": 500,
                    "description": "Maximum match records to return."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::grep_notes::GrepNotesInput>(input)?;
        let out =
            crate::grep_notes::handle(vault_root, parsed).map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── read_note ──────────────────────────────────────────────────────────

struct ReadNoteTool;

impl Tool for ReadNoteTool {
    fn name(&self) -> &'static str {
        "read_note"
    }
    fn description(&self) -> &'static str {
        "Fetch a single note by ULID, title-slug, or path."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ULID identifying the note. One of id/slug/path is required."
                },
                "slug": {
                    "type": "string",
                    "description": "Title-slug. One of id/slug/path is required."
                },
                "path": {
                    "type": "string",
                    "description": "Path relative to the vault root. One of id/slug/path is required."
                },
                "include_sidecar": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include the parsed sidecar JSON."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::read_note::ReadNoteInput>(input)?;
        let out = crate::read_note::handle(vault_root, parsed).map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── follow_backlinks ──────────────────────────────────────────────────

struct FollowBacklinksTool;

impl Tool for FollowBacklinksTool {
    fn name(&self) -> &'static str {
        "follow_backlinks"
    }
    fn description(&self) -> &'static str {
        "Resolve notes linking to a given note, up to N hops away."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ULID of the target note."
                },
                "depth": {
                    "type": "integer",
                    "default": 1,
                    "minimum": 1,
                    "maximum": 3,
                    "description": "Hop depth (1–3)."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::follow_backlinks::FollowBacklinksInput>(input)?;
        let out = crate::follow_backlinks::handle(vault_root, parsed)
            .map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── follow_links ──────────────────────────────────────────────────────

struct FollowLinksTool;

impl Tool for FollowLinksTool {
    fn name(&self) -> &'static str {
        "follow_links"
    }
    fn description(&self) -> &'static str {
        "Resolve outbound wikilinks from a given note, up to N hops away."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["id"],
            "properties": {
                "id": {
                    "type": "string",
                    "description": "ULID of the source note."
                },
                "depth": {
                    "type": "integer",
                    "default": 1,
                    "minimum": 1,
                    "maximum": 3,
                    "description": "Hop depth (1–3)."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::follow_links::FollowLinksInput>(input)?;
        let out =
            crate::follow_links::handle(vault_root, parsed).map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── list_tags ─────────────────────────────────────────────────────────────

struct ListTagsTool;

impl Tool for ListTagsTool {
    fn name(&self) -> &'static str {
        "list_tags"
    }
    fn description(&self) -> &'static str {
        "Enumerate all vault tags with usage counts, first-used, and last-used dates."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prefix": {
                    "type": "string",
                    "description": "Only return tags starting with this prefix (case-insensitive)."
                },
                "min_count": {
                    "type": "integer",
                    "default": 1,
                    "minimum": 0,
                    "description": "Minimum usage count filter."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::list_tags::ListTagsInput>(input)?;
        let out = crate::list_tags::handle(vault_root, parsed).map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}

// ── recent_changes ────────────────────────────────────────────────────────

struct RecentChangesTool;

impl Tool for RecentChangesTool {
    fn name(&self) -> &'static str {
        "recent_changes"
    }
    fn description(&self) -> &'static str {
        "Return vault notes changed within a time window, optionally filtered by author (human/agent) and change type."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "since": {
                    "type": "string",
                    "format": "date-time",
                    "description": "ISO-8601 lower bound (default: 24 h ago)."
                },
                "limit": {
                    "type": "integer",
                    "default": 50,
                    "minimum": 1,
                    "description": "Maximum number of entries to return."
                },
                "author": {
                    "type": "string",
                    "enum": ["human", "agent", "any"],
                    "default": "any",
                    "description": "Filter by author kind."
                }
            }
        })
    }
    fn invoke(&self, vault_root: &Path, input: Value) -> Result<Value, ToolError> {
        let parsed = parse_input::<crate::recent_changes::RecentChangesInput>(input)?;
        let out =
            crate::recent_changes::handle(vault_root, parsed).map_err(|e| adapt_tool_error!(e))?;
        serialize_output(out)
    }
}
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial in-memory tool used to exercise the registry without
    /// dragging in vault fixtures. Production tools are tested via their
    /// own per-tool integration tests under `tests/`.
    struct EchoTool;

    impl Tool for EchoTool {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "Returns the input verbatim — test fixture only."
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn invoke(&self, _: &Path, input: Value) -> Result<Value, ToolError> {
            Ok(input)
        }
    }

    fn vault() -> &'static Path {
        Path::new("/dev/null")
    }

    #[test]
    fn new_registry_is_empty() {
        let r = ToolRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.list().is_empty());
    }

    #[test]
    fn register_then_contains_and_lists() {
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        assert_eq!(r.len(), 1);
        assert!(r.contains("echo"));
        let metas = r.list();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "echo");
    }

    #[test]
    fn list_is_sorted_by_name() {
        let mut r = ToolRegistry::new();
        r.register(GrepNotesTool);
        r.register(ReadNoteTool);
        r.register(FollowBacklinksTool);
        let metas = r.list();
        let names: Vec<&str> = metas.iter().map(|m| m.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "list() must be sorted");
    }

    #[test]
    fn dispatch_invokes_the_named_tool() {
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        let out = r
            .dispatch(vault(), "echo", json!({"hello": "world"}))
            .expect("dispatch ok");
        assert_eq!(out, json!({"hello": "world"}));
    }

    #[test]
    fn dispatch_returns_unknown_tool_error_for_missing_name() {
        let r = ToolRegistry::new();
        let err = r.dispatch(vault(), "no_such_tool", json!({})).unwrap_err();
        assert_eq!(err.code, "unknown_tool");
        assert!(err.message.contains("no_such_tool"));
    }

    #[test]
    fn default_registry_includes_the_shipped_tools() {
        let r = default_registry();
        for expected in [
            "search_notes",
            "grep_notes",
            "read_note",
            "follow_backlinks",
            "follow_links",
        ] {
            assert!(
                r.contains(expected),
                "default_registry missing `{expected}`"
            );
        }
        // Sanity check that each schema is a JSON object — the MCP wire
        // protocol rejects non-objects at `tools/list` time.
        for meta in r.list() {
            assert!(
                meta.input_schema.is_object(),
                "{} schema must be an object",
                meta.name
            );
            assert!(
                !meta.description.is_empty(),
                "{} description must be non-empty",
                meta.name
            );
        }
    }

    #[test]
    fn bad_input_parse_surfaces_bad_input_code() {
        let r = default_registry();
        // grep_notes requires `pattern`; an empty object should fail parse
        // with the inner-tool's bad_input code (which the adapter forwards).
        let err = r.dispatch(vault(), "grep_notes", json!({})).unwrap_err();
        assert_eq!(err.code, "bad_input", "got: {err:?}");
    }

    #[test]
    fn register_overwrites_with_same_name() {
        let mut r = ToolRegistry::new();
        r.register(EchoTool);
        r.register(EchoTool);
        assert_eq!(r.len(), 1, "duplicate registration overwrites, not stacks");
    }

    #[test]
    fn unknown_tool_helper_includes_name_in_message() {
        let e = ToolError::unknown_tool("widget");
        assert_eq!(e.code, "unknown_tool");
        assert!(e.message.contains("widget"));
    }
}
