# MCP tool reference

Engram exposes a set of MCP (Model Context Protocol) tools that Claude Desktop
and Claude Code can call directly. These tools let Claude read your vault, search
notes, inspect links, and check system health — all without leaving your machine.

The MCP server starts automatically with `engram serve` and is available on the
stdio transport (configured in `~/.config/claude/claude_desktop_config.json`).

> **Auto-generated** — this file is regenerated from source by
> `scripts/gen-mcp-docs.sh`. Do not edit by hand; run the script and commit the
> result. CI fails if the committed content diverges from the generated output.

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
