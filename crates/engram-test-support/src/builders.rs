//! [`FixtureVault`] builder and [`TempVault`] convenience wrapper.
//!
//! Provides fluent construction of synthetic vaults on disk. Every note gets
//! deterministic content and valid YAML frontmatter so the vault passes
//! `engram-core` parsing without any extra fixtures.

use std::collections::HashMap;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use engram_core::frontmatter::NoteType;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// FixtureVault
// ---------------------------------------------------------------------------

/// Fluent builder for a synthetic vault.
///
/// Call the `.with_*` methods to configure note counts and topology, then call
/// `.build_at(path)` to materialize the vault on disk.
///
/// # Example
///
/// ```rust,no_run
/// use engram_test_support::FixtureVault;
/// use std::path::Path;
///
/// FixtureVault::builder()
///     .with_evergreen_notes(5)
///     .with_fleeting_notes(3)
///     .build_at(Path::new("/tmp/my-vault"));
/// ```
#[derive(Debug, Default)]
pub struct FixtureVaultBuilder {
    evergreen: usize,
    literature: usize,
    fleeting: usize,
    archive: usize,
    journal: usize,
    moc: usize,
    /// `(from_index, to_index)` pairs within the same note type (evergreen only for now).
    wikilinks: Vec<(usize, usize)>,
    /// Spread `created` dates over this many months back from now.
    age_months: Option<u32>,
    /// Extra notes as `(filename, content)` pairs.
    extra: Vec<(String, String)>,
}

impl FixtureVaultBuilder {
    pub fn with_evergreen_notes(mut self, n: usize) -> Self {
        self.evergreen = n;
        self
    }

    pub fn with_literature_notes(mut self, n: usize) -> Self {
        self.literature = n;
        self
    }

    pub fn with_fleeting_notes(mut self, n: usize) -> Self {
        self.fleeting = n;
        self
    }

    pub fn with_archive_notes(mut self, n: usize) -> Self {
        self.archive = n;
        self
    }

    pub fn with_journal_notes(mut self, n: usize) -> Self {
        self.journal = n;
        self
    }

    pub fn with_moc_notes(mut self, n: usize) -> Self {
        self.moc = n;
        self
    }

    /// Add wikilink edges from evergreen note at index `from` to index `to`
    /// (0-based, within the generated evergreen set).
    pub fn with_wikilink_topology(mut self, edges: Vec<(usize, usize)>) -> Self {
        self.wikilinks = edges;
        self
    }

    /// Spread `created` frontmatter dates over the given number of months.
    pub fn with_age_distribution(mut self, months: u32) -> Self {
        self.age_months = Some(months);
        self
    }

    /// Inject a note at a specific path with exact content (bypasses builder
    /// content generation). Useful for edge-case fixtures.
    pub fn with_extra_note(
        mut self,
        filename: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.extra.push((filename.into(), content.into()));
        self
    }

