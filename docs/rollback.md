# Rolling back engram

engram is designed so that upgrades and rollbacks are safe at every layer.

## What an upgrade touches (and what it does not)

| Layer                                | Safe on upgrade? | Notes                                      |
| ------------------------------------ | ---------------- | ------------------------------------------ |
| Vault markdown files                 | Yes              | engram never rewrites vault content        |
| Vault git history                    | Yes              | agents never commit; only the human does   |
| SQLite index (`.engram/*.db`)        | Yes              | rebuilt on demand with `engram reindex`    |
| LanceDB vectors (`.engram/vectors/`) | Yes              | rebuilt on demand; delete and re-run index |
| Config (`config.toml`)               | Yes              | schema is additive; old keys are ignored   |

## Rolling back the binary

1. Find the version you want in
   [GitHub Releases](https://github.com/torsday/engram/releases).
2. Download the archive for your platform (see [install.md](install.md) for
   platform names).
3. Replace the installed binary:

```sh
tar -xzf engram-<version>-<target>.tar.gz
chmod +x engram
sudo mv engram /usr/local/bin/engram
engram --version   # confirm
```

That is the entire rollback. No database migration in reverse is needed because
the index and vector store are derived caches, not source-of-truth data.

## Rebuilding the derived index

If a schema migration introduced a bug or you rolled back to an older binary that
does not understand the current index schema, rebuild from scratch:

```sh
engram reindex --vault ~/my-vault
```

`engram migrate` is idempotent — running it multiple times is safe. If a
migration step is buggy and leaves the index in a broken state, wipe and rebuild:

```sh
rm ~/my-vault/.engram/engram.db
engram migrate --vault ~/my-vault
```

## Rebuilding the vector store

The LanceDB vector store lives in `.engram/vectors/` inside your vault. It is a
pure cache derived from vault content and is safe to delete at any time:

```sh
rm -rf ~/my-vault/.engram/vectors/
engram reindex --vault ~/my-vault   # re-embeds all notes
```

Re-embedding takes time proportional to vault size; embeddings are cached by
content hash ([ADR 0012](design/adrs/0012-embedding-cache-by-content-hash.md))
so unchanged notes are fast on subsequent runs.

## Emergency recovery

If something goes badly wrong and the vault itself is affected, every vault is a
plain git repository. Check the git log and restore any notes from history:

```sh
cd ~/my-vault
git log --oneline -20
git checkout <sha> -- path/to/note.md
```

engram agents never commit, so any commit in the vault log was made by you.
