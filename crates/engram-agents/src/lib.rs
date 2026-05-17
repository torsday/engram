//! Agent host: scheduler, runner, council deliberation, and review queue.

/// Agent identity, configuration, and lifecycle (ready → running → done).
pub mod identity {}

/// Agent scheduler: triggers (file-watch, cron, on-demand) and concurrency limits.
pub mod scheduler {}

/// Agent runner: prompt loading (hot-reload), tool dispatch, structured output parsing.
pub mod runner {}

/// Tool gateway: validates tool calls against agent permissions and the git-write boundary.
pub mod tool_gateway {}

/// Council deliberation state machine (Steelman gate, Devil's Advocate, synthesis).
pub mod council {}

/// Review queue manager: proposal persistence, approval/rejection, confidence tracking.
pub mod review_queue {}

/// Trust score tracker: per-agent accept/reject rates and calibration history.
pub mod trust {}

/// Agent memory store: per-agent working context between runs.
pub mod memory {}

/// Conversation engine: bounded Pair-Thinking sessions.
pub mod conversation {}