    /// Materialize the vault on disk at `path`, creating directories as needed.
    pub fn build_at(self, path: &Path) {
        fs::create_dir_all(path).expect("failed to create vault directory");

        let now = Utc::now();
        let age_months = self.age_months.unwrap_or(6);

        // Gather evergreen titles for wikilink generation.
        let evergreen_titles: Vec<String> = (1..=self.evergreen)
            .map(|i| format!("Evergreen Note {:04}", i))
            .collect();

        // Build wikilink map: note index → list of target titles.
        let mut wikilink_map: HashMap<usize, Vec<String>> = HashMap::new();
        for (from, to) in &self.wikilinks {
            if *to < evergreen_titles.len() {
                wikilink_map
                    .entry(*from)
                    .or_default()
                    .push(evergreen_titles[*to].clone());
            }
        }

        let total_notes = self.evergreen
            + self.literature
            + self.fleeting
            + self.archive
            + self.journal
            + self.moc;
        let date_step = if total_notes > 1 {
            Duration::days((age_months as i64 * 30) / total_notes.max(1) as i64)
        } else {
            Duration::days(0)
        };

        let mut note_idx = 0usize;

        // Helper: generate a `created` date string.
        let created_at = |idx: usize| -> String {
            let dt = now - date_step * idx as i32;
            dt.format("%Y-%m-%d").to_string()
        };

        // Generate each note type.
        let specs: Vec<(NoteType, usize, &str)> = vec![
            (NoteType::Evergreen, self.evergreen, "evergreen"),
            (NoteType::Literature, self.literature, "literature"),
            (NoteType::Fleeting, self.fleeting, "fleeting"),
            (NoteType::Archive, self.archive, "archive"),
            (NoteType::Journal, self.journal, "journal"),
            (NoteType::Moc, self.moc, "moc"),
        ];

        let mut evergreen_local_idx = 0usize;

        for (note_type, count, type_str) in &specs {
            for i in 1..=*count {
                let ulid = Ulid::new().to_string();
                let title = match note_type {
                    NoteType::Evergreen => format!("Evergreen Note {:04}", i),
                    NoteType::Literature => format!("Literature Note {:04}", i),
                    NoteType::Fleeting => format!("Fleeting Note {:04}", i),
                    NoteType::Archive => format!("Archive Note {:04}", i),
                    NoteType::Journal => format!("Journal {:04}", i),
                    NoteType::Moc => format!("MOC {:04}", i),
                    _ => format!("{} Note {:04}", type_str, i),
                };
                let slug = title_to_slug(&title);
                let created = created_at(note_idx);
                note_idx += 1;

                let mut body = format!(
                    "Fixture note generated by `engram-test-support`. \
                     This is {type_str} note {i}."
                );

                // Inject wikilinks for evergreen notes.
                if matches!(note_type, NoteType::Evergreen) {
                    if let Some(targets) = wikilink_map.get(&(evergreen_local_idx)) {
                        let _ = writeln!(body);
                        for t in targets {
                            let _ = write!(body, "\n[[{t}]]");
                        }
                    }
                    evergreen_local_idx += 1;
                }

                let content = render_note(&ulid, &title, type_str, &created, &body);
                let filename = format!("{slug}.md");
                fs::write(path.join(&filename), &content)
                    .unwrap_or_else(|e| panic!("failed to write {filename}: {e}"));
            }
        }

        // Extra notes.
        for (filename, content) in &self.extra {
            // Create parent dirs if needed.
            let full = path.join(filename);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent)
                    .unwrap_or_else(|e| panic!("failed to create dir for {filename}: {e}"));
            }
            fs::write(&full, content).unwrap_or_else(|e| panic!("failed to write {filename}: {e}"));
        }
    }
}

// ---------------------------------------------------------------------------
// FixtureVault
// ---------------------------------------------------------------------------

/// Entry point for the builder API.
pub struct FixtureVault;

impl FixtureVault {
    pub fn builder() -> FixtureVaultBuilder {
        FixtureVaultBuilder::default()
    }
}

// ---------------------------------------------------------------------------
// TempVault
// ---------------------------------------------------------------------------

/// Convenience wrapper: [`FixtureVaultBuilder`] + a `tempfile::TempDir`.
///
/// The temp directory is cleaned up when `TempVault` is dropped.
///
/// # Example
///
/// ```rust,no_run
/// use engram_test_support::TempVault;
///
/// let vault = TempVault::new().with_evergreen_notes(3).build();
/// let path = vault.path().to_owned();
/// // use vault …
/// // temp directory is deleted when `vault` drops
/// ```
pub struct TempVault {
    builder: FixtureVaultBuilder,
}

impl TempVault {
    pub fn new() -> Self {
        Self {
            builder: FixtureVaultBuilder::default(),
        }
    }

    pub fn with_evergreen_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_evergreen_notes(n);
        self
    }

    pub fn with_literature_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_literature_notes(n);
        self
    }

    pub fn with_fleeting_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_fleeting_notes(n);
        self
    }

    pub fn with_archive_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_archive_notes(n);
        self
    }

    pub fn with_journal_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_journal_notes(n);
        self
    }

    pub fn with_moc_notes(mut self, n: usize) -> Self {
        self.builder = self.builder.with_moc_notes(n);
        self
    }

    pub fn with_wikilink_topology(mut self, edges: Vec<(usize, usize)>) -> Self {
        self.builder = self.builder.with_wikilink_topology(edges);
        self
    }

    pub fn with_age_distribution(mut self, months: u32) -> Self {
        self.builder = self.builder.with_age_distribution(months);
        self
    }

    pub fn with_extra_note(
        mut self,
        filename: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.builder = self.builder.with_extra_note(filename, content);
        self
    }

    /// Materialize the vault in a fresh temp directory.
    pub fn build(self) -> BuiltTempVault {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        self.builder.build_at(dir.path());
        BuiltTempVault { dir }
    }
}

