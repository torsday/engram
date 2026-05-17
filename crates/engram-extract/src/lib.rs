//! File classification and extraction dispatch for the Ingestor pipeline.
//! Routes by MIME type to the appropriate extractor, producing a structured artifact.

/// File type classification by MIME type and magic bytes.
pub mod classify {}

/// PDF text extraction.
pub mod pdf {}

/// Image extraction via Claude vision (primary) and ocrs (local fallback).
pub mod image {}

/// Audio transcription via whisper.cpp (local, Apple Silicon optimized).
pub mod audio {}

/// Web URL fetching and article extraction.
pub mod web {}

/// Plain text and markdown normalization.
pub mod text {}
