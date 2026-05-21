//! `engram-test-support` — shared test infrastructure for engram integration and e2e tests.
//!
//! This crate is intentionally **test-only** and is never published. It provides:
//!
//! - [`FixtureVault`] — fluent builder for synthetic vaults materialized on disk.
//! - [`TempVault`] — convenience wrapper around `FixtureVault` + `tempfile::TempDir`.
//! - [`assertions`] — common test assertions (frontmatter fields, proposal queue, ADR 0003).
//!
//! # Quick start
//!
//! ```rust,no_run
//! use engram_test_support::TempVault;
//!
//! let vault = TempVault::new()
//!     .with_evergreen_notes(5)
//!     .with_fleeting_notes(3)
//!     .build();
//!
//! assert!(vault.path().join("evergreen-0001.md").exists());
//! ```

pub mod assertions;
pub mod builders;

pub use builders::{FixtureVault, TempVault};
