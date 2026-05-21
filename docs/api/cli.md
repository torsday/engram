# CLI reference

The `engram` binary is the primary user-facing interface. It manages the daemon,
vault configuration, secrets, cost caps, and agent scheduling.

> **Not yet implemented** — the CLI (`engram-cli` crate) is under active
> development. This stub will be replaced with auto-generated content from
> `clap_mangen` once the CLI ships. See `docs/design/07-roadmap.md` §CLI
> for the planned command surface.

## Planned commands (from design)

### Lifecycle

| Command          | Description                                       |
| ---------------- | ------------------------------------------------- |
| `engram init`    | First-run wizard (vault, provider, cost, privacy) |
| `engram serve`   | Start the background daemon                       |
| `engram status`  | Show vault, index, and agent health               |
| `engram reindex` | Force a full vault re-index                       |
| `engram migrate` | Apply any pending database migrations             |

### Configuration

| Command                  | Description                           |
| ------------------------ | ------------------------------------- |
| `engram config provider` | Change or re-auth LLM providers       |
| `engram config cost`     | Set monthly cap and per-run budget    |
| `engram config agents`   | Enable/disable agents; set thresholds |
| `engram config curator`  | Curator-specific settings             |
| `engram secrets set`     | Store a secret in the keychain        |

### Vault operations

| Command                | Description                                |
| ---------------------- | ------------------------------------------ |
| `engram backup verify` | Check git remote + Time Machine health     |
| `engram ingest <path>` | Ingest files or directories into the vault |
| `engram run <agent>`   | Run a named agent manually                 |
| `engram eval <agent>`  | Run an agent in dry-run / score-only mode  |

### Review queue

| Command               | Description                        |
| --------------------- | ---------------------------------- |
| `engram proposals`    | List pending diff-review proposals |
| `engram approve <id>` | Approve and commit a proposal      |
| `engram reject <id>`  | Reject a proposal                  |

Full `--help` output and man pages will be generated here once the CLI ships.
