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

/// Splitter — identify notes that violate atomicity (2–3 ideas in
/// one file) and propose specific splits with full link
/// redistribution. Structural invasiveness; always council-routed.
///
/// See `agents/splitter/prompt.md` for the prompt that produces
/// this output.
pub mod splitter;

/// Merger — unify duplicate concept notes into one canonical
/// note; preserve aliases and reroute incoming links. Sibling of
/// Splitter; structural invasiveness; always council-routed.
/// Encodes three never-silent failure modes at the type level:
/// dropped content, unresolved conflicts, lost incoming links.
///
/// See `agents/merger/prompt.md` for the prompt that produces
/// this output.
pub mod merger;

/// Bridge Builder — detect disconnected clusters in the link
/// graph and propose bridge links or bridge notes for accidental
/// gaps. Two output shapes (link vs. note) dispatched by an
/// untagged enum on `proposed_bridge`.
///
/// See `agents/bridge-builder/prompt.md` for the prompt that
/// produces this output.
pub mod bridge_builder;

/// Linker — discover missing wikilinks between notes and propose
/// bidirectional connections. Additive invasiveness; proposals
/// auto-land at confidence ≥ 0.85, council-routed below that.
/// Confidence formula: 0.5×LLM + 0.3×retrieval_agreement +
/// 0.2×calibration_adjustment.
///
/// See `agents/linker/prompt.md` for the prompt that produces
/// this output.
pub mod linker;

/// Scribe — clean fleeting notes (voice transcripts, quick captures)
/// and format literature notes without changing meaning. Two modes:
/// `fleeting_cleanup` (compress filler, fix transcript errors) and
/// `literature_formatting` (normalize headings, tighten citations).
/// Confidence is adjusted downward when the output length deviates
/// from the mode's expected window, guarding against silent content
/// drops or expansions.
///
/// See `agents/scribe/prompt.md` for the prompt that produces this
/// output.
pub mod scribe;

/// Gardener — prune stale content (dead wikilinks, resolved TODOs)
/// and flag decayed evergreens. Follows the ADR 0013 pre-filter-then-
/// judge pattern: deterministic helpers find candidates; the LLM
/// decides which to act on. Runs on a daily cron (03:00).
///
/// See `agents/gardener/prompt.md` for the prompt that produces this
/// output.
pub mod gardener;

/// Predictor — track predictions and confidence claims; maintain a
/// prediction ledger; compute Brier-score calibration profiles per
/// topic. Runs on a daily cron (09:00).
///
/// See `agents/predictor/prompt.md` for the prompt that produces
/// this output.
pub mod predictor;

/// Witness — acknowledge personal and journal notes without analysis,
/// suggestions, or vault modification. Strictly local-only; output
/// goes to `.engram/witness/<date>.md`, never to the vault.
///
/// See `agents/witness/prompt.md` for the prompt that produces this
/// output.
pub mod witness;

/// Tutor — generate spaced-repetition flashcards from evergreen notes
/// using the FSRS-4.5 algorithm and schedule cards for daily review.
/// Additive invasiveness; auto-lands at confidence ≥ 0.80, council-
/// routed below that. Runs on a daily cron (08:00).
///
/// See `agents/tutor/prompt.md` for the prompt that produces this
/// output.
pub mod tutor;

/// Confidence Annotator — scan evergreen notes for claims without
/// explicit epistemic markers and flag them with inline HTML comments.
/// Additive invasiveness; annotations auto-land at confidence ≥ 0.80,
/// council-routed below that. Confidence formula penalises 0.02 per
/// proposed annotation (capped at 0.20) to reflect cumulative
/// uncertainty.
///
/// See `agents/confidence-annotator/prompt.md` for the prompt that
/// produces this output.
pub mod confidence_annotator;

/// Source Demand — flag uncited factual claims in evergreen notes
/// and suggest vault literature notes that could serve as citations.
/// Additive invasiveness; annotations auto-land at confidence ≥ 0.75.
///
/// See `agents/source-demand/prompt.md` for the prompt that produces
/// this output.
pub mod source_demand;

/// Completion Nudger — surface unfinished notes (draft, open TODOs,
/// mid-thought, stale in-progress) as a daily digest. Read-only;
/// never modifies the vault. Runs on a daily cron at 07:00.
///
/// See `agents/completion-nudger/prompt.md` for the prompt that
/// produces this output.
pub mod completion_nudger;

/// Historian — write a weekly activity-log entry summarising agent
/// runs, auto-lands, proposals, and rejections. Creates a changelog
/// note in `.engram/history/`; read-only with respect to vault notes.
///
/// See `agents/historian/prompt.md` for the prompt that produces this
/// output.
pub mod historian;

/// Single dispatch entry point over all typed agent outputs.
/// Used by callers (eval cases, CLI dry-runs, schema-drift CI
/// checks) that want strict validation; the runner's hot path
/// stays on the permissive `parse_confidence` lookup.
pub mod validate;
