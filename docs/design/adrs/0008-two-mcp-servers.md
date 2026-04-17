# ADR 0008: Two MCP servers (internal stdio, external HTTP+SSE), not one

**Status:** Accepted

**Date:** 2026-04 (when external MCP design crystallized)

## Context

Engram exposes the vault as MCP tools so LLM clients can reason over it. Two distinct audiences want this:

1. **Claude Desktop / Code on the user's own machine.** Trusted. Wants full access. Standard MCP stdio transport.
2. **The user's own client applications** (the travel-app motivating example, future apps that build on personal context). Less trusted (each is a discrete code surface that could be buggy or malicious). Needs scoped access, authentication, an audit log, and a consent flow. Wants HTTP+SSE transport because it's not co-located.

The conventional approach would be: one MCP server, with sub-routing or scope-checking on each tool call to determine "internal" vs "external" caller behavior.

## Decision

**Two MCP servers, sharing the same underlying tool implementations and vault index.**

- **Internal MCP server.** stdio transport. Full access. No auth. For Claude Desktop.
- **External MCP server.** HTTP+SSE transport. Per-client API keys (argon2-hashed). Per-client OAuth-style scopes. Default-deny privacy zones. Consent flow via Swift app on first connect. Per-client audit log.

Same tool implementations under the hood; same vault index. The difference is the auth boundary and the curation of which tools are exposed.

## Alternatives considered

1. **One MCP server with sub-routing.** Single server, runtime check on caller identity to gate scope. Rejected: the auth surface area is too important to be a runtime branch; structural separation makes the boundary unmistakable.
2. **External MCP only** (no separate internal server). Use the external server even for Claude Desktop. Rejected: stdio transport is what Claude Desktop expects; HTTP+SSE adds latency and complexity for the trusted local case.
3. **No external MCP** (only internal; expose the user's other apps via direct HTTP API calls instead). Rejected: the MCP protocol is exactly the right fit for "give an LLM access to my data with structured tools"; reinventing it for external clients is wasteful.
4. **Two MCP servers.** Chosen.

## Consequences

**Positive:**

- **The auth boundary is structural, not behavioral.** A bug in scope-check logic on the external server cannot leak data to Claude Desktop, and vice versa. The two servers are different processes with different code paths for auth, even though they share the tool implementations.
- **stdio for trusted local; HTTP+SSE for less-trusted remote.** Each transport is the right fit for its audience.
- **Consent flow is opt-in per server.** Internal MCP doesn't need a consent prompt every session; external MCP does. The right ergonomics for each.
- **Tool curation differs cleanly.** Internal exposes raw retrieval primitives (`grep_notes`, `read_note`, `write_note`); external exposes higher-level packaged tools (`personal_context`, `preferences`, `ask_user`, `record_session`) that don't require the client LLM to compose primitives correctly.
- **Audit log is meaningful.** Per-client access patterns are tractable to review when the auth surface is well-defined.

**Negative:**

- **Two server implementations to maintain.** Mitigation: shared tool implementations in `engram-core`; the servers are thin transport layers.
- **Coordination at startup.** Both servers must come up healthy. Mitigation: standard startup sequencing in `engram serve`; failure on either is a fatal startup error.
- **Slightly more code surface area.** Mitigation: the servers are deliberately small (each is a few hundred lines); the bulk of MCP work is the tool implementations.

## References

- `04-external-mcp.md` --- full external MCP design
- `03-architecture.md` --- "MCP servers" section, both transports
- ADR 0002 --- agents-as-data (the same principle of structural separation: agents are data, transports are processes)
