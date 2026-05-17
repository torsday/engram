use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use ulid::Ulid;

/// Immutable, time-sortable identifier for a note.
///
/// Stored as a 26-byte Crockford base-32 ASCII array so that `as_str`
/// returns a `&str` with lifetime tied to `&self` — no allocation needed.
/// Generated once at note-creation time; never changed thereafter.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NoteId([u8; 26]);

/// Errors produced by [`NoteId::parse`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("invalid ULID: {0}")]
    InvalidUlid(String),
}

impl NoteId {
    /// Generate a new `NoteId` using the current wall-clock time.
    pub fn new() -> Self {
        Self::from_ulid(Ulid::new())
    }

    /// Parse a ULID string into a `NoteId`.
    ///
    /// Returns [`ParseError`] for any string that is not a valid 26-character
    /// Crockford base-32 ULID (wrong length, invalid characters, or overflow).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        Ulid::from_string(s)
            .map(Self::from_ulid)
            .map_err(|_| ParseError::InvalidUlid(s.to_owned()))
    }

    /// Return the canonical uppercase ULID string (26 characters).
    ///
    /// No allocation — returns a `&str` backed by the internal byte array.
    pub fn as_str(&self) -> &str {
        // SAFETY: NoteId is only ever constructed from valid Crockford base-32
        // uppercase ASCII characters, so the byte array is always valid UTF-8.
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }

    /// Extract the millisecond-precision timestamp embedded in the ULID.
    pub fn timestamp(&self) -> DateTime<Utc> {
        let ulid =
            Ulid::from_string(self.as_str()).expect("NoteId always holds a valid ULID string");
        let ms = ulid.timestamp_ms();
        DateTime::from_timestamp_millis(ms as i64)
            .expect("ULID timestamp is always a valid Unix ms value")
    }

    fn from_ulid(ulid: Ulid) -> Self {
        let s = ulid.to_string();
        let bytes = s.as_bytes();
        let mut arr = [0u8; 26];
        arr.copy_from_slice(bytes);
        NoteId(arr)
    }
}

impl Default for NoteId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for NoteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NoteId({})", self.as_str())
    }
}

impl FromStr for NoteId {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        NoteId::parse(s)
    }
}

impl Serialize for NoteId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for NoteId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        NoteId::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::thread;
    use std::time::Duration;

    // ── unit tests ───────────────────────────────────────────────────────────

    #[test]
    fn new_returns_valid_ulid_string() {
        let id = NoteId::new();
        let s = id.to_string();
        assert_eq!(s.len(), 26, "ULID must be 26 characters");
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn parse_roundtrip() {
        let id = NoteId::new();
        let s = id.to_string();
        let parsed = NoteId::parse(&s).expect("parse must succeed for just-generated id");
        assert_eq!(id, parsed);
    }

    #[test]
    fn as_str_no_allocation() {
        let id = NoteId::new();
        let p1 = id.as_str().as_ptr();
        let p2 = id.as_str().as_ptr();
        // Both calls return pointers into the same internal array.
        assert_eq!(
            p1, p2,
            "as_str must return a reference into the struct, not a new allocation"
        );
    }

    #[test]
    fn parse_invalid_too_short() {
        let err = NoteId::parse("TOOSHORT").unwrap_err();
        assert!(matches!(err, ParseError::InvalidUlid(_)));
    }

    #[test]
    fn parse_invalid_wrong_chars() {
        // 'I', 'L', 'O', 'U' are not valid Crockford base-32 characters.
        let err = NoteId::parse("IIIIIIIIIIIIIIIIIIIIIIIIII").unwrap_err();
        assert!(matches!(err, ParseError::InvalidUlid(_)));
    }

    #[test]
    fn parse_invalid_empty() {
        let err = NoteId::parse("").unwrap_err();
        assert!(matches!(err, ParseError::InvalidUlid(_)));
    }

    #[test]
    fn parse_invalid_26_bad_chars() {
        let err = NoteId::parse("LLLLLLLLLLLLLLLLLLLLLLLLLL").unwrap_err();
        assert!(matches!(err, ParseError::InvalidUlid(_)));
    }

    #[test]
    fn timestamp_is_recent() {
        use chrono::TimeDelta;
        // ULID timestamps are truncated to milliseconds; `Utc::now()` has
        // nanosecond precision. Subtract 1ms from `before` to absorb the
        // truncation so the test isn't racy at millisecond boundaries.
        let before = Utc::now() - TimeDelta::milliseconds(1);
        let id = NoteId::new();
        let after = Utc::now();
        let ts = id.timestamp();
        assert!(
            ts >= before,
            "timestamp should be >= time before generation (minus 1ms)"
        );
        assert!(ts <= after, "timestamp should be <= time after generation");
    }

    #[test]
    fn time_ordered_ids_sort_lexicographically() {
        let a = NoteId::new();
        thread::sleep(Duration::from_millis(2));
        let b = NoteId::new();
        assert!(a < b, "later-generated id must sort after earlier one");
        assert!(
            a.to_string() < b.to_string(),
            "string ordering must match id ordering"
        );
    }

    #[test]
    fn display_and_as_str_agree() {
        let id = NoteId::new();
        assert_eq!(id.to_string(), id.as_str());
    }

    #[test]
    fn from_str_trait() {
        let id = NoteId::new();
        let s = id.to_string();
        let parsed: NoteId = s.parse().expect("FromStr must succeed");
        assert_eq!(id, parsed);
    }

    #[test]
    fn serde_roundtrip_json() {
        let id = NoteId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        // JSON value should be a quoted ULID string.
        assert!(json.starts_with('"') && json.ends_with('"'));
        let decoded: NoteId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, decoded);
    }

    #[test]
    fn serde_rejects_invalid_ulid() {
        let err = serde_json::from_str::<NoteId>("\"notaulid\"");
        assert!(err.is_err(), "deserializing invalid ULID must fail");
    }

    // ── property tests ───────────────────────────────────────────────────────

    proptest! {
        #[test]
        fn prop_parse_roundtrip(_seed in 0u64..u64::MAX) {
            let id = NoteId::new();
            let s = id.to_string();
            let parsed = NoteId::parse(&s).unwrap();
            prop_assert_eq!(id, parsed);
        }

        #[test]
        fn prop_string_is_26_chars(_seed in 0u64..u64::MAX) {
            let id = NoteId::new();
            prop_assert_eq!(id.to_string().len(), 26);
        }

        #[test]
        fn prop_string_is_uppercase_alnum(_seed in 0u64..u64::MAX) {
            let id = NoteId::new();
            let s = id.to_string();
            prop_assert!(s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
        }

        #[test]
        fn prop_ordering_matches_string_ordering(a_seed in 0u64..u64::MAX, b_seed in 0u64..u64::MAX) {
            // Generate two ids and verify id ordering ↔ string ordering.
            let _ = (a_seed, b_seed); // seeds unused; ordering comes from time
            let a = NoteId::new();
            let b = NoteId::new();
            // They might be equal (same millisecond), but otherwise must agree.
            let id_order = a.cmp(&b);
            let str_order = a.as_str().cmp(b.as_str());
            prop_assert_eq!(id_order, str_order);
        }
    }
}
