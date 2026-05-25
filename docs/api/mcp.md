# MCP tool reference

Engram exposes a set of MCP (Model Context Protocol) tools that Claude Desktop
and Claude Code can call directly. These tools let Claude read your vault, search
notes, inspect links, and check system health — all without leaving your machine.

## Starting the MCP server

Run `engram serve --mcp-stdio --vault /path/to/vault` to start the server.
Claude Desktop launches this automatically — add it to
`~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "engram": {
      "command": "engram",
      "args": ["serve", "--mcp-stdio", "--vault", "/absolute/path/to/vault"]
    }
  }
}
```

To enable MCP by default via `.engram/config.toml`:

```toml
[mcp]
enabled = true   # default false; --mcp-stdio flag overrides this
```

> **Auto-generated** — this file is regenerated from source by
> `scripts/gen-mcp-docs.sh`. Do not edit by hand; run the script and commit the
> result. CI fails if the committed content diverges from the generated output.

---

`rmcp` stdio adapter that binds the transport-agnostic
[`ToolRegistry`](crate::ToolRegistry) to the actual MCP wire protocol.

# Surface

- [`EngramMcpServer`] — a `rmcp::handler::server::ServerHandler`
  implementation that delegates `initialize` / `tools/list` /
  `tools/call` to the engram registry.
- [`serve_stdio`] — convenience entry point that wires the server to
  `rmcp::transport::io::stdio()`. CLI subcommands and tests call this.

Tools (`grep_notes`, `read_note`, …) live in their own modules and
register themselves through [`crate::default_registry`]. This adapter
never re-implements tool logic; it translates between the MCP wire
types and the registry's JSON-in / JSON-out shape.

# Error mapping

A successful tool dispatch produces a `CallToolResult::success` with
a single `text` content block containing the JSON-encoded result and
`structured_content` carrying the same value parsed. A
[`ToolError`](crate::ToolError) produces a `CallToolResult::error`
whose content text is `"<code>: <message>"` — clients can match on
the `code` prefix. The `is_error` flag is set, which is the MCP
signal Claude Desktop / Code use to surface the failure.

Why not return a JSON-RPC error (`McpError`) for tool failures?
Because per the MCP spec, errors specific to a tool's execution —
"not found", "bad input" — belong in `CallToolResult { is_error: true }`,
not in the JSON-RPC error channel. The latter is reserved for
protocol-level failures (method not found, etc.).

---

`search_notes` MCP tool — hybrid semantic search over the vault.

Wraps [`engram_index::search::hybrid_search`] as an MCP tool. This
module is a pure translation surface: no retrieval logic lives here.

## Input schema

```json
{
  "query": "<string>",                   // required
  "k":     10,                           // optional, default 10
  "filter": {                            // optional
    "tag":       "<string>",
    "type":      "<note_type>",
    "since":     "<ISO-8601 timestamp>",
    "author":    "<string>"
  }
}
```

## Output schema

```json
{
  "results": [
    {
      "note_id":    "<ULID>",
      "title":      "<string>",
      "path":       "<vault-relative path>",
      "snippet":    "<excerpt with <b>…</b> markers>",
      "score":      0.015,
      "provenance": "bm25" | "dense" | "both"
    }
  ]
}
```

## Error codes

| code                   | meaning                                        |
|------------------------|------------------------------------------------|
| `bad_input`            | Empty query or unparseable input JSON          |
| `vault_not_configured` | Vault root is not a directory or DB is missing |
| `search_error`         | SQLite / FTS5 error during search              |

---

`grep_notes` MCP tool — exact-string or regex lookup across vault markdown.

Distinct from `search_notes` — no embeddings, no ranking, no relevance
scoring. Deterministic substring or regex match. Useful when the user knows
the literal phrase, ID, or pattern they are looking for.

## Input schema

```json
{
  "pattern":        "<string>",      // required
  "regex":          false,           // optional, default false
  "case_sensitive": false,           // optional, default false
  "max_matches":    100              // optional, default 100
}
```

## Output schema

```json
{
  "matches": [
    {
      "note_id":     "<ULID or empty string if no frontmatter>",
      "path":        "<absolute path to .md file>",
      "line_number": 1,
      "line_text":   "<the matching line>",
      "char_offset": 5
    }
  ]
}
```

## Error codes

| code                   | meaning                                    |
|------------------------|--------------------------------------------|
| `bad_input`            | Empty pattern or invalid regex             |
| `vault_not_configured` | Vault root is not a directory              |
| `io_error`             | I/O failure scanning the vault             |

---

`read_note` MCP tool — fetch a note by id, slug, or path.

## Input schema

```json
{
  "id":              "<ULID string>",  // optional
  "slug":            "<title-slug>",   // optional
  "path":            "<rel path>",     // optional
  "include_sidecar": false             // optional, default false
}
```

Exactly one of `id`, `slug`, or `path` must be present.

## Output schema

```json
{
  "id":          "<ULID>",
  "title":       "<string>",
  "path":        "<absolute path>",
  "body":        "<markdown body>",
  "frontmatter": { /* full parsed frontmatter */ },
  "sidecar":     { /* sidecar JSON or null */ },
  "backlinks":   [{ "from_note_id": "<ULID>", "anchor": "<string>" }]
}
```

## Error codes

