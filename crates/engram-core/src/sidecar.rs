//! Sidecar JSON (`.engram/sidecar/<id>.json`) read/write with schema versioning.
//!
//! Each note's rich agent metadata lives in a sidecar file, separate from the
//! lean human-readable frontmatter. See ADR 0005 and `06-note-conventions.md`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::note_id::NoteId;

/// Current schema version emitted by this binary.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("I/O error for sidecar at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("JSON parse error for sidecar at {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("sidecar at {path} is missing required field `schema_version`")]
    MissingSchemaVersion { path: PathBuf },

    #[error(
        "sidecar at {path} has schema_version {found} which exceeds this binary's support ({supported})"
    )]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    #[error("no upgrade path from schema_version {from} to {to}")]
    NoUpgradePath { from: u32, to: u32 },
}

// ---------------------------------------------------------------------------
// Schema structs — all optional fields use `skip_serializing_if`
// ---------------------------------------------------------------------------

/// One entry in the note's provenance history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceEvent {
    pub event: String,
    pub by: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliberation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Embedding metadata stored alongside the note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingMetadata {
    pub model: String,
    pub version: String,
    pub dimensions: u32,
    pub hash: String,
    pub computed_at: String,
}

/// One entry in the agent visit log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentVisit {
    pub agent: String,
    pub at: String,
    pub outcome: String,
}

/// One entry in the rubric check history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RubricCheck {
    pub at: String,
    pub result: String,
    pub by: String,
}

/// One calibration claim attached to the note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationClaim {
    pub claim: String,
    pub confidence: f64,
    pub by: String,
    pub extracted_by: String,
}

/// Ingestion provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestionMetadata {
    pub via: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_corpus: Option<String>,
}

/// The full sidecar document for one note.
///
/// Unknown fields encountered while reading are **tolerated** (forward-compat)
/// but dropped on the next write — the schema is strict on output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// ULID of the owning note. Matches the `id:` frontmatter field.
    pub id: String,

    /// Schema version. Always [`CURRENT_SCHEMA_VERSION`] on write.
    pub schema_version: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub birth_certificate: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_history: Option<Vec<ProvenanceEvent>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<EmbeddingMetadata>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_visit_log: Option<Vec<AgentVisit>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rubric_check_history: Option<Vec<RubricCheck>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration_claims: Option<Vec<CalibrationClaim>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ingestion: Option<IngestionMetadata>,
}

// ---------------------------------------------------------------------------
// Path helper
// ---------------------------------------------------------------------------

fn sidecar_path_for_id(id: &NoteId, vault_root: &Path) -> PathBuf {
    vault_root
        .join(".engram")
        .join("sidecar")
        .join(format!("{}.json", id.as_str()))
}