impl Default for TempVault {
    fn default() -> Self {
        Self::new()
    }
}

/// A vault materialized in a temporary directory. Cleaned up on drop.
pub struct BuiltTempVault {
    dir: tempfile::TempDir,
}

impl BuiltTempVault {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Keep the temp directory alive beyond this function (useful for debugging).
    pub fn into_path(self) -> PathBuf {
        self.dir.keep()
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn render_note(id: &str, title: &str, note_type: &str, created: &str, body: &str) -> String {
    format!("---\nid: {id}\ntitle: {title}\ntype: {note_type}\ncreated: {created}\n---\n\n{body}\n")
}

fn title_to_slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engram_core::frontmatter::{parse_frontmatter, NoteType};

    #[test]
    fn builder_creates_expected_files() {
        let vault = TempVault::new()
            .with_evergreen_notes(2)
            .with_fleeting_notes(1)
            .build();

        assert!(vault.path().join("evergreen-note-0001.md").exists());
        assert!(vault.path().join("evergreen-note-0002.md").exists());
        assert!(vault.path().join("fleeting-note-0001.md").exists());
    }

    #[test]
    fn generated_notes_have_valid_frontmatter() {
        let vault = TempVault::new().with_evergreen_notes(3).build();
        let content = std::fs::read_to_string(vault.path().join("evergreen-note-0001.md")).unwrap();
        let fm = parse_frontmatter(&content).unwrap();
        assert_eq!(fm.note_type, NoteType::Evergreen);
        assert_eq!(fm.title, "Evergreen Note 0001");
        assert!(!fm.id.is_empty());
    }

    #[test]
    fn wikilink_topology_injects_links() {
        let vault = TempVault::new()
            .with_evergreen_notes(3)
            .with_wikilink_topology(vec![(0, 1), (0, 2)])
            .build();
        let content = std::fs::read_to_string(vault.path().join("evergreen-note-0001.md")).unwrap();
        assert!(content.contains("[[Evergreen Note 0002]]"));
        assert!(content.contains("[[Evergreen Note 0003]]"));
    }

    #[test]
    fn extra_note_injected_verbatim() {
        let vault = TempVault::new()
            .with_extra_note(
                "custom/note.md",
                "---\nid: test123\ntitle: Custom\ntype: fleeting\n---\n\nHello.\n",
            )
            .build();
        assert!(vault.path().join("custom/note.md").exists());
        let content = std::fs::read_to_string(vault.path().join("custom/note.md")).unwrap();
        assert!(content.contains("Custom"));
    }

    #[test]
    fn age_distribution_sets_created_dates() {
        let vault = TempVault::new()
            .with_evergreen_notes(3)
            .with_age_distribution(3)
            .build();
        let content = std::fs::read_to_string(vault.path().join("evergreen-note-0001.md")).unwrap();
        let fm = parse_frontmatter(&content).unwrap();
        assert!(fm.created.is_some());
    }

    #[test]
    fn empty_builder_produces_empty_vault() {
        let vault = TempVault::new().build();
        let entries: Vec<_> = std::fs::read_dir(vault.path()).unwrap().collect();
        assert!(entries.is_empty());
    }

    #[test]
    fn title_to_slug_handles_spaces_and_digits() {
        assert_eq!(title_to_slug("Evergreen Note 0001"), "evergreen-note-0001");
        assert_eq!(title_to_slug("MOC 0001"), "moc-0001");
    }

    #[test]
    fn literature_notes_created() {
        let vault = TempVault::new().with_literature_notes(2).build();
        assert!(vault.path().join("literature-note-0001.md").exists());
        let content =
            std::fs::read_to_string(vault.path().join("literature-note-0001.md")).unwrap();
        let fm = parse_frontmatter(&content).unwrap();
        assert_eq!(fm.note_type, NoteType::Literature);
    }

    #[test]
    fn build_at_materializes_to_given_path() {
        let dir = tempfile::tempdir().unwrap();
        FixtureVault::builder()
            .with_fleeting_notes(1)
            .build_at(dir.path());
        assert!(dir.path().join("fleeting-note-0001.md").exists());
    }
}
