# engram

> Your thoughts, encoded. A living knowledge base that rewrites itself.

Engram is a personal knowledge layer that sits on top of your Obsidian vault and
runs a set of local AI agents to keep your notes organized, linked, and
distilled — without you having to manage it. You capture; engram curates.

## Why

Most note-taking tools treat you as the curator. You build the structure, you
maintain the links, you decide what to keep and what to prune. That works — until
the vault grows past a few hundred notes and the overhead of maintenance starts to
exceed the value of the knowledge itself.

Engram's bet: a small set of focused agents (Linker, Gardener, Cartographer,
Scribe, Curator) can handle the maintenance work, operating below a strict
confidence threshold that keeps every proposed change in a diff-review queue for
your approval. You stay in control; the agents do the housekeeping.

## Status

**Pre-release / alpha.** Core infrastructure (LLM providers, MCP tools, vault
reader, index layer) is implemented. The full agent pipeline, CLI, and SwiftUI app
are under active development. See [`docs/ship-checklist.md`](docs/ship-checklist.md)
for what is and isn't done.

## Quick install

See **[`docs/install.md`](docs/install.md)** for full instructions.

**Shortest path (macOS, Cargo):**

```sh
cargo install engram-cli
engram status
```

## Claude Desktop / Claude Code integration

Engram exposes its vault tools to Claude via the [MCP protocol](https://modelcontextprotocol.io).
Add the following block to your `claude_desktop_config.json`
(`~/Library/Application\ Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "engram": {
      "command": "engram",
      "args": ["serve", "--mcp-stdio", "--vault", "/absolute/path/to/your/vault"]
    }
  }
}
```

Claude Desktop will launch `engram serve --mcp-stdio` as a subprocess and communicate
over stdin/stdout. All tracing output goes to stderr so it won't interfere with the
JSON-RPC channel.

You can also enable MCP by default in your vault config (`.engram/config.toml`):

```toml
[mcp]
enabled = true
```

See **[`docs/api/mcp.md`](docs/api/mcp.md)** for the full tool reference.

## First run

See **[`docs/first-run.md`](docs/first-run.md)** for the guided setup wizard.
The wizard covers vault selection, provider config, cost limits, and privacy zones.

## Troubleshooting

See **[`docs/troubleshooting.md`](docs/troubleshooting.md)** for common issues.

## Architecture overview

See **[`docs/architecture-overview.md`](docs/architecture-overview.md)** for a
user-friendly diagram of note types, the diff-review workflow, and how agents
decide when to act versus propose.

## Design corpus

The full implementer-facing design lives in [`docs/design/README.md`](docs/design/README.md)
(14 numbered docs + 14 ADRs + glossary). Start there if you want to understand
the reasoning behind every architectural decision.

## Copyright

Copyright © 2026 Torsday. All rights reserved.

This repository is source-visible but **not open-source**. There is no `LICENSE`
file. No use, modification, or redistribution is granted. A license may be added
in the future.