fn sidecar_path_for_str(id: &str, vault_root: &Path) -> PathBuf {
    vault_root
        .join(".engram")
        .join("sidecar")
        .join(format!("{}.json", id))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Read the sidecar for `id` from the vault, applying any needed schema upgrades.
///
/// Returns `SidecarError::Io` if the file does not exist.
pub fn read_sidecar(id: &NoteId, vault_root: &Path) -> Result<Sidecar, SidecarError> {
    let path = sidecar_path_for_id(id, vault_root);
    let raw = std::fs::read_to_string(&path).map_err(|source| SidecarError::Io {
        path: path.clone(),
        source,
    })?;

    // Parse to Value first so we can inspect schema_version before full deserialization.
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|source| SidecarError::Parse {
            path: path.clone(),
            source,
        })?;

    let found_version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .map(|v| v as u32)
        .ok_or_else(|| SidecarError::MissingSchemaVersion { path: path.clone() })?;

    if found_version > CURRENT_SCHEMA_VERSION {
        return Err(SidecarError::SchemaTooNew {
            path,
            found: found_version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }

    if found_version < CURRENT_SCHEMA_VERSION {
        value = upgrade_sidecar(value, found_version, CURRENT_SCHEMA_VERSION)?;
    }

    serde_json::from_value(value).map_err(|source| SidecarError::Parse { path, source })
}

/// Write `sidecar` to `.engram/sidecar/<id>.json` atomically (temp-file rename).
///
/// Always writes at [`CURRENT_SCHEMA_VERSION`]. Creates the directory if needed.
/// Uses sorted-key pretty printing for byte-identical diffs.
pub fn write_sidecar(sidecar: &Sidecar, vault_root: &Path) -> Result<(), SidecarError> {
    let path = sidecar_path_for_str(&sidecar.id, vault_root);

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SidecarError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // Serialize to sorted-key pretty JSON.
    let json_str = to_sorted_pretty_json(sidecar)?;

    // Atomic write: write to a temp file in the same directory, then rename.
    // with_extension replaces the last extension; since the final ext is "json"
    // we build the tmp path manually to get "<id>.json.tmp".
    let tmp_path = path.with_file_name(format!(
        "{}.json.tmp",
        path.file_stem().expect("sidecar path has a stem")
            .to_string_lossy()
    ));
    std::fs::write(&tmp_path, &json_str).map_err(|source| SidecarError::Io {
        path: tmp_path.clone(),
        source,
    })?;
    std::fs::rename(&tmp_path, &path).map_err(|source| SidecarError::Io {
        path: path.clone(),
        source,
    })?;

    Ok(())
}

/// Apply in-memory schema migrations from `from` to `to`.
///
/// Currently only version 1 exists, so this is a no-op unless called with
/// future version numbers (in which case it returns `NoUpgradePath`).
pub fn upgrade_sidecar(
    mut value: Value,
    from: u32,
    to: u32,
) -> Result<Value, SidecarError> {
    // Each arm migrates current → current+1.
    // When v2 is introduced, replace this with:
    //   let mut current = from;
    //   while current < to { match current { 1 => { value = migrate_v1_to_v2(value)?; } ... } current += 1; }
    if from < to {
        return Err(SidecarError::NoUpgradePath { from, to });
    }

    // Stamp the new version.
    if let Value::Object(ref mut map) = value {
        map.insert(
            "schema_version".to_string(),
            Value::Number(to.into()),
        );
    }

    Ok(value)
}

// ---------------------------------------------------------------------------
// Sorted-key pretty JSON
// ---------------------------------------------------------------------------

/// Serialize `T` to pretty-printed JSON with keys sorted lexicographically.
///
/// `serde_json` preserves insertion order for objects; we want a stable,
/// diff-friendly order, so we go through `Value` and sort recursively.
fn to_sorted_pretty_json<T: Serialize>(value: &T) -> Result<String, SidecarError> {
    // Serialize to Value (preserves insertion order but we'll sort next).
    let mut json_value = serde_json::to_value(value).map_err(|source| SidecarError::Parse {
        path: PathBuf::from("<serialize>"),
        source,
    })?;

    sort_keys_recursive(&mut json_value);

    // Pretty-print with 2-space indent.
    let mut buf = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    json_value.serialize(&mut ser).map_err(|source| SidecarError::Parse {
        path: PathBuf::from("<serialize>"),
        source,
    })?;

    // Ensure trailing newline.
    let mut s = String::from_utf8(buf).expect("serde_json produces UTF-8");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    Ok(s)
}

/// Recursively sort the keys of every JSON object in `value`.
fn sort_keys_recursive(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // Collect, sort, rebuild.
            let mut entries: Vec<(String, Value)> = map.clone().into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            map.clear();
            for (k, mut v) in entries {
                sort_keys_recursive(&mut v);
                map.insert(k, v);
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                sort_keys_recursive(item);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn minimal_sidecar() -> Sidecar {
        Sidecar {
            id: NoteId::new().to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            created_by: None,
            birth_certificate: None,
            provenance_history: None,
            embedding: None,
            agent_visit_log: None,
            rubric_check_history: None,
            calibration_claims: None,
            ingestion: None,
        }
    }

    fn full_sidecar() -> Sidecar {
        Sidecar {
            id: NoteId::new().to_string(),
            schema_version: CURRENT_SCHEMA_VERSION,
            created_by: Some("synthesizer".to_string()),
            birth_certificate: Some("2026-04-15-0003".to_string()),
            provenance_history: Some(vec![
                ProvenanceEvent {
                    event: "created".to_string(),
                    by: "synthesizer".to_string(),
                    at: "2026-04-15T14:32:00Z".to_string(),
                    deliberation: Some("2026-04-15-0003".to_string()),
                    confidence: None,
                },
                ProvenanceEvent {
                    event: "linked".to_string(),
                    by: "linker".to_string(),
                    at: "2026-04-15T15:01:00Z".to_string(),
                    deliberation: None,
                    confidence: Some(0.93),
                },
            ]),
            embedding: Some(EmbeddingMetadata {
                model: "bge-m3".to_string(),
                version: "1.5".to_string(),
                dimensions: 1024,
                hash: "sha256:abc123".to_string(),
                computed_at: "2026-04-15T14:32:30Z".to_string(),
            }),
            agent_visit_log: Some(vec![AgentVisit {
                agent: "linker".to_string(),
                at: "2026-04-17T03:00:00Z".to_string(),
                outcome: "no-change".to_string(),
            }]),
            rubric_check_history: Some(vec![RubricCheck {
                at: "2026-04-15T14:32:00Z".to_string(),
                result: "pass".to_string(),
                by: "socratic-prober".to_string(),
            }]),
            calibration_claims: Some(vec![CalibrationClaim {
                claim: "transformers will plateau by 2027".to_string(),
                confidence: 0.7,
                by: "human".to_string(),
                extracted_by: "predictor".to_string(),
            }]),
            ingestion: Some(IngestionMetadata {
                via: "ingestor".to_string(),
                at: "2026-04-15T10:30:00Z".to_string(),
                source_artifact: Some("a3f4e2sha256".to_string()),
                source_corpus: None,
            }),
        }
    }

    // -- round-trip tests --

    #[test]
    fn round_trip_minimal() {
        let tmp = TempDir::new().unwrap();
        let sc = minimal_sidecar();
        let id = NoteId::parse(&sc.id).unwrap();
        write_sidecar(&sc, tmp.path()).unwrap();
        let loaded = read_sidecar(&id, tmp.path()).unwrap();
        assert_eq!(sc, loaded);
    }

    #[test]
    fn round_trip_full() {
        let tmp = TempDir::new().unwrap();
        let sc = full_sidecar();
        let id = NoteId::parse(&sc.id).unwrap();
        write_sidecar(&sc, tmp.path()).unwrap();
        let loaded = read_sidecar(&id, tmp.path()).unwrap();
        assert_eq!(sc, loaded);
    }

    #[test]
    fn round_trip_optional_sections() {
        let tmp = TempDir::new().unwrap();
        let mut sc = minimal_sidecar();
        sc.created_by = Some("curator".to_string());
        sc.agent_visit_log = Some(vec![]);
        let id = NoteId::parse(&sc.id).unwrap();
        write_sidecar(&sc, tmp.path()).unwrap();
        let loaded = read_sidecar(&id, tmp.path()).unwrap();
        assert_eq!(sc, loaded);
    }

    // -- JSON formatting --

    #[test]
    fn serialize_twice_byte_identical() {
        let sc = full_sidecar();
        let a = to_sorted_pretty_json(&sc).unwrap();
        let b = to_sorted_pretty_json(&sc).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn json_keys_are_sorted() {
        let sc = full_sidecar();
        let json = to_sorted_pretty_json(&sc).unwrap();
        // Verify top-level keys appear in alphabetical order.
        // Sorted order: agent_visit_log < birth_certificate < calibration_claims
        //               < created_by < embedding < id < ingestion < provenance_history
        //               < rubric_check_history < schema_version
        let agent_pos = json.find(r#""agent_visit_log""#).unwrap();
        let birth_pos = json.find(r#""birth_certificate""#).unwrap();
        let calibration_pos = json.find(r#""calibration_claims""#).unwrap();
        let created_by_pos = json.find(r#""created_by""#).unwrap();
        let embedding_pos = json.find(r#""embedding""#).unwrap();
        let schema_pos = json.find(r#""schema_version""#).unwrap();
        assert!(agent_pos < birth_pos, "agent < birth");
        assert!(birth_pos < calibration_pos, "birth < calibration");
        assert!(calibration_pos < created_by_pos, "calibration < created_by");
        assert!(created_by_pos < embedding_pos, "created_by < embedding");
        assert!(embedding_pos < schema_pos, "embedding < schema_version");
    }

    #[test]
    fn output_has_trailing_newline() {
        let sc = minimal_sidecar();
        let json = to_sorted_pretty_json(&sc).unwrap();
        assert!(json.ends_with('\n'));
    }

    // -- error conditions --

    #[test]
    fn read_missing_file_returns_io_error() {
        let tmp = TempDir::new().unwrap();
        let id = NoteId::new();
        let err = read_sidecar(&id, tmp.path()).unwrap_err();
        assert!(matches!(err, SidecarError::Io { .. }));
    }

    #[test]
    fn read_missing_schema_version() {
        let tmp = TempDir::new().unwrap();
        let sidecar_dir = tmp.path().join(".engram").join("sidecar");
        std::fs::create_dir_all(&sidecar_dir).unwrap();
        let id = NoteId::new();
        let id_str = id.to_string();
        std::fs::write(
            sidecar_dir.join(format!("{}.json", id_str)),
            format!(r#"{{"id": "{}"}}"#, id_str),
        )
        .unwrap();
        let err = read_sidecar(&id, tmp.path()).unwrap_err();
        assert!(
            matches!(err, SidecarError::MissingSchemaVersion { .. }),
            "expected MissingSchemaVersion, got: {:?}",
            err
        );
    }

    #[test]
    fn read_schema_too_new_returns_error() {
        let tmp = TempDir::new().unwrap();
        let sidecar_dir = tmp.path().join(".engram").join("sidecar");
        std::fs::create_dir_all(&sidecar_dir).unwrap();
        let id = NoteId::new();
        let id_str = id.to_string();
        std::fs::write(
            sidecar_dir.join(format!("{}.json", id_str)),
            format!(r#"{{"id": "{}", "schema_version": 9999}}"#, id_str),
        )
        .unwrap();
        let err = read_sidecar(&id, tmp.path()).unwrap_err();
        assert!(matches!(err, SidecarError::SchemaTooNew { found: 9999, .. }));
    }

    #[test]
    fn unknown_fields_tolerated_on_read() {
        let tmp = TempDir::new().unwrap();
        let sidecar_dir = tmp.path().join(".engram").join("sidecar");
        std::fs::create_dir_all(&sidecar_dir).unwrap();
        let id = NoteId::new();
        let id_str = id.to_string();
        // Add a field not in the struct — should deserialize without error.
        std::fs::write(
            sidecar_dir.join(format!("{}.json", id_str)),
            format!(
                r#"{{"id": "{}", "schema_version": 1, "future_field": "ignored"}}"#,
                id_str
            ),
        )
        .unwrap();
        let sc = read_sidecar(&id, tmp.path()).unwrap();
        assert_eq!(sc.id, id_str);
    }

    #[test]
    fn atomic_write_creates_directory() {
        let tmp = TempDir::new().unwrap();
        // Directory does not exist yet — write_sidecar must create it.
        let sc = minimal_sidecar();
        write_sidecar(&sc, tmp.path()).unwrap();
        assert!(tmp
            .path()
            .join(".engram")
            .join("sidecar")
            .join(format!("{}.json", sc.id))
            .exists());
    }

    #[test]
    fn no_temp_file_left_after_write() {
        let tmp = TempDir::new().unwrap();
        let sc = minimal_sidecar();
        write_sidecar(&sc, tmp.path()).unwrap();
        let tmp_path = tmp
            .path()
            .join(".engram")
            .join("sidecar")
            .join(format!("{}.json.tmp", sc.id));
        assert!(!tmp_path.exists(), "temp file should be renamed away");
    }

    // -- upgrade_sidecar --

    #[test]
    fn upgrade_same_version_is_noop() {
        let value = serde_json::json!({
            "id": "01JRZK3M7PQNX8BTEST00006",
            "schema_version": 1
        });
        // from == to == 1 → while loop never executes
        let result = upgrade_sidecar(value.clone(), 1, 1).unwrap();
        assert_eq!(result["schema_version"], 1);
    }

    #[test]
    fn upgrade_unknown_path_returns_error() {
        let value = serde_json::json!({"id": "x", "schema_version": 1});
        let err = upgrade_sidecar(value, 1, 2).unwrap_err();
        assert!(matches!(err, SidecarError::NoUpgradePath { from: 1, to: 2 }));
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn arb_provenance() -> impl Strategy<Value = Vec<ProvenanceEvent>> {
        proptest::collection::vec(
            (
                "[a-z]{3,10}",
                "[a-z]{3,10}",
                "2026-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9]:[0-5][0-9]Z",
            )
                .prop_map(|(event, by, at)| ProvenanceEvent {
                    event,
                    by,
                    at,
                    deliberation: None,
                    confidence: None,
                }),
            0..5,
        )
    }

    /// Valid Crockford base32 alphabet (excludes I, L, O, U).
    /// First character restricted to 0-7 so the 128-bit value doesn't overflow.
    fn arb_note_id() -> impl Strategy<Value = String> {
        // 1 first char (0-7) + 25 Crockford base32 chars (excludes I, L, O, U).
        // Crockford alphabet: 0-9 A-H J-K M-N P-T V-Z (32 chars).
        "[0-7][0-9A-HJ-KM-NP-TV-Z]{25}".prop_map(|s| s)
    }

    proptest! {
        #[test]
        fn round_trip_parse_serialize(
            id in arb_note_id(),
            created_by in proptest::option::of("[a-z]{3,10}"),
            provenance in arb_provenance(),
        ) {
            let sc = Sidecar {
                id,
                schema_version: CURRENT_SCHEMA_VERSION,
                created_by,
                birth_certificate: None,
                provenance_history: if provenance.is_empty() { None } else { Some(provenance) },
                embedding: None,
                agent_visit_log: None,
                rubric_check_history: None,
                calibration_claims: None,
                ingestion: None,
            };
            let tmp = TempDir::new().unwrap();
            let note_id = NoteId::parse(&sc.id).unwrap();
            write_sidecar(&sc, tmp.path()).unwrap();
            let loaded = read_sidecar(&note_id, tmp.path()).unwrap();
            prop_assert_eq!(sc, loaded);
        }

        #[test]
        fn serialize_twice_is_byte_identical(
            id in arb_note_id(),
            created_by in proptest::option::of("[a-z]{3,10}"),
        ) {
            let sc = Sidecar {
                id,
                schema_version: CURRENT_SCHEMA_VERSION,
                created_by,
                birth_certificate: None,
                provenance_history: None,
                embedding: None,
                agent_visit_log: None,
                rubric_check_history: None,
                calibration_claims: None,
                ingestion: None,
            };
            let a = to_sorted_pretty_json(&sc).unwrap();
            let b = to_sorted_pretty_json(&sc).unwrap();
            prop_assert_eq!(a, b);
        }
    }
}
