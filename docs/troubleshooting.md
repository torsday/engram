# Troubleshooting

## "Agents are silent" — nothing is being proposed

**Check daemon health:**

```sh
engram status
```

Look for:

- `provider: ok` — if this says `error`, re-run `engram config provider` and
  verify your API key.
- `index: running` — if the index isn't running, proposals can't be generated.
  Restart with `engram serve`.
- `agents: enabled` — check that the agents you want are turned on in
  `engram config agents`.

**Check eval scores:**

Agents gate their own proposals behind a confidence threshold (default 0.7).
If the vault is sparse or the notes are very short, agents may score below the
threshold and stay silent.

```sh
engram status --verbose
```

The verbose output shows recent agent runs and their confidence scores. If every
run is just below threshold, lower the gate temporarily:

```sh
engram config agents --confidence-threshold 0.5
```

## "Indices are out of sync" — search results look stale

Force a full re-index:

```sh
engram reindex
```

This rebuilds the SQLite FTS index and the LanceDB vector store from scratch.
On a large vault (10k+ notes) this can take a few minutes.

## "I lost vault state" — recovery from git

Engram never deletes files — all writes go through the diff-review queue and
every approved change is committed to git. To recover from a bad state:

```sh
# See recent commits
git -C ~/Documents/my-vault log --oneline -20

# Revert the last engram commit
git -C ~/Documents/my-vault revert HEAD

# Or reset to a specific point in time
git -C ~/Documents/my-vault checkout <sha> -- path/to/note.md
```

Because every write is a git commit, you have a full audit trail of every agent
action.

## "Token costs are too high"

**View the cost dashboard:**

```sh
engram status --cost
```

This shows per-agent spend for the current month and the last 7 days.

**Find the expensive agents:**

The Curator (digest pipeline) is the most expensive agent by far because it
processes large clusters of notes. If cost is a concern:

1. Raise the Curator's confidence threshold — it will run less often.
2. Reduce the Curator's `max_cluster_size` in `engram config curator`.
3. Switch expensive agents to Ollama (local) for the bulk work and keep
   Anthropic/OpenAI for the final synthesis pass only.

**Set a tighter cap:**

```sh
engram config cost --monthly-cap 5.00 --per-run-cap 0.25
```

## "Backup check is failing"

```sh
engram backup verify
```

The output will identify which layer is stale:

- **Git remote stale** — push your vault repo: `git -C ~/Documents/my-vault push`
- **Time Machine stale** — check that Time Machine is running and the last backup
  completed successfully (`System Settings → General → Time Machine`).

Until both layers are healthy, agents that write to the vault will refuse to run.

## Still stuck?

Check the daemon logs:

```sh
engram logs --tail 100
```

Open an issue at <https://github.com/torsday/engram/issues> with the relevant
log lines (redact any personal content first).
