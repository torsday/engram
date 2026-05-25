# engram-mcp

MCP server exposing engram vault tools to Claude Desktop and Claude Code via the rmcp stdio transport.

## Tools

### `search_notes`

Hybrid semantic search (BM25 + Reciprocal Rank Fusion) across vault notes.

**Input:**

```json
{
  "query": "atomic habits compound",
  "k": 10,
  "filter": {
    "tag": "productivity",
    "type": "evergreen",
    "since": "2024-01-01T00:00:00Z",
    "author": "human"
  }
}
```

- `query` (required) — natural-language or FTS5-syntax search string
- `k` (optional, default `10`) — maximum results to return
- `filter` (optional) — all filter fields are optional

**Output:**

```json
{
  "results": [
    {
      "note_id": "01HX...",
      "title": "Atomic Habits",
      "path": "books/atomic-habits.md",
      "snippet": "Small <b>compound</b> changes…",
      "score": 0.0154,
      "provenance": "bm25"
    }
  ]
}
```

- `provenance` is one of `"bm25"`, `"dense"`, or `"both"`
- `snippet` contains HTML `<b>…</b>` markers around matched tokens

**Error codes:**

| code | meaning |
|------|---------|
| `bad_input` | Empty query or invalid input |
| `vault_not_configured` | `engram.db` not found at `<vault_root>/.engram/engram.db` |
| `search_error` | SQLite/FTS5 error during search |

---

### `grep_notes`

Exact-string literal search across vault markdown files.

**Input:**

```json
{
  "pattern": "compound interest",
  "regex": false,
  "case_sensitive": false,
  "max_matches": 100
}
```

**Output:**

```json
{
  "matches": [
    {
      "note_id": "01HX...",
      "path": "/vault/notes/finance.md",
      "line_number": 12,
      "line_text": "Compound interest is the eighth wonder.",
      "char_offset": 0
    }
  ]
}
```

---

### `read_note`

Fetch a single note by ULID, title-slug, or vault-relative path.

**Input:** one of `id`, `slug`, or `path` (required); `include_sidecar` (optional boolean).

---

### `list_tags`

Enumerate all vault tags with usage counts, first-used, and last-used dates.

**Input:** `prefix` (optional string filter), `min_count` (optional integer).

---

### `follow_backlinks`

Resolve notes linking to a given note, up to N hops away.

**Input:** `id` (ULID, required), `depth` (1–3, default 1).

---

### `follow_links`

Resolve outbound wikilinks from a given note, up to N hops away.

**Input:** `id` (ULID, required), `depth` (1–3, default 1).

---

### `recent_changes`

Return vault notes changed within a time window.

**Input:** `since` (ISO-8601), `limit` (integer), `author` (`"human"` | `"agent"` | `"any"`).

---

### `vault_health`

Structural health check: broken links, orphaned notes, missing sidecars.

**Input:** none required.
