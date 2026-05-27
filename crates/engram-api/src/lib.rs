//! Axum HTTP server exposing the engram REST and SSE API surface.
//! All endpoints documented in docs/design/03-architecture.md §API surface.

/// Axum router assembly and server lifecycle.
pub mod server {}

/// `POST /ingest` — accept a raw file or text capture.
pub mod ingest {}

/// `GET /notes`, `GET /notes/:id` — note retrieval.
pub mod notes {}

/// `GET /search` — hybrid semantic + keyword search with provenance filtering.
pub mod search {}

/// `GET /proposals`, `POST /proposals/:id/approve`, `POST /proposals/:id/reject`.
pub mod proposals {}

/// `GET /events` — SSE stream of agent activity, queue updates, indexing progress.
pub mod events {}

/// `POST /council/query` — on-demand Research Council briefing.
pub mod council {}

/// `GET /flows`, `POST /flows/:id/resume` — multi-step flow management.
pub mod flows {}

/// `GET /evals/:agent` — per-agent evaluation history.
pub mod evals {}

/// `GET /agents`, `POST /agents/:id/run` — agent introspection and on-demand trigger.
pub mod agents {}

/// Shared API response envelope and error types.
pub mod response {}

/// `GET /cost` — month-to-date USD spend, per-agent breakdown, projection.
///
/// Response: `{ "period": "2026-05", "total_usd": 12.34, "monthly_cap_usd": 50.0,
///   "percent_consumed": 24.68, "at_warning": false, "at_cap": false,
///   "per_agent": [...] }`
pub mod cost {}
