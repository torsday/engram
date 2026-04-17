# ADR 0009: Git read/write boundary enforced at the type system

**Status:** Accepted

**Date:** 2026-04 (added during the design-excellence pass when the architectural-completeness review noted the git-safe boundary was asserted but not designed)

## Context

[ADR 0003](0003-no-agent-commits.md) committed engram to "agents never run `git add` or `git commit`." This is the cornerstone of the git-safe autonomy story: even confident agent actions land as unstaged working-tree changes, and only the human stages or commits.

The original architecture asserted this constraint but did not specify the *enforcement mechanism*. A code review of any agent's runner could find a stray `git_command.commit()` call, but only by review. The constraint needed to be unrepresentable in the type system, not a coding convention.

## Decision

Split git access into **two distinct trait/handle types** in the `engram-git` crate:

```rust
/// Read-only git operations. Available to all agents.
pub trait ReadOnlyGit: Send + Sync {
    fn status(&self) -> Result<Status>;
    fn diff(&self, path: &Path) -> Result<Diff>;
    fn diff_index(&self) -> Result<Vec<FileDiff>>;     // staged but not committed
    fn diff_worktree(&self) -> Result<Vec<FileDiff>>;  // unstaged
    fn log(&self, path: Option<&Path>, limit: usize) -> Result<Vec<Commit>>;
    fn show(&self, sha: &str, path: &Path) -> Result<Vec<u8>>;
    fn ls_tree(&self, sha: &str) -> Result<Vec<TreeEntry>>;
    fn rev_parse(&self, ref_name: &str) -> Result<Sha>;
}

/// Mutating git operations. NOT available to agents.
/// Only constructed inside HTTP handlers and CLI subcommands invoked by the human.
pub trait WriteGit: ReadOnlyGit {
    fn add(&self, paths: &[&Path]) -> Result<()>;        // git add
    fn restore(&self, paths: &[&Path]) -> Result<()>;    // git restore
    fn commit(&self, message: &str, opts: CommitOpts) -> Result<Sha>;
    fn push(&self, remote: &str, branch: &str) -> Result<()>;
    fn pull(&self, remote: &str, branch: &str) -> Result<()>;
}

/// The agent runner accepts only ReadOnlyGit. The write capability is unrepresentable in agent code.
pub struct AgentRunner<G: ReadOnlyGit> {
    git: G,
    // ...
}

/// HTTP handlers and CLI subcommands accept WriteGit.
/// They are the only places that can construct a writable handle.
pub struct ChangesHandler<G: WriteGit> {
    git: G,
    // ...
}
```

The single `WriteGit` implementation (a wrapper around `gix::Repository`) is constructed exactly once at process startup and is **not stored as a globally accessible singleton**. It's owned by the API/CLI layers and passed only to handlers that need it. The agent runner is constructed with a different value: a `ReadOnlyGit` view of the same repository.

Constructors:

```rust
impl GitRepo {
    /// Open the repo. Returns both handles: the WriteGit handle to be passed
    /// to API/CLI layers, and a ReadOnlyGit handle (a downcast view) for the
    /// agent runner.
    pub fn open(path: &Path) -> Result<(impl WriteGit, impl ReadOnlyGit)> { ... }
}
```

The `ReadOnlyGit` view is a **distinct type** (not the same struct exposed via a narrower trait). This prevents agent code from doing `git as &dyn WriteGit` to upcast.

## Alternatives considered

1. **Convention only.** Code review enforces "agents don't commit." Rejected: humans miss things; mid-iteration code might slip the constraint; cannot survive a contributor unfamiliar with the rule.
2. **Runtime check.** A wrapper that panics if `commit` is called from an agent thread. Rejected: runtime failures are worse than compile-time prevention; relies on thread identity which is fragile.
3. **Capability tokens.** Pass an explicit `WriteCapability` token to functions that perform writes; only the human-input path holds the token. Rejected: more cumbersome than two traits and easier to leak.
4. **Two traits with a downcast-prevention pattern.** Chosen.

## Decision rationale

- **Compile-time enforcement.** An agent that tries to call `git.commit()` won't compile. The error message is explicit: "method `commit` not found in `dyn ReadOnlyGit`."
- **Local reasoning.** A reader of agent code knows it cannot mutate git history. They don't have to chase down whether the git handle in scope happens to be the writable one.
- **Easy testing.** Agents under test get a `MockReadOnlyGit`; only handler tests need a `WriteGit` mock.
- **Survives refactors.** Adding a new agent automatically inherits the constraint; no checklist to follow.
- **Survives new contributors.** The trait names themselves teach the rule.

## Consequences

**Positive:**

- The "agents never commit" invariant is structural, not aspirational.
- A grep for `WriteGit` in the codebase yields a small list of sites that can mutate history; all are entry points triggered by the user.
- ADR 0003 is enforced; not just declared.

**Negative:**

- **Slightly more boilerplate at construction time.** The startup code must hand out two handles where one might have sufficed. Mitigation: it's a one-liner per consumer.
- **Sub-agent invocation needs care.** When Curator invokes Synthesizer, it passes its own `ReadOnlyGit` handle through. Sub-agents can never get a `WriteGit` handle even via a parent. This is correct behavior; just worth being explicit about.
- **Mocking in tests requires both trait impls.** Mitigation: a single `MockGit` implementing both for tests that need writes; agent tests use a dedicated `MockReadOnlyGit`.

## References

- [ADR 0003](0003-no-agent-commits.md) --- the constraint this enforces
- `03-architecture.md` --- Concurrency model section, "Git access" entry
- `03-architecture.md` --- `agent_actions` ↔ git stage reconciliation (which writes happen on the human's behalf via the WriteGit handle)
