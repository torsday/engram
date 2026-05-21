# First run

After installing engram (see [`install.md`](install.md)), run:

```sh
engram init
```

This launches the **first-run wizard**, a guided 7-stage setup that takes about
five minutes.

## Stage 1 — Vault

Point engram at your existing Obsidian vault:

```
Vault path: ~/Documents/my-vault
```

Engram reads your vault as plain markdown files. It never moves, renames, or
deletes files without your approval via the diff-review queue.

## Stage 2 — LLM provider

Choose how engram talks to language models:

| Option                | Notes                                                    |
| --------------------- | -------------------------------------------------------- |
| **Anthropic** (cloud) | Best quality. Requires an API key.                       |
| **OpenAI** (cloud)    | Good quality. Requires an API key.                       |
| **Ollama** (local)    | Free, private, slower. Requires a local Ollama instance. |

You can use Anthropic or OpenAI for most work and Ollama for privacy-sensitive
content (configured via privacy zones in Stage 5).

API keys are stored in your macOS Keychain — never in plaintext files.

## Stage 3 — Cost limits

Set a monthly spend cap and per-run budget:

```
Monthly cap:   $10.00
Per-run cap:   $0.50
```

Agents pause and surface a warning when either limit is approached. You can
adjust these at any time with `engram config cost`.

## Stage 4 — Backup

Engram checks two backup layers before writing anything to your vault:

1. **Git remote** — your vault repo should have an unpushed commit horizon of ≤ 1 day.
2. **Time Machine** — the latest snapshot should be ≤ 1 day old.

If either check fails, agents refuse to write and surface the warning in
`engram status`.

Run `engram backup verify` at any time to check manually.

## Stage 5 — Privacy zones

Privacy zones let you route sensitive content to a local provider (Ollama) instead
of a cloud API:

```
Private paths: ~/Documents/my-vault/journal/
               ~/Documents/my-vault/health/
```

Notes under these paths are never sent to Anthropic or OpenAI. Ollama must be
configured (Stage 2) for this to work.

## Stage 6 — Agents

Enable the agents you want to run:

| Agent            | What it does                                        |
| ---------------- | --------------------------------------------------- |
| **Linker**       | Proposes wikilinks between notes                    |
| **Gardener**     | Flags and prunes stale notes                        |
| **Cartographer** | Keeps `index.md` and MOCs current                   |
| **Scribe**       | Cleans fleeting notes and formats literature notes  |
| **Curator**      | Distills clusters of notes into evergreen summaries |

All agents operate in **propose mode** until you promote them. Every proposed
change lands in the diff-review queue — you approve or reject each one.

## Stage 7 — Tutorial vault

Optionally create a small tutorial vault to try engram on a safe corpus before
pointing it at your real notes:

```
Create tutorial vault? [Y/n]
```

The tutorial vault includes sample notes across all five note types and a
pre-seeded diff-review queue showing what agent proposals look like.

---

After the wizard completes, engram starts the background daemon and runs an
initial index pass. Check `engram status` to confirm everything is healthy.
