//! Per-agent typed output schemas.
//!
//! Each agent on disk under `agents/<name>/` has a documented JSON
//! output schema in its `prompt.md`. This module groups the
//! corresponding Rust types — one submodule per agent — used by the
//! runner to parse, validate, and route agent output.
//!
//! Per `docs/design/12-agent-spec-template.md` step 3 ("Implement
//! the structured output schema as a Rust type"), the typed output
//! struct is the second build slice for an agent — landing after the
//! `agents/<name>/{config.toml, prompt.md}` files and before the
//! confidence-formula function (step 4).
//!
//! ## Schema discipline (ADR 0010 + ADR 0011)
//!
//! Every output struct **must** place `confidence` first, then
//! `rationale`, then any payload fields. The serde field ordering
//! flows into the prompt's JSON-schema documentation and into the
//! provider's streaming early-exit logic per ADR 0011 — if the
//! cheap fields (`confidence`, `rationale`) stream first, the
//! runner can abort generation before the expensive payload when
//! `confidence` falls below the auto-land floor.
//!
//! ## Per-agent submodules
//!
//! Each agent shipped to `agents/<name>/` gets a submodule here as
//! its typed-output slice lands. The two are kept in lockstep — if
//! the prompt's JSON schema changes, the Rust type changes in the
//! same PR.

/// Steelman (constructive role) — strengthen weak notes with
/// supporting evidence and stronger framings.
///
/// See `agents/steelman-constructive/prompt.md` for the prompt that
/// produces this output; the Rust types here mirror its documented
/// JSON schema.
pub mod steelman_constructive;

/// Devil's Advocate — argue against claims; surface counter-
/// evidence and unstated assumptions. Critical counterpart to
/// `steelman_constructive`; all output passes the Steelman
/// rationality gate per ADR 0007 before counting in council
/// votes or landing as annotations.
///
/// See `agents/devils-advocate/prompt.md` for the prompt that
/// produces this output.
pub mod devils_advocate;

/// Inquirer — generate questions about the vault from four
/// vantage points (`daily-reactive`, `seed-empty-note`,
/// `holistic-gap`, `blindspot`). First multi-mode agent; the
/// `InquirerMode` enum is the typed-dispatch template that
/// Voice Keeper and Pair-Thinking reuse.
///
/// See `agents/inquirer/prompt.md` for the prompt that produces
/// this output.
pub mod inquirer;

/// Voice Keeper — protect authorial voice; flag or rewrite
/// agent-drafted content that doesn't sound like the user.
/// Two-mode (`review`, `model-update`) — the minimal multi-mode
/// variant of the Inquirer template.
///
/// See `agents/voice-keeper/prompt.md` for the prompt that
/// produces this output.
pub mod voice_keeper;

/// Pair-Thinking — live writing collaborator; bounded 3–5 round
/// conversation, one question per round. Conversation-mode
/// agent; each LLM call produces one `PairThinkingTurn`.
///
/// See `agents/pair-thinking/prompt.md` for the prompt that
/// produces this output.
pub mod pair_thinking;

/// Synthesizer — identify clusters of related notes and propose
/// new evergreen notes that name the concept they circle.
/// Structural invasiveness; every output downgrades to a council
/// proposal per ADR 0004 regardless of confidence.
///
/// See `agents/synthesizer/prompt.md` for the prompt that
/// produces this output.
pub mod synthesizer;
