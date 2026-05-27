//! Typed output schema for the Bridge Builder agent.
//!
//! Mirrors the JSON schema documented in
//! `agents/bridge-builder/prompt.md` § "Output schema". Per ADR
//! 0011, `confidence` and `rationale` come first so streaming
//! early-exit can abort before the per-cluster-pair verdicts.
//!
//! Bridge Builder's invasiveness is `editorial` per
//! `agents/bridge-builder/config.toml` — capturing the higher of
//! the two output shapes (bridge links auto-land at high
//! confidence; bridge notes are council-routed regardless). The
//! Rust types here describe what the LLM emits; the per-output
//! routing decision lives in the runner.
//!
//! ## Two output shapes, one verdict-driven enum
//!
//! Per-cluster-pair verdicts come in three flavors:
//! `meaningful` (decline), `accidental_link` (propose a
//! bridge link), `accidental_note` (propose a bridge note). The
//! verdict picks the [`ProposedBridge`] variant shape — link
//! carries source/target/anchor; note carries title/slug/body +
//! per-cluster anchor IDs. Serde's `#[serde(untagged)]` lets the
//! prompt emit either object without an extra discriminator
//! field, matching its existing JSON schema.

use serde::{Deserialize, Serialize};

/// Top-level output from the Bridge Builder agent.
///
/// Field order is the ADR 0011 streaming-early-exit contract:
/// `confidence` → `rationale` → `cluster_pair_verdicts`. With the
/// payload as a flat array, low-confidence outputs short-circuit
/// before any per-pair verdict streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeBuilderOutput {
    /// Self-assessed confidence (0.0–1.0) that the verdicts
    /// (decline or bridge proposals) are correct for the cluster
    /// pairs analyzed. Low-confidence bridges pollute the graph
    /// more than they help — the prompt requires the agent to
    /// rate honestly and decline as the default.
    pub confidence: f32,

    /// One paragraph: what made the proposed bridges (or the
    /// declines) defensible, and what could be wrong.
    pub rationale: String,

    /// One verdict per cluster pair in the input. The runner
    /// asserts post-parse that there's one verdict per input
    /// pair (no missing pairs, no surprise extras).
    #[serde(default)]
    pub cluster_pair_verdicts: Vec<ClusterPairVerdict>,
}

/// The verdict for a single cluster pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterPairVerdict {
    /// Cluster A's ID.
    pub cluster_a_id: String,
    /// Cluster B's ID.
    pub cluster_b_id: String,
    /// The verdict for this pair.
    pub verdict: BridgeVerdict,
    /// One sentence explaining the verdict in terms of the
    /// clusters' actual content.
    pub reasoning: String,
    /// The proposed bridge. `None` when `verdict ==
    /// Meaningful` (decline). Shape depends on the verdict for
    /// the non-decline cases; serde's `#[serde(untagged)]`
    /// dispatches on field presence.
    #[serde(default)]
    pub proposed_bridge: Option<ProposedBridge>,
}

/// The three verdicts Bridge Builder can issue per cluster pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BridgeVerdict {
    /// The disconnection is meaningful — the two clusters cover
    /// genuinely unrelated topics. Decline; `proposed_bridge` is
    /// `None`. The default outcome in a healthy vault.
    Meaningful,
    /// The disconnection is accidental — a specific anchor in
    /// one cluster reaches into the other. Propose a
    /// bridge *link* (lower invasiveness; auto-landable at high
    /// confidence).
    AccidentalLink,
    /// The disconnection is accidental and the connection is
    /// broad (a shared abstraction across both clusters).
    /// Propose a bridge *note* (Medium invasiveness;
    /// council-routed). Reserved for cases where no existing
    /// note is the right place to put a single bridge link.
    AccidentalNote,
}

