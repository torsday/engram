//! MCP server exposing vault tools to Claude Desktop and Claude Code.
//! Uses rmcp stdio transport. Runs in the same process as `engram serve`.
//! See docs/design/04-external-mcp.md for the full tool surface.

/// MCP server setup: tool registry and stdio transport initialization.
pub mod server {}

/// `search_notes` — semantic hybrid search across the vault.
pub mod search_notes {}

/// `grep_notes` — exact-string literal search.
pub mod grep_notes;

/// `read_note` — fetch a note by id, slug, or path.
pub mod read_note;

/// `list_tags` — enumerate all vault tags with counts.
pub mod list_tags {}

/// `follow_backlinks` — resolve notes linking to a given note.
pub mod follow_backlinks;

/// `follow_links` — resolve outbound wikilinks from a given note.
pub mod follow_links;

/// `recent_changes` — notes modified in the last N days.
pub mod recent_changes {}

/// `vault_health` — structural health check (broken links, orphans, missing sidecars).
pub mod vault_health {}

/// `read_biography` — retrieve the Biographer agent's current user model.
pub mod read_biography {}

/// `trace_concept` — concept evolution over time via git history.
pub mod trace_concept {}

/// `write_note` — gated write: subject to confidence threshold and review queue.
pub mod write_note {}
