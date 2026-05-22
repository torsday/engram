# Release Notes

## [Unreleased]

<!-- Add entries here as work lands on main. Move to a versioned section on release. -->

---

## [0.1.0] - YYYY-MM-DD

First tagged release of the engram CLI.

### New MCP Tools

- `engram_read` — read a note by title or ULID from the vault
- `engram_search` — full-text and semantic search across the vault
- `engram_write` — write a new note (unstaged; human commits)
- `engram_link` — surface related notes for a given note ID

### New Agents

- **Librarian** — auto-tags and links newly added notes
- **Researcher** — synthesises vault context in response to a query

### Breaking Changes

None — first release.

### Bug Fixes

None — first release.

### Infrastructure

- GitHub Actions release workflow: builds macOS arm64/x86_64 and Linux x86_64
  binaries and publishes a draft GitHub Release on `v*` tags
- `scripts/install.sh` one-liner install for macOS and Linux

---

For rollback instructions, see [docs/rollback.md](docs/rollback.md).