/// The two shapes a bridge proposal can take.
///
/// Serde's `#[serde(untagged)]` makes the JSON identical to the
/// prompt's documented schema (no `kind` discriminator field) —
/// the variant is picked by field presence at parse time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProposedBridge {
    /// A new wikilink added to an existing note that reaches
    /// into the other cluster. Lowest-invasiveness option;
    /// preferred when a specific anchor will do.
    Link {
        /// The note that receives the new link.
        source_note_id: String,
        /// The note in the other cluster being linked to.
        target_note_id: String,
        /// The anchor text for the new link.
        anchor_text: String,
        /// One sentence: why this specific link reaches across
        /// the gap meaningfully.
        justification: String,
    },
    /// A new bridge note that explicitly connects two clusters.
    /// Higher invasiveness — council-routed regardless of
    /// confidence per the prompt + config.
    Note {
        /// Title of the new bridge note.
        title: String,
        /// Kebab-case filename per ADR 0006.
        slug: String,
        /// Body (2–4 paragraphs) that names the conceptual
        /// overlap and links into both clusters.
        body: String,
        /// Notes in cluster A the bridge note links to.
        cluster_a_anchor_note_ids: Vec<String>,
        /// Notes in cluster B the bridge note links to.
        cluster_b_anchor_note_ids: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_OUTPUT: &str = r#"{
        "confidence": 0.79,
        "rationale": "Three cluster pairs analyzed: two are meaningfully disconnected (one woodworking + one Rust; one personal-journal + one technical-writing) and one is an accidental gap (the rate-distortion cluster and the editing cluster both reach for lossy compression).",
        "cluster_pair_verdicts": [
            {
                "cluster_a_id": "c-rate-distortion",
                "cluster_b_id": "c-editing",
                "verdict": "accidental_link",
                "reasoning": "Both clusters reach for lossy compression; 01H8X9 (in c-rate-distortion) has an explicit anchor that fits 01H8XA in c-editing.",
                "proposed_bridge": {
                    "source_note_id": "01H8X9",
                    "target_note_id": "01H8XA",
                    "anchor_text": "rate-distortion",
                    "justification": "01H8X9's rate-distortion analogy directly informs 01H8XA's claim about editor choice."
                }
            },
            {
                "cluster_a_id": "c-woodworking",
                "cluster_b_id": "c-rust",
                "verdict": "meaningful",
                "reasoning": "Two genuinely unrelated topics; the author maintains them as separate projects."
            },
            {
                "cluster_a_id": "c-systems-design",
                "cluster_b_id": "c-knowledge-systems",
                "verdict": "accidental_note",
                "reasoning": "Both clusters circle the same abstraction (feedback loops in self-improving systems) without anyone note being the right place to put a single link.",
                "proposed_bridge": {
                    "title": "Feedback loops in self-improving systems",
                    "slug": "feedback-loops-self-improving-systems",
                    "body": "Both software design and knowledge management depend on...",
                    "cluster_a_anchor_note_ids": ["01H9A1", "01H9A2"],
                    "cluster_b_anchor_note_ids": ["01H9B1", "01H9B2"]
                }
            }
        ]
    }"#;

    #[test]
    fn parses_representative_output() {
        let parsed: BridgeBuilderOutput =
            serde_json::from_str(SAMPLE_OUTPUT).expect("sample JSON must parse");
        assert_eq!(parsed.cluster_pair_verdicts.len(), 3);

        let v0 = &parsed.cluster_pair_verdicts[0];
        assert_eq!(v0.verdict, BridgeVerdict::AccidentalLink);
        match v0.proposed_bridge.as_ref().expect("bridge present") {
            ProposedBridge::Link {
                source_note_id,
                anchor_text,
                ..
            } => {
                assert_eq!(source_note_id, "01H8X9");
                assert_eq!(anchor_text, "rate-distortion");
            }
            ProposedBridge::Note { .. } => panic!("expected Link variant"),
        }

        let v1 = &parsed.cluster_pair_verdicts[1];
        assert_eq!(v1.verdict, BridgeVerdict::Meaningful);
        assert!(v1.proposed_bridge.is_none());

        let v2 = &parsed.cluster_pair_verdicts[2];
        assert_eq!(v2.verdict, BridgeVerdict::AccidentalNote);
        match v2.proposed_bridge.as_ref().expect("bridge present") {
            ProposedBridge::Note {
                cluster_a_anchor_note_ids,
                cluster_b_anchor_note_ids,
                ..
            } => {
                assert_eq!(cluster_a_anchor_note_ids.len(), 2);
                assert_eq!(cluster_b_anchor_note_ids.len(), 2);
            }
            ProposedBridge::Link { .. } => panic!("expected Note variant"),
        }
    }

    #[test]
    fn round_trips_via_serde_json() {
        let parsed: BridgeBuilderOutput = serde_json::from_str(SAMPLE_OUTPUT).expect("parse");
        let re_serialized = serde_json::to_string(&parsed).expect("serialize");
        let re_parsed: BridgeBuilderOutput =
            serde_json::from_str(&re_serialized).expect("re-parse");
        assert_eq!(parsed, re_parsed);
    }

    /// All three verdicts round-trip.
    #[test]
    fn all_verdicts_parse() {
        for (json_value, expected) in [
            ("meaningful", BridgeVerdict::Meaningful),
            ("accidental_link", BridgeVerdict::AccidentalLink),
            ("accidental_note", BridgeVerdict::AccidentalNote),
        ] {
            let parsed: BridgeVerdict =
                serde_json::from_str(&format!("\"{json_value}\"")).expect("parse");
            assert_eq!(parsed, expected);
        }
    }

    /// `deny_unknown_fields` is the schema-drift guardrail. Note:
    /// `serde(untagged, deny_unknown_fields)` on the enum
    /// applies the rejection to each variant body individually —
    /// a stray field in the Link variant won't accidentally
    /// match the Note variant.
    #[test]
    fn unknown_fields_rejected_on_outer() {
        let extra = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "cluster_pair_verdicts": [],
            "future_field": "should not parse"
        }"#;
        let err = serde_json::from_str::<BridgeBuilderOutput>(extra)
            .expect_err("unknown field must fail");
        assert!(
            err.to_string().contains("future_field"),
            "error message should point at the offending field; got: {err}"
        );
    }

    /// Untagged enum with `deny_unknown_fields` on each variant
    /// means a malformed `proposed_bridge` (neither shape) fails
    /// to parse rather than silently matching either variant.
    #[test]
    fn malformed_bridge_rejected() {
        let bad = r#"{
            "confidence": 0.5,
            "rationale": "ok",
            "cluster_pair_verdicts": [
                {
                    "cluster_a_id": "a",
                    "cluster_b_id": "b",
                    "verdict": "accidental_link",
                    "reasoning": "...",
                    "proposed_bridge": {
                        "source_note_id": "x",
                        "extra": "neither variant"
                    }
                }
            ]
        }"#;
        assert!(
            serde_json::from_str::<BridgeBuilderOutput>(bad).is_err(),
            "malformed proposed_bridge must fail to parse"
        );
    }

    /// ADR 0011 streaming-order contract: confidence < rationale
    /// < cluster_pair_verdicts.
    #[test]
    fn serializes_confidence_first() {
        let out = BridgeBuilderOutput {
            confidence: 0.9,
            rationale: "r".into(),
            cluster_pair_verdicts: vec![],
        };
        let json = serde_json::to_string(&out).expect("serialize");
        let conf_idx = json.find("\"confidence\"").expect("present");
        let rat_idx = json.find("\"rationale\"").expect("present");
        let verdicts_idx = json
            .find("\"cluster_pair_verdicts\"")
            .expect("present");
        assert!(
            conf_idx < rat_idx && rat_idx < verdicts_idx,
            "field order must be confidence < rationale < cluster_pair_verdicts \
             (got {conf_idx}, {rat_idx}, {verdicts_idx})"
        );
    }

    /// An all-meaningful verdict set (every cluster pair
    /// declines) is the most common shape in a healthy vault.
    /// The prompt's "default to decline" invariant means this is
    /// the happy path, not the empty path.
    #[test]
    fn all_meaningful_verdicts_parse() {
        let all_decline = r#"{
            "confidence": 0.88,
            "rationale": "Healthy vault; all four cluster pairs are meaningfully disconnected.",
            "cluster_pair_verdicts": [
                {"cluster_a_id": "a", "cluster_b_id": "b", "verdict": "meaningful", "reasoning": "..."},
                {"cluster_a_id": "c", "cluster_b_id": "d", "verdict": "meaningful", "reasoning": "..."}
            ]
        }"#;
        let parsed: BridgeBuilderOutput = serde_json::from_str(all_decline).expect("parse");
        assert!(parsed
            .cluster_pair_verdicts
            .iter()
            .all(|v| matches!(v.verdict, BridgeVerdict::Meaningful)));
        assert!(parsed
            .cluster_pair_verdicts
            .iter()
            .all(|v| v.proposed_bridge.is_none()));
    }
}