| code                  | meaning                              |
|-----------------------|--------------------------------------|
| `not_found`           | No note matched the key              |
| `bad_input`           | Missing or ambiguous key             |
| `vault_not_configured`| Vault root is not a directory        |
| `io_error`            | I/O failure reading the note file    |
| `parse_error`         | Frontmatter could not be parsed      |

---

`list_tags` MCP tool — enumerate all vault tags with usage counts.

Returns every distinct tag in the `tags` table together with its usage
count, the earliest note creation date it appears on (`first_used`), and
the latest (`last_used`). Results can be filtered by prefix and a minimum
count threshold.

## Input schema

```json
{
  "prefix":    "evergreen",   // optional — only tags starting with this prefix
  "min_count": 1              // optional, default 1
}
```

## Output schema

```json
{
  "tags": [
    {
      "tag":        "evergreen",
      "count":      42,
      "first_used": "2024-01-01T00:00:00Z",
      "last_used":  "2025-06-01T00:00:00Z"
    }
  ]
}
```

## Error codes

| code                   | meaning                               |
|------------------------|---------------------------------------|
| `bad_input`            | Negative min_count or other bad input |
| `vault_not_configured` | SQLite DB not found / not accessible  |
| `internal_error`       | Unexpected SQLite failure             |

---

`follow_backlinks` MCP tool — notes that wikilink to the given note (incoming).

## Input schema

```json
{
  "note_id": "<ULID>",    // one of these is required
  "slug":    "<slug>",
  "path":    "<rel path>",
  "depth":   1             // optional, default 1, max 3
}
```

## Output schema

```json
{
  "backlinks": [
    {
      "note_id": "<ULID>",
      "title":   "<string>",
      "path":    "<absolute path>",
      "anchors": ["<wikilink text>"],
      "depth":   1
    }
  ]
}
```

## Notes

Backlinks require a live link-graph index (`engram-index::link_graph`).
Until that index is built the tool returns an empty `backlinks` list —
this is correct (no links indexed = no known backlinks) and will
auto-populate once the index is running.

## Error codes

| code          | meaning                              |
|---------------|--------------------------------------|
| `bad_input`   | None of id/slug/path provided, or depth out of range |
| `not_found`   | Note not found in the vault          |

---

`follow_links` MCP tool — notes the given note wikilinks to (outgoing).

## Input schema

```json
{
  "note_id":      "<ULID>",   // one of these is required
  "slug":         "<slug>",
  "path":         "<rel path>",
  "include_dead": true        // optional, default true — include unresolved links
}
```

## Output schema

```json
{
  "links": [
    {
      "target_id":   "<ULID or null>",
      "target_slug": "<slug or null>",
      "anchor":      "<wikilink text>",
      "line_number": 5,
      "resolved":    true
    }
  ]
}
```

## Notes

Forward-link resolution requires the wikilink parser and link-graph index
(`engram-core::wikilink`, `engram-index::link_graph`). Until those are
built the tool returns an empty `links` list and a `links_note` explaining
why. This is intentional and correct — the tool surface is stable; the
underlying data populates once the index runs.

## Error codes

| code          | meaning                              |
|---------------|--------------------------------------|
| `bad_input`   | None of id/slug/path provided        |
| `not_found`   | Note not found in the vault          |

---

`recent_changes` MCP tool — notes modified within a time window.

Returns vault notes changed since a given ISO-8601 timestamp, optionally
filtered by author kind (human, agent, or any) and ordered by recency.

The data is sourced from two tables:
- `notes` — captures human-authored creates and modifies (`created_at`,
  `modified_at`)
- `agent_actions` — captures agent-proposed writes (`wrote_at`)

## Input schema

```json
{
  "since":  "2024-01-01T00:00:00Z",  // optional, default: 24h ago
  "limit":  50,                       // optional, default: 50
  "author": "any"                     // optional: "human"|"agent"|"any"
}
```

## Output schema

```json
{
  "changes": [
    {
      "note_id":     "01JXXXXXXXXXXXXXXXXXXXXXXX",
      "path":        "notes/some-note.md",
      "change_type": "modified",
      "at":          "2024-06-01T12:00:00Z",
      "author":      "agent",
      "agent_name":  "linker"
    }
  ]
}
```

## Error codes

| code                   | meaning                               |
|------------------------|---------------------------------------|
| `bad_input`            | Unrecognised `author` value, bad date  |
| `vault_not_configured` | SQLite DB not found / not accessible  |
| `internal_error`       | Unexpected SQLite failure             |

---

`vault_health` MCP tool — diagnostic summary of vault state.

Returns note counts by type, index health, agent activity, recent
failures, backup status, and vault age. Useful for both the user
("how's my vault?") and Claude (knowing what state the system is in).

## Input schema

```json
{}
```

(No inputs — this is a read-only health snapshot.)

## Output schema

```json
{
  "note_counts": {
    "evergreen": 42,
    "literature": 10,
    "fleeting": 5,
    "archive": 3,
    "journal": 1,
    "other": 2,
    "total": 63
  },
  "last_indexed_at": null,
  "agent_activity_24h": {},
  "index_health": { "sqlite": true, "lance": true, "ok": true },
  "recent_failures": [],
  "backup_status": null,
  "vault_age_days": 120
}
```

Fields that depend on a running index or agent runner are `null` / empty
until those subsystems are wired in.

## Error codes

| code                   | meaning                          |
|------------------------|----------------------------------|
| `vault_not_configured` | Vault root is not a directory    |
| `io_error`             | I/O failure scanning the vault   |

---
