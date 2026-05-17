//! LLM provider abstraction: Anthropic, OpenAI, Ollama.
//! Agents specify a model tier (`fast`, `standard`, `deep`); this crate maps to
//! a concrete model and handles prompt caching, structured output, and tool use.
//! See ADR 0010 (prompt-caching-first-class), ADR 0011 (tiered-model-escalation),
//! ADR 0013 (tool-use-over-generation).

/// Provider trait: streaming completion, tool use, structured output.
pub mod provider {}

/// Anthropic provider (Opus / Sonnet / Haiku with prompt caching).
pub mod anthropic {}

/// OpenAI provider (GPT-4o, text-embedding-3-large).
pub mod openai {}

/// Ollama provider (local models, embedding fallback).
pub mod ollama {}

/// Model-tier mapping: `fast` / `standard` / `deep` → concrete model IDs.
pub mod tier {}

/// Structured output parsing from LLM responses.
pub mod structured {}

/// Tool-use protocol: tool schemas, call dispatch, result injection.
pub mod tools {}

/// Prompt caching: static-head / dynamic-tail split, cache-hit tracking.
pub mod cache {}
