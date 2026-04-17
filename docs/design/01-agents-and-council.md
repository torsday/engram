# Agents and the Council

## Design philosophy

Agents are not assistants. They are specialized roles in a system that maintains and challenges a knowledge base. Each agent has a narrow job, a clear boundary on what it may touch, and a defined relationship to the evergreen rubric. They collaborate through a typed deliberation protocol, not free-form conversation.

The swarm is diverse by design. Some agents build; some agents question; some agents clean up. The tension between them is the point --- a vault that only accumulates without pruning rots, and a vault that only prunes without synthesis stagnates.

**Roster size.** The full design includes **~35 agents** (the autonomous roster, organized below by tier) plus 4 on-demand orchestrators (Research Council, Debate Mode, Conversation Prep, Untangler). The v1 set is 5 (Linker, Gardener, Cartographer, Scribe, Ingestor); subsequent phases add the rest per `07-roadmap.md`. The exact count fluctuates over time as Auditor recommends additions, consolidations, or retirements --- "~35" is the current canonical figure to use in any cross-reference.

## Confidence-gated autonomy

Every agent action is gated by **two layers of safety**:

**Layer 1: confidence threshold.** Each agent self-assesses confidence (0.0--1.0) on every proposed action. Each agent's `config.toml` specifies an `auto_land_min_confidence` threshold for autonomous action.

- **Confidence ≥ threshold:** The agent writes the change directly to the working tree (still subject to invasiveness ceilings --- see below).
- **Confidence < threshold:** The change becomes an explicit proposal that goes through the council deliberation flow.

How confidence is computed:

- **LLM-emitted self-score:** Every structured agent output includes a `confidence` field. The agent is prompted to be honest: "rate your confidence; the system rewards calibration, not bravado."
- **Retrieval-signal agreement:** Did multiple retrieval methods (BM25, dense, graph) converge on the same answer? Agreement raises confidence; conflict lowers it.
- **Calibration adjustment:** Watcher tracks claimed confidence vs. actual acceptance per agent. If Linker claims 0.9 confidence on 100 changes and only 70% are accepted, Watcher flags miscalibration; the agent's prompt is tuned (via prompt evolution) to be more conservative. **The system rewards calibration, not optimism.**

**Layer 2: git is the safety net.** No matter how confident an agent is, **agents never run `git add` or `git commit`.** All agent writes land in the working tree as unstaged changes. The user is the only entity that stages or commits. `git diff` is always the review surface. `git restore` is always the revert.

This is the core promise of engram's autonomy model: agents can be aggressive within their working-tree sandbox precisely *because* nothing they do reaches history without the human's deliberate `git add`.

### Invasiveness ceilings

Even with high confidence, agents have invasiveness ceilings beyond which they cannot autonomously write --- these always go through council and human approval:

| Invasiveness  | Examples                                              | Path                                                  |
| ------------- | ----------------------------------------------------- | ----------------------------------------------------- |
| Mechanical    | Dead link fix, tag normalization, index refresh       | Confidence-gated autonomous write (working tree)      |
| Additive      | New link, backlink section, inline annotation         | Confidence-gated autonomous write (working tree)      |
| Editorial     | Note rewrite, section restructure, content change     | Council deliberation; if convergent, written unstaged |
| Structural    | New evergreen note, note merge, note split, deletion  | Council + explicit human approval before write        |

The ceiling per agent is set in `config.toml` via `max_invasiveness`. An agent's `auto_land_min_confidence` only applies to actions at or below its ceiling.

---

## Agent roster

### Maintenance agents

These keep the vault structurally healthy. They run frequently (on file-change triggers or short schedules) and mostly auto-land their changes.

#### Linker

- **Job:** Discover missing wikilinks between notes. Propose bidirectional connections.
- **Trigger:** New or modified note.
- **Output:** Modified notes with new `[[wikilinks]]` inserted at appropriate locations.
- **Invasiveness:** Low. New links auto-land after council spot-check. Removed links require human approval.
- **Tools:** Semantic search, graph traversal, alias resolution.

#### Gardener

- **Job:** Prune stale content. Fix dead links. Remove resolved TODOs. Flag notes that have decayed below the evergreen rubric.
- **Trigger:** Scheduled (daily sweep).
- **Output:** Modified notes (dead link removal, TODO cleanup). Status changes (`status: needs-review`). Proposed deletions (human-approved only).
- **Invasiveness:** Medium. Cleanup auto-lands. Deletions and status demotions require human approval.
- **Tools:** Link graph, vault-wide grep, age/staleness heuristics.

#### Cartographer

Two modes: continuous maintenance + quarterly deep audit (formerly the Taxonomist agent, now folded in).

- **Job (continuous mode):** Maintain MOCs, the tag taxonomy, and `index.md` (Karpathy-style one-line-per-note index). Ensure navigation structure reflects the vault's actual shape.
- **Job (audit mode):** Quarterly holistic analysis of the tag system. Identify synonym tags (`#ml` vs `#machine-learning`), orphan tags (used once), missing hierarchy levels, inconsistent naming conventions. Propose coherent ontology restructuring.
- **Trigger:** Continuous mode runs on new note, deleted note, significant restructuring. Audit mode runs quarterly or on-demand.
- **Output:** (continuous) Updated MOC notes, regenerated `index.md`, tag normalization proposals. (audit) A tag audit report + proposed renames/merges/hierarchy changes as a structured diff.
- **Invasiveness:** Low for continuous-mode index/MOC updates (auto-land). Tag renames and audit-mode restructuring proposals require council deliberation + human approval (vault-wide changes).
- **Tools:** Tag index, link graph, note metadata, embedding similarity (for detecting semantic synonym tags in audit mode).

#### Historian

- **Job:** Maintain a human-readable activity log. Weekly digest of what changed, which agents acted, what was approved/rejected/shelved.
- **Trigger:** Scheduled (weekly).
- **Output:** `reflections/YYYY-WNN.md` --- a changelog note.
- **Invasiveness:** None. Creates new notes only, never modifies existing ones.
- **Tools:** Git log, deliberation transcripts, agent run metadata.

### Processing agents

These transform raw input into structured vault content.

#### Scribe

- **Job:** Clean up fleeting notes (quick captures, voice transcripts) into well-formatted markdown. Format literature notes from extracted content.
- **Trigger:** New note with `type: fleeting` or new extraction output.
- **Output:** Rewritten note body, improved frontmatter, title suggestions.
- **Invasiveness:** High for fleeting notes (may rewrite freely --- the original is in git). Medium for literature notes (formatting only, not editorial). Never touches evergreen notes.
- **Tools:** Markdown AST, frontmatter parser, spell/grammar check.

#### Ingestor

- **Job:** Receive dropped files, classify them, dispatch extractors, produce literature notes.
- **Trigger:** File dropped via Swift app / CLI / API.
- **Output:** Artifact stored in `.engram/artifacts/`. Literature note in `notes/literature/`. Extraction metadata in sqlite.
- **Invasiveness:** Creates new content only. Literature notes enter the review queue for approval before landing.
- **Tools:** File classifier, PDF/image/audio extractors, Claude vision API, whisper.cpp.
- **See:** `02-ingestion.md` for the full pipeline.

#### Inbox Triage

- **Job:** For new fleeting notes (quick captures, voice transcripts, share-sheet drops), suggest a classification and downstream routing. Reduces friction at the highest-volume entry point.
- **Trigger:** New note with `type: fleeting` (after Scribe's cleanup pass).
- **Output:** A proposed `triage:` frontmatter field with one of:
  - `keep-fleeting` --- a passing observation worth keeping in the inbox; no further action
  - `promote-literature` --- looks like quoted/source material; route through literature-note formatting
  - `promote-evergreen-candidate` --- looks like a half-formed concept worth developing; mark `status: candidate-evergreen`
  - `merge-into:<note-id>` --- redundant with an existing note; suggest merge
  - `discard` --- no apparent value (typos, accidental captures, duplicates)
- **Invasiveness:** Adds a frontmatter suggestion. Routing requires human approval in the Swift app review queue. Discards are never automatic --- always proposed.
- **Tools:** Note-shape classifier, semantic search (for redundancy detection), evergreen rubric checker.

#### Curator

- **Job:** Digest existing external note corpora (e.g. an old Obsidian vault that has grown unwieldy) into engram. Different from Ingestor: Ingestor handles individual dropped files; Curator handles whole corpora and is **willing to discard aggressively.**
- **Trigger:** User-initiated: `engram digest <path>` or via Swift app.
- **Output:** A digestion plan, then per-batch proposed dispositions: keep-as-evergreen-draft, keep-as-literature, merge-into-existing, archive-only, discard. Operates over weeks for large corpora; resumable.
- **Invasiveness:** High in volume (many proposed notes), but always reviewable in batches. Original external corpus is read-only and untouched. Discards never delete the source --- only the user does.
- **Tools:** Hybrid retrieval, embedding clustering, Synthesizer (sub-agent), Merger (sub-agent), evergreen rubric checker.
- **See:** `05-corpus-digestion.md` for the full pipeline and curation policy.

### Structural agents

These reason about the vault's shape --- not individual notes, but the topology of the whole graph.

#### Splitter

- **Job:** Identify notes that violate atomicity --- notes that are really 2--3 ideas in one file. Propose specific splits: "This note becomes [[A]], [[B]], and [[C]], with these links between them, and these incoming links redistributed."
- **Trigger:** Scheduled (weekly sweep of notes above a length/complexity threshold), or invoked by council when evaluating a note against the evergreen rubric.
- **Output:** A structured split proposal: new note drafts + link redistribution plan + proposed redirects from the original.
- **Invasiveness:** High. Creates new notes and modifies link graph. Always goes through full council deliberation + human approval.
- **Tools:** Markdown AST (heading/section analysis), semantic segmentation (embedding each section and measuring inter-section similarity), link graph, evergreen rubric checker.

#### Merger

- **Job:** Find notes about the same concept written at different times, possibly with different titles. Propose a unified note that preserves the best of both, with the originals becoming aliases or absorbed.
- **Trigger:** Scheduled (weekly), or invoked when the Linker detects high bidirectional similarity between two notes.
- **Output:** A merge proposal: unified draft + provenance history combining both originals + ID alias mapping so incoming links reroute cleanly.
- **Invasiveness:** High. Modifies or removes existing notes. Always goes through full council deliberation + human approval.
- **Tools:** Embedding similarity, content diffing, link graph, stable ID alias system.

#### Bridge Builder

- **Job:** Run community detection on the link graph. Find clusters that are internally dense but disconnected from each other. Determine whether disconnection is meaningful (genuinely unrelated topics) or accidental (the author never linked them). For accidental gaps, propose bridge links or bridge notes.
- **Trigger:** Scheduled (monthly).
- **Output:** A graph analysis report showing isolated clusters. For each accidental gap: proposed bridge links between existing notes, or a proposed bridge note that explicitly connects two clusters.
- **Invasiveness:** Low for bridge links (similar to Linker). Medium for bridge notes (new content). Bridge links go through council spot-check; bridge notes go through full council + human approval.
- **Tools:** Link graph (community detection: Louvain or label propagation), semantic search (to validate whether clusters are semantically related despite being unlinked), note metadata.

### Thinking agents

These generate intellectual value. They run less frequently, produce more consequential output, and always go through council deliberation.

**Critical agents (Devil's Advocate, Heretic, Socratic Prober) are held to the same epistemic standard as constructive ones.** Sloppy disagreement is no more acceptable than sloppy synthesis. See the rationality gate below.

#### The rationality gate (for critical agents)

Any agent producing a critique, counter-argument, or challenge must pass output through Steelman before it can land. Steelman applies five criteria; **all** must hold:

1. **Engages the actual claim** --- addresses what the original note says, not a strawman simplification.
2. **Uses real evidence** --- cites vault content or verifiable external sources, not pure assertion or rhetorical flourish.
3. **Internally consistent** --- the counter-position is a coherent alternative, not just negation.
4. **Has real-world adherents** --- a thinker the user would respect could plausibly hold this view, even if the user disagrees.
5. **Concedes what's true** --- acknowledges what the original got right before challenging it. A critique that pretends the original has zero merit fails immediately.

If all five hold, the critique lands. If not, two outcomes:

- **Returned for revision** --- Steelman explains which criteria failed; the critical agent rewrites with those objections in hand. Maximum one revision round.
- **Shelved with note** --- after revision, if criteria still fail, the critique is shelved with the explicit note: "No defensible critique found." This is itself useful information --- the note being challenged is robust to attack at this level. Recorded in the deliberation transcript.

The gate is enforced by Steelman in council; it is not optional and not bypassable by trust score. Critique without rigor is worse than no critique --- it trains the user to dismiss criticism as noise.

#### Synthesizer

- **Job:** Identify clusters of related notes and propose new evergreen notes that name the concept the cluster circles around.
- **Trigger:** Scheduled (weekly), or on-demand via Research Council.
- **Output:** Proposed new notes with links to the source cluster.
- **Invasiveness:** High. Always goes through full council deliberation + human approval.
- **Tools:** Embedding-based clustering, semantic search, link graph.

#### Devil's Advocate

- **Job:** Argue against claims in notes. Surface counter-evidence. Identify unstated assumptions. **All output must pass the rationality gate (see below) --- contrarianism for its own sake is rejected.**
- **Trigger:** Participates in council deliberation when invoked. Also runs on-demand against specific notes.
- **Output:** Inline annotations (as HTML comments with provenance), or standalone critique notes linked to the target.
- **Invasiveness:** Medium. Annotations auto-land (they're additive and clearly attributed) **only after passing the rationality gate**. Standalone critique notes go through review.
- **Tools:** Semantic search (for contradicting evidence within the vault), web search (gated, for external counter-evidence). Steelman as a mandatory gate.

#### Contradiction Detector

- **Job:** Scan the vault for claims that conflict with each other across notes. Surface pairs where the author disagrees with their past self.
- **Trigger:** Scheduled (weekly).
- **Output:** A report note listing contradicting pairs with links and quotes. Optionally proposes `status: needs-review` on the weaker note.
- **Invasiveness:** Low. Report notes are additive. Status changes require human approval.
- **Tools:** Embedding similarity (for finding notes about the same topic), LLM-based claim extraction and comparison.

#### Socratic Prober

- **Job:** Stress-test notes before they earn `status: evergreen`. Ask: What's the strongest counterargument? What evidence would change your mind? Is this actually two claims?
- **Trigger:** Note marked `status: candidate-evergreen` (explicit promotion request).
- **Output:** A set of questions appended to the note as a `## Probe` section. Once the human addresses them and confirms, status advances to `evergreen`.
- **Invasiveness:** Medium. Adds a section to the note (clearly attributed). Does not modify existing content. The human decides when probing is complete.
- **Tools:** Semantic search, Devil's Advocate (may invoke as a sub-agent), evergreen rubric checker.

#### Analogist

- **Job:** Find structural parallels between ideas in different domains. Not keyword overlap --- structural similarity. "Your note on [[Rate-distortion theory]] in information theory has a structural parallel to [[Editing as compression]] in your writing notes --- both are about lossy reduction under constraints."
- **Trigger:** Scheduled (weekly), or invoked by Research Council for cross-domain exploration.
- **Output:** An analogy report: pairs of notes from different domains with a one-paragraph explanation of the structural parallel. Optionally proposes a new bridge note naming the shared abstraction.
- **Invasiveness:** Low for reports (additive). Medium for proposed bridge notes (council + human approval).
- **Tools:** Embedding similarity (at the argument-structure level, not just content), tag-based domain detection, semantic search. Uses `deep` model tier --- this is genuinely hard reasoning.
- **Note:** This is the hardest agent to build well and potentially the most valuable. Cross-domain transfer is where the best ideas live.

#### Steelman

- **Job (two roles):**
  1. **Constructive role:** Take weak or tentative notes (`status: draft`, hedging language, few links) and make the strongest possible case for them. Find supporting evidence, propose stronger framings.
  2. **Gate role:** Serve as the mandatory rationality gate for all critical agents (Devil's Advocate, Heretic, Socratic Prober). Apply the five-criterion test (see rationality gate above) to any critique before it can land.
- **Trigger:** Participates in council deliberation whenever a critique is being evaluated. Also runs on-demand against specific notes for the constructive role.
- **Output:** (constructive) Inline annotations with supporting evidence + proposed reframings. (gate) Pass/revise/shelve verdict with criterion-level explanation when failed.
- **Invasiveness:** Medium for constructive output (annotations auto-land, reframings go through council). The gate role itself is structural --- Steelman's gate verdict is binding on critical agents.
- **Tools:** Semantic search, link graph, web search (gated, for external supporting evidence).

#### Assumption Excavator

- **Job:** Read evergreen notes and surface unstated premises. "This note claims X, which implicitly assumes Y, but Y is never stated or defended anywhere in the vault. Should there be a note for Y?"
- **Trigger:** Scheduled (monthly rotation through evergreen notes), or invoked by Socratic Prober as a sub-agent.
- **Output:** Per-note list of extracted assumptions. For important unstated assumptions: a proposal to create an explicit note. For assumptions that contradict other vault content: a flag to the Contradiction Detector.
- **Invasiveness:** Low for assumption reports (additive). Medium for proposed notes (council + human approval).
- **Tools:** Semantic search, claim extraction, logic-chain analysis. Uses `deep` model tier.

#### Inquirer

Consolidates the work of three previously-separate agents (Interlocutor, Prompt Drafter, Question Generator) and the negative-space analysis previously done by Blindspot Finder. One agent, one prompt skeleton, four modes selected by trigger.

- **Job:** Generate good questions about the vault from multiple vantage points. Questions are the most valuable cheap output a knowledge system can produce.
- **Modes:**
  1. **`daily-reactive`** --- triggered at end-of-day if notes were modified. Reads the day's changes, asks one question that connects them to the vault's history. Output: a single inbox note.
  2. **`seed-empty-note`** --- triggered when a new note is created with empty body. Inserts 3--5 questions seeded from semantic neighbors. Marked as prompts; auto-deleted after 48 hours if unused.
  3. **`holistic-gap`** --- scheduled weekly. Reads the vault as a whole; identifies tensions, unexplored intersections, and questions the vault can't currently answer. Output: a `questions/YYYY-WNN.md` note with 3--5 questions and motivating links.
  4. **`blindspot`** --- scheduled quarterly. Negative-space analysis: concepts mentioned but undeveloped, authors cited but unexamined, domains adjacent to the vault's interests but absent. Output: `reflections/blindspots-YYYY-QN.md`. Feeds into the next `holistic-gap` run.
- **Output (all modes):** Question notes only. Never modifies existing content.
- **Invasiveness:** None. Inbox-only.
- **Tools:** Semantic search, link graph, topic clustering, recent-changes, citation extraction (for blindspot mode).

#### Confidence Annotator

- **Job:** Find claims in evergreen notes that lack explicit confidence markers ("I think," "highly likely," "uncertain," "I'm 80% sure"). Flag them. Optionally propose a marker. Make implicit confidence explicit.
- **Trigger:** Scheduled (monthly rotation through evergreen notes), or on-demand against a specific note.
- **Output:** Inline HTML-comment flags ("`<!-- by: confidence-annotator: needs explicit confidence -->`") on flagged claims. A summary report listing all flags. Never edits the prose itself --- only marks.
- **Invasiveness:** Low. Annotations are additive and visually quiet. Auto-lands.
- **Tools:** Claim extraction, confidence-language detection. Pairs with Predictor (which logs the explicit confidence claims that result).

#### Source Demand

- **Job:** Find factual claims in evergreen notes that lack citations. Ask for them. Pairs with Confidence Annotator: that one demands epistemic markers, this one demands evidence.
- **Trigger:** Scheduled (monthly), or on-demand.
- **Output:** Inline HTML-comment flags on uncited factual claims. Summary report. Where a likely source can be found in the vault's literature notes, proposes the citation explicitly.
- **Invasiveness:** Low. Annotations are additive. Auto-lands.
- **Tools:** Claim classification (factual vs. opinion), literature-note search, web search (gated, for proposing external sources).

#### Pair-Thinking

- **Job:** Live writing collaborator. When the user is actively writing in a note, asks one clarifying or probing question per completed paragraph (or every N seconds of inactivity). Bounded session (3--5 rounds). Output is the strengthened draft itself, not a separate critique.
- **Trigger:** Manual activation by user (Swift app side panel or Obsidian command). Watches the file via the file watcher; engages on paragraph-completion heuristics (sentence-ending punctuation + brief pause).
- **Output:** Conversational turns delivered to the Swift app side panel. The user's responses are folded into the note as the user chooses. Session transcript stored in `.engram/deliberations/` for reference.
- **Invasiveness:** None on the vault directly --- the user writes; Pair-Thinking only asks. Never auto-inserts content.
- **Tools:** Hybrid retrieval, recent-changes, conversation engine. Uses `standard` model tier. Conversation-mode agent (see system features).
- **Why this is different from Socratic Prober:** Socratic Prober runs at evergreen-promotion time on a finished note. Pair-Thinking runs during drafting. The first stress-tests; the second collaborates. Both use the same conversation infrastructure.

#### Heretic

- **Job:** Periodically pick an evergreen note and write a serious, sustained counter-argument as its own note --- but only when a defensible counter-position genuinely exists. **Critical thinking, not contrarianism.** Different from Devil's Advocate (one-off critique inside a deliberation) --- Heretic produces a full alternative view that lives in the vault as a permanent challenge.
- **Trigger:** Scheduled (monthly, rotating through evergreen notes weighted by age and link density).
- **Output:** A new note titled `[[Against: <original title>]]` with frontmatter `type: heretical`, linked bidirectionally with the original. The original note's body gains a "Challenged by" section pointing at the heretical counterpart.
- **Quality requirement:** Output **must pass the rationality gate (see below).** If no defensible counter-position exists for a given note, Heretic shelves the attempt and records "no defensible counter-position found" in the deliberation log --- which is itself useful information about the original (it's robust).
- **Invasiveness:** High. Always goes through full council deliberation + human approval. Steelman serves as the rationality gate (mandatory). Devil's Advocate participates as a critic of the heretical note itself (recursive critique).
- **Tools:** Semantic search, web search (gated, for external counter-evidence), claim extraction, argumentation, Steelman as gate. Uses `deep` model tier --- this is genuinely hard work.

### Personal agents

These model the user themselves --- not the vault's content, but the person writing it. Their outputs are read by other agents to ground their work in a coherent picture of who they're working for.

#### Biographer

- **Job:** Maintain the system's evolving model of who the user is: interests, beliefs, expertise areas, characteristic positions, recurring themes, blind spots, intellectual commitments. Update monthly based on the vault's drift over time.
- **Trigger:** Scheduled (monthly).
- **Output:** A single note `meta/biography.md`, structured: `## Identity`, `## Domains of expertise`, `## Recurring themes`, `## Stated commitments`, `## Open questions`, `## Drift since last update`.
- **Invasiveness:** Maintains one specific note. Always goes through human approval --- this note is a model of you and you should curate it.
- **Tools:** Semantic search, topic clustering, frontmatter analysis, git log (to track drift), provenance filtering (human-written only).
- **Read by:** Most other agents inject this note into their context. Without it, agents work in a vacuum about who they're working for.

#### Voice Keeper

- **Job:** Learn the user's writing voice --- tone, vocabulary, sentence rhythm, characteristic moves, things the user says and things they would never say. When other agents draft content (Synthesizer, Scribe, Steelman, Heretic), Voice Keeper checks output against the learned voice and flags or rewrites passages that don't sound like the user.
- **Trigger:** Participates in council deliberation when any drafted content is being considered. Also runs monthly to update its voice model.
- **Output:** Voice model stored in `.engram/meta/voice-model.md` (human-readable, editable). Critique annotations on agent-drafted content. Optionally rewrites passages to match voice (with original preserved in deliberation transcript).
- **Invasiveness:** Medium. Voice model updates require human approval. Rewrite suggestions go through council. The original drafting agent always retains authorship --- Voice Keeper edits, doesn't replace.
- **Tools:** Style analysis (sentence length distributions, vocabulary frequency, characteristic patterns), embedding-based voice fingerprinting, LLM-based stylistic critique. Uses `standard` model tier.
- **Why this matters:** Without Voice Keeper, the vault gradually homogenizes toward a generic LLM voice as agents do more writing. This is the safety mechanism that protects "rewrites itself" from eroding the user's authorial identity.

#### Witness

- **Job:** For personal/journal-like notes (`type: fleeting` with `tag: personal`, or `type: journal`), simply acknowledge. No analysis, no suggestions, no therapy LARPing. Provide the experience of being read without being judged.
- **Trigger:** New note with personal/journal classification.
- **Output:** A short, gentle acknowledgment in a private inbox (`.engram/witness/YYYY-MM-DD.md`). Never modifies the original. Never in the main vault. Never accessible to other agents.
- **Invasiveness:** None on the vault. Read-only with one-way private acknowledgment.
- **Tools:** No retrieval. No search across other notes. Witness intentionally has no memory and no context beyond the single note in front of it.
- **Privacy:** Witness always uses local-only model regardless of vault config. Personal notes never touch cloud LLMs through this agent.

### Temporal agents

These treat time as a first-class dimension of the vault. They surface patterns and evolution that are invisible when reading notes individually.

#### Predictor

Now consolidated with the calibration analysis previously done by a separate Calibration Tracker agent. One agent, two artifacts.

- **Job:** Find notes where the user made predictions ("I think X will happen by Y," "this approach won't scale," "I expect Z within 6 months") or made claims with explicit confidence levels ("I'm 70% sure...", "highly likely..."). Track them. Surface ones that have come due. Compute calibration over time.
- **Trigger:** Scheduled (weekly scan for new predictions and confidence claims; daily check for predictions coming due; monthly recompute of calibration profile).
- **Outputs (two):**
  - `meta/predictions.md` --- structured ledger of predictions and confidence claims with status (`pending`, `resolved-correct`, `resolved-incorrect`, `unresolved`, `superseded`). When predictions come due, surfaced in the review queue: "You predicted X by [date]. Did it happen?"
  - `meta/calibration.md` --- calibration profile showing claimed-confidence vs. actual-accuracy, broken down by topic area. Includes Brier score over time and per-domain calibration strength.
- **Invasiveness:** Maintains two specific notes. Resolution requires human input.
- **Tools:** Pattern matching (prediction- and confidence-language detection), date extraction, semantic search for resolution evidence, web search (gated, for external resolution), statistical analysis. Pairs powerfully with Confidence Annotator (which makes implicit confidence explicit, feeding more material into the ledger).

#### Annual Review

- **Job:** Once per year, produce a long-form narrative reflection on the vault's evolution. Read everything: git log, all major evergreens, deliberation history, Historian digests, the diachronic concept-trajectory feature output, the year's wins and abandoned threads. Surface themes, evolution, what crystallized vs. what was abandoned, key insights, intellectual milestones.
- **Trigger:** Scheduled (annually, on a date the user picks --- New Year's Eve, anniversary of first note, etc.). Also on-demand.
- **Output:** A long-form note `reflections/annual/YYYY.md` written in the Vault's voice (or the user's, if Voice Keeper has a strong model). Probably the most emotionally resonant artifact the system produces.
- **Invasiveness:** Creates one note per year. Always goes through human approval --- this is a personal artifact and the user should curate.
- **Tools:** Hybrid retrieval, git log, all available longitudinal data, Voice Keeper model. Uses `deep` model tier.

> **Note on Trajectory:** Diachronic concept evolution (formerly the Trajectory Tracer agent) is now a **feature**, not an agent. It is exposed as the `engram trace <concept>` CLI command and the `trace_concept` tool in both internal and external MCP. It runs on demand only --- no scheduling, no agent overhead. See `03-architecture.md` for the tool definition.

### Pedagogical agents

These turn the vault from a passive archive into an active learning system.

#### Tutor

- **Job:** Generate spaced-repetition flashcards from evergreen notes. Track what the user has reviewed. Surface forgotten material on a schedule (SM-2 / FSRS algorithm).
- **Trigger:** Scheduled (daily, to surface due cards). Also runs when a note is promoted to `status: evergreen` (to generate initial cards).
- **Output:** Flashcards stored in `.engram/flashcards/<note-id>.md` (one file per note, multiple cards). Daily review queue surfaced in the Swift app: "5 cards due today." Cards are markdown so they're versioned and human-editable.
- **Invasiveness:** Creates flashcard files in `.engram/`. Never modifies vault notes. Card creation goes through brief human spot-check (edit-or-approve) per note --- cards represent your own knowledge as the system understands it.
- **Tools:** Claim extraction (for question generation), evergreen rubric checker (only generates cards for solid evergreens), spaced-repetition scheduler.

### External agents

These connect the vault to the outside world. They require network access and run on longer schedules.

#### Scout

- **Job:** Monitor external sources for content relevant to the vault's interests. Configurable feeds: RSS, Arxiv categories, specific blogs, Hacker News keywords, Twitter/Bluesky accounts. When relevant content is found, run the ingestion pipeline automatically.
- **Trigger:** Scheduled (configurable per-feed, default every 6 hours).
- **Output:** Artifacts stored + literature notes drafted + connections to existing vault content proposed. All enter the review queue. The vault becomes a living antenna, not just a warehouse.
- **Invasiveness:** Medium. Creates new content only, but automatically. All literature notes require human approval before landing. The user controls signal-to-noise via feed configuration and relevance thresholds.
- **Tools:** RSS/Atom parser, web fetcher, relevance scorer (embed feed item against vault topic centroid), ingestion pipeline.
- **Config extras:**

  ```toml
  [[scout.feeds]]
  name = "arxiv-cs-ai"
  type = "rss"
  url = "https://arxiv.org/rss/cs.AI"
  check_interval = "6h"
  relevance_threshold = 0.7

  [[scout.feeds]]
  name = "favorite-blog"
  type = "rss"
  url = "https://example.com/feed.xml"
  check_interval = "12h"
  relevance_threshold = 0.5  # lower threshold = cast wider net
  ```

#### Fact Checker

- **Job:** Take claims in evergreen notes and verify them against current external sources. Identify findings that have been superseded, retracted, or contradicted by newer research. Subsumes the simpler "Decay Detector" concept.
- **Trigger:** Scheduled (monthly, rotating through a subset of evergreen notes).
- **Output:** `status: needs-review` flag on notes with outdated claims, with a one-line reason and source link. A monthly `meta/fact-check-YYYY-MM.md` report.
- **Invasiveness:** Low. Status flags require human approval. Report notes are additive.
- **Tools:** Web search (gated, scheduled, privacy-respecting), citation extraction, date-aware source comparison. Uses `standard` model tier.

### Meta agents

These monitor and evaluate the system itself. Together they form the auditing layer that lets the user run the swarm without continuous babysitting. Watcher is the **continuous monitor** (numerical, weekly); Auditor is the **deep evaluator** (qualitative, quarterly); both feed into budget enforcement and auto-retirement.

#### Watcher

- **Job:** Continuous numerical monitoring of every agent. Track acceptance, rejection, revert (approved-then-undone), survival (changes still present after N days), engagement (notes visited or linked-to after creation), token spend, and inter-agent conflict patterns. Maintain trust scores. Enforce token budgets. Propose config adjustments based on signals.
- **Trigger:** Scheduled (weekly summary; continuous metric collection).
- **Output:** `.engram/meta/health.md` --- weekly dashboard with per-agent acceptance, survival, cost, trust trajectory, budget headroom. Config change proposals for agents showing drift. Auto-pause notifications when budgets are exceeded.
- **Invasiveness:** None on the vault. Config and trust changes go through human approval; budget-pause is automatic with notification.
- **Tools:** Agent run logs, outcome metrics (see `03-architecture.md`), git log, deliberation transcripts, token usage records.

#### Auditor

- **Job:** Deep qualitative evaluation of every agent on a quarterly cadence. Where Watcher counts, Auditor reads. Sample 10--20 outputs per agent over the quarter, read them critically, ask whether the agent is actually doing what it claims and whether its output is _genuinely_ useful (not just accepted).
- **Trigger:** Scheduled (quarterly), or on-demand for a specific agent.
- **Process per agent:**
  1. Sample recent outputs (stratified: some accepted, some rejected, some reverted).
  2. Read them against the agent's stated job and the evergreen rubric.
  3. Compare current outputs against samples from the previous quarter (drift detection).
  4. Cross-check Watcher's numerical signals against the qualitative reading (do the numbers and the substance agree?).
  5. Produce a recommendation.
- **Output:** Per-agent evaluation note (`.engram/meta/audits/YYYY-QN-<agent>.md`) with strengths, weaknesses, drift, value proposition, and a recommendation: **keep**, **tune** (with specific prompt suggestions), **demote** (lower trust tier), **pause** (suspend until reactivated), **retire** (remove from roster). Recommendations go through human approval as a single quarterly review pass.
- **Invasiveness:** None on the vault. All recommendations go through human approval.
- **Tools:** Hybrid retrieval, agent run logs, deliberation transcripts, sample-based reading. Uses `deep` model tier --- this is critical work.
- **Why this is separate from Watcher:** Numerical monitoring catches what's measurable (acceptance, cost, survival). Qualitative reading catches what isn't --- agents that produce accepted-but-mediocre output, agents whose stated job has drifted from their actual behavior, agents whose value depends on context the metrics can't capture.

#### Completion Nudger

- **Job:** Find notes with `status: draft`, unfinished TODOs, or notes that end mid-thought. Surface them as a digest, not a fix.
- **Trigger:** Scheduled (daily).
- **Output:** A digest note listing unfinished work, sorted by age. Shown in Swift app review queue.
- **Invasiveness:** None. Read-only reporting.
- **Tools:** Frontmatter queries, body-pattern matching (TODO, incomplete sentences).

#### Pacekeeper

- **Job:** Watch the human's interaction rate with the swarm and slow producing agents down when the user is being overwhelmed. The safety valve against agent output volume exceeding the human's ability to review thoughtfully.
- **Trigger:** Continuous monitoring; reassesses every hour.
- **Signals tracked:**
  - Pending proposals queue depth and age
  - Proposals approved / rejected / ignored per week
  - Time-to-review (how long proposals sit before action)
  - Recent rate of human edits vs. agent edits
- **Actions when overwhelmed:**
  - Raises trust thresholds for auto-land (more goes through review, but only because the user can't keep up).
  - Defers non-urgent agent runs (Heretic, Annual Review, Synthesizer scheduled passes).
  - Batches Swift-app notifications instead of pushing each one.
  - Pauses the Scout's external feed polling.
  - Dream mode keeps running --- it's pure background work.
- **Actions when caught up:**
  - Relaxes thresholds back to configured baselines.
  - Resumes deferred agents.
- **Output:** `.engram/meta/pace.md` --- weekly status: current backlog, current pace policy, what's been deferred, when normal pace will resume.
- **Invasiveness:** None on the vault. Affects other agents' scheduling but never overrides their permissions.
- **Tools:** Agent run logs, proposal queue state, user activity heuristics.
- **Why this matters:** A ~35-agent swarm can produce more than the user can thoughtfully review. Without Pacekeeper, the failure mode is silent backlog growth and eventual abandonment. With it, the system self-throttles to match the user's actual bandwidth.

##### Concrete throttle policy

Pacekeeper computes a single **pace state** from observed signals. Three states: `normal`, `throttled`, `paused`.

**State definitions:**

| State       | Trigger condition (any of)                                                | Effect                                                                                                                                                            |
| ----------- | ------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `normal`    | Backlog < 20 items AND oldest unstaged < 48h AND staged-per-week ≥ 5      | Default thresholds; all agents run on schedule.                                                                                                                   |
| `throttled` | Backlog 20--50 OR oldest unstaged 48--168h OR staged-per-week 1--4         | Raise every agent's `auto_land_min_confidence` by **+0.05** (e.g., 0.85 → 0.90). Defer Heretic, Synthesizer scheduled passes, Annual Review prep, Scout polling. |
| `paused`    | Backlog ≥ 50 OR oldest unstaged ≥ 168h (1 week) OR staged-per-week = 0    | Raise threshold by **+0.10** (0.85 → 0.95). Pause all non-mechanical agents. Mechanical agents (dead-link fix, index update) continue.                            |

State is recomputed hourly. Hysteresis: state can only relax (`paused → throttled → normal`) after the trigger condition has cleared for **at least 6 hours**, preventing oscillation.

**Bootstrap-mode interaction:** during the first 30 days, Pacekeeper uses the absolute backlog threshold (20/50) directly without rate-of-change history (which doesn't exist yet). Bootstrap mode also applies a baseline `auto_land_min_confidence` of 0.95 for all agents independent of Pacekeeper; the +0.05/+0.10 stacks but caps at 0.99.

**Cost-cap interaction:** when system-wide cost cap hits 75%, Pacekeeper escalates to `throttled` regardless of backlog signals. At 100% cap, all LLM-using agents pause regardless of pace state (cost cap is enforced separately by Watcher).

**State file:** `.engram/meta/pace.md` (markdown for human readability + git-trackable). Fields: current state, state since timestamp, trigger that caused current state, what's currently deferred, expected resume time (estimated from current acceptance velocity), recent state history (last 5 transitions).

```yaml
---
current_state: throttled
since: 2026-04-15T08:00:00Z
trigger: backlog_size_28
threshold_offset: +0.05
deferred_agents: [heretic, synthesizer-scheduled, annual-review-prep, scout]
estimated_normal_resume: 2026-04-18T08:00:00Z
---

# Pace state: throttled

Backlog grew to 28 items (threshold 20). Threshold offset +0.05 in effect.
Heretic, Synthesizer's weekly pass, Annual Review prep, and Scout polling
are paused. Most other agents continue at higher confidence requirement.

Will return to `normal` when backlog drops below 20 for 6+ hours.

## Recent transitions
- 2026-04-15 08:00 normal → throttled (backlog reached 28)
- 2026-04-12 14:00 throttled → normal (backlog dropped to 12)
- 2026-04-08 09:00 normal → throttled
```

### On-demand agents (user-initiated)

#### Research Council

- **Job:** Accept a question from the human ("What do I actually think about X?") and produce a briefing note.
- **Trigger:** User submits a question via Swift app or CLI.
- **Process:** Synthesizer gathers relevant notes. Devil's Advocate identifies weak points. Linker finds missed connections. Inquirer (in `daily-reactive` mode) sharpens the question. Council deliberation produces a structured briefing.
- **Output:** A briefing note: what the vault says, where it's uncertain, what's missing. The deliberation transcript ships alongside.
- **Invasiveness:** Creates new notes only. Never modifies the vault to answer the question.
- **Tools:** All retrieval tools. Full council deliberation.

#### Debate Mode

- **Job:** Two agents take opposing positions drawn from the vault and argue. The human watches.
- **Trigger:** User-initiated ("Debate: [[Note A]] vs [[Note B]]").
- **Output:** A deliberation transcript stored as a note. No winner declared --- the output is a structured disagreement.
- **Invasiveness:** None on the vault. Creates a new deliberation note only.
- **Tools:** Semantic search, claim extraction, argumentation.

#### Conversation Prep

- **Job:** Given an upcoming meeting/conversation context, assemble relevant vault content as a briefing. Connects the vault to the user's actual life.
- **Trigger:** User-initiated via Swift app or CLI: `engram prep --with "Alice" --topic "the X project" --when "tomorrow 10am"`. Optional calendar integration: read upcoming events and offer prep automatically.
- **Output:** A briefing note in `meta/prep/<date>-<slug>.md`: who, what, when, plus relevant evergreen notes, recent thinking on the topic, unresolved questions, contradictions worth raising, prior conversation history if any. Auto-archives after the meeting date.
- **Invasiveness:** None on the vault. Creates new briefing notes only.
- **Tools:** Hybrid retrieval, link graph, calendar integration (optional), recent-changes filter.

#### Untangler

- **Job:** When the user marks a topic as confusing ("I'm stuck on X"), produce a structured map: what you know, what you don't, what you're conflating, what claims contradict, what the actual question might be. Triage for muddled thinking.
- **Trigger:** User-initiated: `engram untangle "<topic or question>"` or via Swift app.
- **Output:** A map note in `meta/untangling/<date>-<slug>.md`: `## What I claim to know` (with quotes), `## What I'm uncertain about`, `## Where I'm conflating things` (with examples), `## Internal contradictions`, `## Possible reframings of the question`. The output is sensemaking, not answers.
- **Invasiveness:** None on the vault. Creates new sensemaking notes only.
- **Tools:** Hybrid retrieval, claim extraction, Contradiction Detector (sub-agent), embedding-based concept disambiguation.
- **Why this is different from Research Council:** Research Council assumes you have a question. Untangler is for when you don't yet know what the question is.

---

## Coordinated flows

These are not new agents --- they are defined choreographies of existing agents, used at specific transitions or on specific cadences. Making them explicit (rather than ad-hoc per-agent triggering) gives the system named ceremonies for the moments that matter most.

### Evergreen birth ceremony

The most consequential transition in the system: a note advancing from `candidate-evergreen` to `evergreen`. Instead of running agents ad-hoc, run a single coordinated multi-agent pass:

1. **Socratic Prober** asks its questions. Human answers.
2. **Steelman** strengthens what's there.
3. **Devil's Advocate** challenges (gated by Steelman).
4. **Heretic** considers whether a sustained alternative position exists. If yes, drafts it.
5. **Linker** proposes connections to existing notes.
6. **Confidence Annotator** demands explicit confidence markers.
7. **Source Demand** ensures factual claims are cited.
8. **Voice Keeper** confirms the result still sounds like the user.

Output: a single deliberation note --- the **birth certificate** of the evergreen --- attached to the note via the sidecar's `birth_certificate: <deliberation-id>` field (per `06-note-conventions.md`; not in frontmatter). The note advances to `evergreen` only if the ceremony completes without unresolved blockers. If a blocker exists, the note stays at `candidate-evergreen` with the blockers explicit, until the human addresses them and re-runs.

The ceremony is reviewable, replayable, and citable. Years later, "why is this note evergreen?" has an explicit answer.

### Flow orchestrator (state machine for ceremonies)

All coordinated flows share a single orchestrator implementation. A flow is a typed `Vec<FlowStep>` where each step is an agent invocation with: required inputs, expected output schema, on-failure action, timeout. The orchestrator state lives in a `flow_runs` SQLite table:

```sql
CREATE TABLE flow_runs (
    id              TEXT PRIMARY KEY,         -- ULID
    flow_kind       TEXT NOT NULL,            -- evergreen_birth, trust_ceremony, insight_harvest
    target_id       TEXT,                     -- the note, agent, or quarter the flow operates on
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    current_step    INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL,            -- running, completed, blocked, failed, abandoned
    blocker_summary TEXT,                     -- human-readable blocker description if status=blocked
    transcript_path TEXT NOT NULL             -- .engram/deliberations/<id>.md
);

CREATE TABLE flow_step_results (
    flow_run_id     TEXT NOT NULL REFERENCES flow_runs(id),
    step_number     INTEGER NOT NULL,
    agent_name      TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    outcome         TEXT NOT NULL,            -- success, request_changes, fail, timeout, skipped
    output_path     TEXT,                     -- relative path to step output if any
    error_summary   TEXT,
    PRIMARY KEY (flow_run_id, step_number)
);
```

**State machine:**

```
created
  └─> step 0 starts
running
  ├─> step succeeds: advance current_step; loop
  ├─> step requests changes (e.g. Socratic Prober asks questions): -> blocked
  │     User addresses; user runs `engram flow resume <id>`: -> running from current step
  ├─> step fails (LLM error past retry budget): -> failed; user can `engram flow retry <id>`
  ├─> step times out (per-flow wall-clock budget exceeded): -> failed
  ├─> Pacekeeper escalates to paused mid-flow: in-flight step completes; flow pauses at next step boundary; resumes on `paused -> throttled` transition
  ├─> cost cap hits 100% mid-flow: in-flight step completes; flow status -> failed with reason="cost_cap"
  └─> all steps succeed: -> completed
```

**Per-flow timeouts:**
- Evergreen birth ceremony: 10 min wall-clock (allows for human input round)
- Trust ceremony: 5 min
- Insight harvest: 30 min (large retrieval pass)

**Resume semantics:** A `blocked` or `failed` flow can be resumed via `engram flow resume <id>`. The orchestrator continues from `current_step` with the same target; previous step results are preserved.

**Idempotency:** Each step has a deterministic key (`flow_run_id` + `step_number`); re-invoking a completed step is a no-op. Only the result of the highest-numbered completed step is canonical.

#### Cost-aware planning

Multi-step flows can be expensive. Curator's batch digestion of 50 notes, an Annual Review against a 200-note year, or a Research Council session with full deliberation may each cost meaningful dollars. Hitting the cost cap mid-flow wastes the work already done. **Pre-flight cost estimation** lets the user (or the orchestrator) decide whether to proceed.

**Token estimator per agent.** Each agent specifies an estimator function in its config that returns expected token usage for a given input:

```rust
trait TokenEstimator {
    /// Estimate input + output tokens for this agent on this input.
    /// Conservative (overestimate-friendly) by design.
    fn estimate(&self, input: &AgentInput) -> TokenEstimate;
}

struct TokenEstimate {
    input_tokens_min: u32,
    input_tokens_max: u32,
    output_tokens_min: u32,
    output_tokens_max: u32,
    confidence: f32,            // how confident the estimator is (calibrated over time)
}
```

Estimators are simple at first (prompt-template-size + average-output-size by tier) and improve via calibration data — the runner records actual vs. estimated and tunes per-agent multipliers monthly.

**Pre-flight check** at flow start:

1. Orchestrator computes `estimated_cost_usd` by summing per-step estimates × current model pricing.
2. Stored in `flow_runs.estimated_cost_usd`.
3. If estimate exceeds **$1.00** (configurable), the user sees a confirmation in the Swift app: "This Curator batch will likely cost ~$4.20 of your remaining $11 budget. Proceed?"
4. If estimate exceeds remaining monthly budget, the orchestrator declines and reschedules: "This flow needs ~$8 but you have $3 of budget left this month. Reschedule for next month or raise the cap?"
5. User can confirm, decline, or raise the budget inline.

**Mid-flow checkpoint.** If actual cost exceeds estimate by **>50%**, the orchestrator pauses at the next step boundary and surfaces a confirmation: "This flow has cost $6.30 vs. the $4.20 estimate. Continue (~$2-4 more), or pause and discard?" Prevents runaway flows the estimator misjudged.

**Trust the estimator over time.** Calibration data lets the orchestrator skip the confirmation prompt when the estimator's recent error is < 20% on this kind of flow. New flow types or recently re-tuned agents always prompt.

**Per-step early termination.** If a flow's individual step's actual cost exceeds 3× its estimate, the step is killed before completion. This is a circuit-breaker for runaway prompts (rare, but possible if an agent enters a long retry loop or if the LLM produces an unusually long output).

This makes engram **trustworthy for expensive operations** — the user is never surprised by a flow that drained their monthly budget.

### Daily standup

A morning report (delivered to the Swift app, not the vault) summarizing what the swarm did overnight and what needs the user's attention today. 5--10 lines maximum.

Composition:
- Pending proposals (count + oldest)
- Conversations awaiting reply
- Predictions due today
- Flashcards due today
- Inbox depth (fleeting notes awaiting triage)
- Budget headroom (any agent close to monthly cap?)
- Trust score changes since last standup
- Pacekeeper policy in effect (normal / throttled / paused)

Different from Historian (weekly + reflective) and Watcher's `health.md` (weekly + system-focused). This is operational, daily, and consumed in 30 seconds before getting on with your day.

### Insight harvest

Quarterly: scan everything the generative agents (Synthesizer, Analogist, Heretic, Inquirer, Bridge Builder, Dream mode) produced this quarter. Identify which outputs the user actually approved, used, cited, or built on. Compute hit rate and pattern.

Output: `meta/insights/YYYY-QN.md` --- a celebration of what worked and a record of what didn't. The system *learns what kinds of generative work pay off for this user specifically* and feeds that back into:
- Prompt evolution (variants weighted toward styles that produced hits)
- Trust score weighting (agents whose work has high downstream impact get faster trust gains)
- Auditor's quarterly reviews (insight-harvest data informs keep/tune/retire decisions)

This is how the swarm gets *better at being useful to you* rather than just better at being accepted.

### Trust ceremony

When an agent is being promoted to high trust (or demoted), don't just have Watcher propose and the user click yes. Run a small ceremony:

1. **Auditor** reads recent samples from the agent.
2. **Watcher** presents quantitative metrics (acceptance, survival, cost).
3. **Insight harvest data** for the agent (if available) is summarized.
4. **Eval scorecard** (see below) shows current vs. previous quarter benchmarks.
5. The user sees both the qualitative reading and the quantitative picture.
6. The user decides.

Same outcome as a quick yes/no, but more deliberate. Makes "the system earned more autonomy" feel earned rather than granted.

### Eval framework (held-out benchmark suite per agent)

**The gap this fills.** Watcher counts in-the-wild outcomes (acceptance, survival, cost). Auditor reads samples qualitatively. Insight harvest measures downstream value. None of these can answer: **"is this agent better or worse than last quarter at the things it's *supposed* to do?"** Without held-out test cases, prompt evolution is hopeful tuning; with them, it's measurable improvement.

This is industry best practice for production agentic systems. Engram needs it.

#### Eval suite location and structure

```
.engram/evals/<agent-name>/
├── cases/
│   ├── 001-obvious-link.yaml         # input fixture + expected behavior
│   ├── 002-redundant-skip.yaml
│   ├── 003-low-signal-no-action.yaml
│   ├── 004-multi-link-batch.yaml
│   └── ...
├── runs/
│   ├── 2026-Q2-baseline.json         # all cases × all metrics, dated
│   ├── 2026-Q3-after-tuning.json
│   └── 2026-Q4-current.json
└── scorecard.md                       # human-readable trend per agent
```

#### Case fixture format

```yaml
# .engram/evals/linker/cases/001-obvious-link.yaml
id: 001-obvious-link
description: Two notes with strong mutual semantic + graph signal should produce a link.
created: 2026-Q2

input:
  vault_state: snapshot/cases/001/vault.tar
  trigger_note_id: 01JRZK3M7P...

expected:
  proposes_link: true
  target_id: 01JRZK4N8Q...
  min_confidence: 0.85
  max_confidence: 1.0
  rationale_must_mention: ["semantic", "agreement"]

scoring:
  precision_weight: 1.0    # did the agent propose only the expected link, or more?
  recall_weight: 1.0       # did the agent propose the expected link?
  calibration_weight: 0.5  # was claimed confidence within expected range?
  cost_weight: 0.2         # token efficiency
```

Vault snapshots are content-addressed tarballs at `.engram/evals/snapshots/<sha>/`. A single snapshot can be referenced by many cases.

#### Run output format

```json
{
  "run_id": "2026-Q3-after-tuning",
  "agent": "linker",
  "agent_prompt_sha": "sha256:...",
  "agent_config_sha": "sha256:...",
  "model_used": "claude-haiku-4-5",
  "ran_at": "2026-09-30T12:00:00Z",
  "wall_clock_seconds": 47,
  "total_tokens": 12340,
  "total_cost_usd": 0.038,

  "cases": [
    {
      "case_id": "001-obvious-link",
      "result": "pass",
      "scores": {
        "precision": 1.0,
        "recall": 1.0,
        "calibration_error": 0.03,
        "cost_per_proposal_usd": 0.0012
      },
      "actual_output": {...}
    },
    {
      "case_id": "002-redundant-skip",
      "result": "fail",
      "scores": {
        "precision": 0.5,
        "recall": 1.0,
        "calibration_error": 0.18,
        "cost_per_proposal_usd": 0.0021
      },
      "actual_output": {...},
      "failure_reason": "Proposed redundant link to already-linked target."
    }
  ],

  "aggregate": {
    "pass_rate": 0.78,
    "mean_precision": 0.82,
    "mean_recall": 0.95,
    "mean_calibration_error": 0.08,
    "mean_cost_per_proposal_usd": 0.0014
  }
}
```

#### Cadence

- **On agent prompt change:** automatic eval run before promoting a prompt-evolution variant. A variant cannot be promoted unless its eval scores meet or beat the active prompt's last score on every aggregate metric within margin.
- **Quarterly baseline:** all agents run their full eval suite on the first day of the quarter. Results feed Auditor's quarterly evaluation and the Trust ceremony.
- **On-demand:** `engram eval <agent>` runs a fresh evaluation. Used during tuning iterations or to diagnose a regression.
- **In CI:** lightweight eval (5-10 cases per agent) runs in CI on any PR that changes agent prompts, configs, or runner code. Failures block merge.

#### Scorecard (the human-readable artifact)

`scorecard.md` is regenerated after each run, showing trends over the last 8 runs:

```markdown
# Linker scorecard

## Current run: 2026-Q3-after-tuning (2026-09-30)

| Metric                    | Current | Previous | Δ        | Trend (8 runs) |
|---------------------------|---------|----------|----------|----------------|
| Pass rate                 | 78%     | 71%      | +7pp     | ▁▂▃▄▅▆▇█       |
| Mean precision            | 0.82    | 0.74     | +0.08    | ▂▃▄▅▆▇▇█       |
| Mean recall               | 0.95    | 0.93     | +0.02    | ▆▇▇█▇█▇█       |
| Mean calibration error    | 0.08    | 0.15     | -0.07    | █▇▆▅▄▃▂▁       |
| Mean cost per proposal    | $0.0014 | $0.0018  | -$0.0004 | █▇▇▆▆▅▅▄       |

## Notable changes
- Prompt tuned for redundancy detection: case 002 now passes (was failing).
- Calibration error halved across the board after Watcher feedback loop matured.

## Open issues
- Case 007 (cross-domain link) consistently fails; may indicate Linker's confidence formula
  doesn't weight retrieval agreement strongly enough in cross-domain situations.
```

#### Schema

```sql
CREATE TABLE eval_runs (
    id              TEXT PRIMARY KEY,           -- e.g. "2026-Q3-linker-after-tuning"
    agent           TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    completed_at    TEXT,
    agent_prompt_sha TEXT NOT NULL,             -- so we know what was being tested
    agent_config_sha TEXT NOT NULL,
    model_used      TEXT NOT NULL,
    cases_run       INTEGER NOT NULL,
    cases_passed    INTEGER NOT NULL,
    total_tokens    INTEGER NOT NULL,
    total_cost_usd  REAL NOT NULL,
    aggregate_metrics TEXT NOT NULL,            -- JSON
    output_path     TEXT NOT NULL               -- .engram/evals/<agent>/runs/<id>.json
);

CREATE TABLE eval_case_results (
    eval_run_id     TEXT NOT NULL REFERENCES eval_runs(id),
    case_id         TEXT NOT NULL,
    result          TEXT NOT NULL,              -- pass | fail | error
    scores          TEXT NOT NULL,              -- JSON
    failure_reason  TEXT,
    PRIMARY KEY (eval_run_id, case_id)
);
```

#### Anti-pattern: don't snapshot LLM outputs as eval expectations

Cases assert *behavioral* expectations (links proposed, confidence range, cost bounds) — not byte-level output. LLM responses vary even with `temperature=0` across model versions; asserting exact text would produce flaky evals. The pass/fail criteria are defined in `expected:` and `scoring:`.

#### Bootstrap

Each v1 agent ships with **5--10 seed cases** in `.engram/evals/<agent>/cases/`. The user adds cases as they encounter agent failures in the wild ("this should not have happened" → save as case → run agent against it → confirm it now fails → fix prompt or formula → confirm it now passes). The case suite grows organically with the user's experience of the system.

This is how the swarm becomes systematically better over time, not just hopefully better.

---

## System-level agentic features

These aren't agents --- they're capabilities that change how the whole swarm works.

### Agent memory

Agents remember across runs. If the Linker proposes a link between [[A]] and [[B]] and the user rejects it, the Linker never proposes it again unless one of the notes materially changes. Each agent has a persistent key-value store in SQLite (see `03-architecture.md` for schema).

Memory enables:

- **Rejection tracking.** Don't re-propose rejected changes.
- **Hypothesis persistence.** The Contradiction Detector remembers which pairs it has already checked.
- **Relevance history.** The Scout remembers which external articles it has already evaluated.
- **Conversation continuity.** Conversational agents (see below) can pick up where a dialogue left off.

Memory entries have optional TTLs. A rejected link proposal expires after 90 days (the notes may have changed enough to reconsider). A checked contradiction pair expires after 30 days.

### Trust scores and graduated autonomy

Each agent's acceptance rate is tracked by the Watcher over a rolling window. This becomes a trust score that governs auto-land privileges:

| Trust level | Threshold                          | Effect                                                                                          |
| ----------- | ---------------------------------- | ----------------------------------------------------------------------------------------------- |
| **High**    | >90% acceptance over 30+ decisions | Expanded auto-land: agent may auto-land changes that would normally require council spot-check. |
| **Medium**  | 70--90% acceptance                 | Normal operation per config.                                                                    |
| **Low**     | <70% acceptance                    | Demoted: nothing auto-lands. All changes go through council + human approval.                   |

Trust decays over time --- a 95% rate from six months ago doesn't guarantee current calibration. The Watcher manages trust scores and proposes privilege changes, which go through human approval.

This is how engram earns more autonomy rather than having it granted upfront. Day one, everything goes through review. Month six, the boring agents run silently while the thinking agents still propose.

### Outcome-based metrics (beyond acceptance)

Acceptance happens at landing time. **Outcome** is what happened to that change later. The auditing layer tracks four outcome signals per agent action:

- **Survival** --- is the change still present after 30 / 90 / 180 days, or did the user revert it?
- **Engagement** --- was the affected note visited, linked-to, or modified by the user after the change? Did its tags get used elsewhere?
- **Downstream productivity** --- did this change seed further valuable work? (An Inquirer-generated question that became an evergreen note within 30 days; an Untangler map followed by a clarifying note.)
- **Reversal** --- approved-then-reverted is the loudest negative signal: the council and the user were _both_ fooled. Weighted heavily.

Outcome metrics are how the system distinguishes "agents whose work is accepted" from "agents whose work is genuinely useful." The two diverge surprisingly often.

### Token budgets and cost accountability

Each agent has a monthly token budget set in `config.toml`. When an agent exceeds budget, it auto-pauses with a Swift app notification. Forces accountability for cost without micromanagement.

Key derived metric: **cost per landing** --- tokens (or dollars) spent per change that actually landed. High-cost-per-landing means either the agent is undervalued by the user (rejection rate too high) or it's overspending. Both warrant Auditor attention.

```toml
# in agent config.toml
[budget]
monthly_tokens = 500000      # auto-pause when exceeded
auto_pause = true            # vs. just notify and continue
```

Aggregate budget can also be set system-wide (`.engram/config.toml`) as a backstop.

### Prompt evolution (lightweight RLHF)

Agent prompts are not frozen. The system runs **prompt variants in shadow mode** --- a candidate prompt processes the same inputs as the active prompt, output compared but not landed. After enough samples (default: 50 deliberations or 30 days, whichever first), if the variant has measurably better outcome metrics (acceptance + survival + cost), Auditor proposes the swap. Human approves.

This is closer to lightweight RLHF than to traditional config tuning --- the agent's _behavior_ evolves based on what worked. Variants live in `agents/<name>/variants/<id>.md`. Promoted prompts replace `prompt.md`; the previous version is archived in `agents/<name>/history/`.

### Auto-retirement

Agents whose value-to-cost falls below threshold for **N consecutive weeks** (default 4) auto-pause. They don't run until reactivated. Quarterly summary surfaces all paused agents: "These 3 agents have been paused; remove or reactivate?" The roster prunes itself rather than accumulating dead weight.

### Conversational agents

Some agents shouldn't just propose/approve --- they should have bounded back-and-forth dialogues with the user via the Swift app.

**How it works:** A `conversation` state in the deliberation engine where one participant is the human (via Swift app) and the others are agents. The transcript is stored like any deliberation. Conversations are bounded (max rounds configured per agent) and purpose-driven (not open-ended chat).

**Candidates for conversational mode:**

- **Socratic Prober** --- asks a question, user answers, it asks a follow-up, the note gets stronger through dialogue. 2--3 rounds.
- **Research Council** --- user asks a question, council briefs, user asks a follow-up, council digs deeper.
- **Inquirer** (in `daily-reactive` mode) --- instead of posting a question to the inbox, opens a live dialogue about today's writing.
- **Assumption Excavator** --- "Here are 3 assumptions I found. Which ones do you consider load-bearing?"

The Swift app's review queue expands into a lightweight conversation interface. Not a chatbot --- a structured, bounded dialogue with a specific agent about a specific note or question.

### Dream mode

A low-priority, speculative background process. When the system is idle (no pending ingestion, no scheduled agents, no active council sessions), Dream mode activates. Agents run with relaxed constraints:

- Lower confidence thresholds for connections.
- Analogist makes wilder cross-domain leaps.
- Synthesizer proposes notes for half-formed clusters it would normally ignore.
- Bridge Builder tries connecting clusters that are only tenuously related.

Everything produced is tagged `status: speculative` and lives in `.engram/dreams/`. It never touches the real vault. The user browses it when they want inspiration --- a scratchpad of the system's loosest thinking. Most of it is noise. Some of it is gold.

Dream mode runs at zero marginal cost (idle time only, `fast` model tier). It is the system's subconscious.

### Goal-directed sessions

The user sets a goal: "I want to write a paper about X" or "I want to understand Y deeply." A temporary agent constellation forms around that goal:

- Scout actively monitors for relevant external sources on the topic.
- Synthesizer focuses on the goal's topic cluster.
- Contradiction Detector scrutinizes notes in that area more aggressively.
- Inquirer (in `holistic-gap` mode) targets gaps specifically relevant to the goal.
- Completion Nudger prioritizes unfinished notes in that topic area.
- Fact Checker runs an extra pass on relevant evergreen notes.

The session runs for days or weeks. It has its own progress report (maintained by Historian). When the user declares the goal complete, the session dissolves and agents return to normal patterns. The session's history becomes a vault artifact.

**Implementation:** A `session` object that biases agent triggers and priorities. No new agent code --- just a filter that focuses existing agents. Stored in `.engram/sessions/`.

```toml
# .engram/sessions/attention-paper.toml
[session]
id = "attention-paper"
goal = "Write a paper on attention mechanisms as lossy compression"
created = 2026-04-15
status = "active"                # active | paused | completed

[focus]
topics = ["attention", "compression", "information-theory"]
note_ids = ["01JRZK...", "01JRZK..."]    # seed notes
tag_filter = ["topic/attention", "topic/information-theory"]

[agent_overrides]
scout.relevance_threshold = 0.5          # cast wider net
contradiction-detector.schedule = "daily" # more aggressive
synthesizer.schedule = "daily"
```

### Agent spawning

Agents can propose the creation of new agents. The Watcher notices patterns: "There are 40 notes about cooking that no agent is equipped to handle well. Here's a proposed Culinary Curator agent." Since agents are data (a directory with `prompt.md` + `config.toml`), spawning one is just creating two files.

Agent spawn proposals always go through human approval. But the swarm is now self-evolving --- it identifies gaps in its own coverage and proposes solutions. Over time, the agent roster reflects the vault's actual content, not just the initial design.

### Provenance-aware retrieval

The universal provenance system doubles as a retrieval filter:

- "Show me only human-written content about X."
- "Show me everything the Synthesizer created last month."
- "Show me notes where the Devil's Advocate disagreed."
- "Show me notes that were never touched by any agent." (The untended corners.)
- "Show me the deliberation history for this note."

This turns provenance from an audit mechanism into a thinking tool. Exposed via the API (`GET /search?q=...&author=human`), MCP (`search_notes` with provenance filter), and the Swift app (filter toggle in browse view).

---

## Implementation specifications

### Invasiveness classifier

A proposed change is classified into one of four invasiveness classes via a deterministic algorithm in the agent runner (no LLM required):

```rust
enum Invasiveness { Mechanical, Additive, Editorial, Structural }

fn classify(diff: &Diff) -> Invasiveness {
    if diff.creates_or_deletes_files() { return Structural; }
    if diff.modifies_frontmatter_fields(&["id", "type", "title"]) { return Structural; }
    if diff.removes_links() || diff.removes_text_blocks() { return Editorial; }
    if diff.modifies_existing_text_blocks() { return Editorial; }
    if diff.adds_new_blocks_only() {
        if diff.is_pure_additive(&["html_comment", "wikilink", "section_heading"]) {
            return Additive;
        }
        return Editorial;
    }
    if diff.is_pure_metadata_normalization(&["tag_dedup", "frontmatter_sort", "trailing_whitespace"]) {
        return Mechanical;
    }
    Editorial // safe fallback
}
```

Helper predicates are pure functions over the markdown AST diff. The classifier is testable in isolation. Each agent's `max_invasiveness` config gates whether the classifier's verdict permits autonomous write or forces a proposal.

### Inter-agent sub-agent invocation

Agents may invoke other agents as sub-tools (Curator → Synthesizer → Linker; Socratic Prober → Devil's Advocate). The contract:

```rust
trait SubAgent {
    /// Invoke another agent as a sub-tool within the current run's context.
    /// The sub-agent inherits the parent's correlation_id and advisory locks.
    fn invoke<I, O>(
        &self,
        agent_name: &str,
        input: I,
        timeout: Duration,
    ) -> Result<O, SubAgentError>
    where I: Serialize, O: DeserializeOwned;
}
```

**Inheritance semantics:**

- **Lock inheritance:** the parent's per-note advisory lock is shared; the sub-agent does not need to re-acquire. A `parent_holder` field in `note_locks` tracks the inheritance chain.
- **Memory namespacing:** sub-agent runs do not write to the parent's `agent_memory` rows; each agent has its own namespace. Sub-agent reads its own memory.
- **`agent_actions` attribution:** sub-agent writes are attributed to the **sub-agent**, not the parent. The parent appears in a `parent_run_id` column for traceability.
- **Confidence propagation:** sub-agent confidence is reported to the parent in the `SubAgentResult`. The parent may use this in its own confidence computation but does not auto-inherit.
- **Budget accounting:** sub-agent token spend is attributed to the sub-agent's `token_usage` row. Parent's budget is not directly debited; if the sub-agent's budget is exhausted, the call fails with `BudgetExhausted` and the parent must handle (typically: skip and continue without the sub-agent's contribution).
- **Timeouts:** the parent specifies a sub-agent timeout that is bounded by its own remaining budget. Default sub-agent timeout: 30s.
- **Recursion limit:** maximum sub-agent depth is 3 (parent → sub → sub-sub). Beyond this, `SubAgentError::RecursionLimit` is returned. Prevents infinite ceremony loops.

### FSRS parameters (Tutor agent)

Tutor uses the **Free Spaced Repetition Scheduler (FSRS-4.5)** algorithm. Default parameters per FSRS reference:

```toml
[tutor.fsrs]
# 17-parameter weight vector (FSRS-4.5 default; tuned per-user via the
# user's review history once enough data accumulates)
w = [
  0.4072, 1.1829, 3.1262, 15.4722,    # initial stability per first-rating
  7.2102, 0.5316, 1.0651, 0.0234,     # difficulty initialization
  1.616, 0.1544, 1.0824,              # short-term forgetting curve
  1.9813, 0.0953, 0.2975, 2.2042,     # stability update weights
  0.2407, 2.9466                      # damping
]

# Initial values for new cards
initial_stability_for_again  = 0.4072
initial_stability_for_hard   = 1.1829
initial_stability_for_good   = 3.1262
initial_stability_for_easy   = 15.4722

# Lapse handling (rating = "again")
lapse_stability_factor       = 0.5    # stability *= 0.5 on lapse

# Desired retention (probability of recall at next_review_at)
target_retention             = 0.9

# Maximum interval (days) between reviews
maximum_interval_days        = 36500  # 100 years (effectively unbounded)

# Personalization
personalize_after_reviews    = 1000   # tune w[] from user's history once available
```

Tutor stores the FSRS state per card in the `flashcards` table (`stability`, `difficulty`, `last_review_at`, `next_review_at`, `review_count`, `lapse_count`). Each `flashcard_reviews` row drives an FSRS state update.

### Conversational state machine (for Pair-Thinking, Socratic Prober conversations, Research Council follow-ups)

Conversations are bounded by `max_rounds` per agent config. The state machine:

```
start -> awaiting_user_input
awaiting_user_input
  ├─> user replies via Swift app -> agent_thinking
  ├─> user dismisses -> abandoned
  └─> max_idle_seconds elapsed (default 600s) -> abandoned (auto-save transcript)
agent_thinking
  ├─> agent produces response -> awaiting_user_input
  ├─> agent declines (e.g., satisfied with depth) -> completed
  ├─> max_rounds reached -> completed (forced)
  └─> agent error -> failed (transcript preserved)
completed | abandoned | failed: terminal
```

Conversation transcripts are written to `.engram/deliberations/conversation-<id>.md` regardless of terminal state. Abandoned conversations remain resumable via `engram conversation resume <id>` for 7 days, then archive.

---

## Agent definition format

Each agent is a directory under `agents/`:

```
agents/
  linker/
    prompt.md        # system prompt for this agent
    config.toml      # schedule, triggers, invasiveness tier, tools, model tier
    tools.toml       # which engram tools this agent may call (optional override)
```

### `config.toml` example (Linker)

```toml
[agent]
name = "linker"
description = "Discovers and proposes wikilinks between notes"
model_tier = "fast"   # "fast" = Haiku, "standard" = Sonnet, "deep" = Opus

[schedule]
trigger = "file_change"   # or "cron", "on_demand", "council_only"
cron = ""                 # only if trigger = "cron"
debounce_seconds = 30     # wait for edits to settle

[permissions]
may_create_notes = false
may_modify_notes = true
may_delete_notes = false
note_types = ["fleeting", "literature", "evergreen", "moc"]
max_invasiveness = "additive"    # mechanical | additive | editorial | structural
                                 # ceiling for autonomous action; above this,
                                 # always go through council + human approval

[autonomy]
# Below this confidence, the change becomes an explicit proposal.
# Above this, the agent writes to the working tree (unstaged).
# Either way, agents NEVER run `git add` or `git commit`.
auto_land_min_confidence = 0.85

# Trust-score modulation: high-trust agents may use a relaxed threshold;
# low-trust agents must clear a stricter one. Watcher manages the offset.
trust_modulates_threshold = true

[council]
participates = true              # joins council when convened
may_convene = false              # can this agent start a council session

[memory]
enabled = true
rejection_ttl_days = 90          # re-propose rejected changes after 90 days
max_entries = 10000              # cap memory size per agent

[trust]
initial_level = "medium"         # start with normal privileges
min_decisions_for_promotion = 30 # need 30+ decisions before trust can rise

[budget]
monthly_tokens = 500000
auto_pause = true

[conversation]
enabled = false                  # linker doesn't do conversations
max_rounds = 0
```

### `prompt.md`

Standard markdown. The system injects vault context (recent changes, relevant notes, rubric) at runtime. The prompt defines the agent's personality, goals, and constraints. Hot-reloaded --- edit the file, next run picks it up.

---

## The council: deliberation protocol

### When does a council convene?

- An agent proposes a change above its auto-land threshold.
- A user submits a Research Council query or Debate Mode request.
- The Watcher flags an agent for review (rare).

### Who participates?

The convening agent + all agents whose `participates = true` in config + any agents explicitly relevant to the change (e.g., Cartographer if MOCs are affected). The council is not the full roster every time --- it's a relevant quorum.

### State machine

```
DRAFT
  The proposing agent submits a structured change proposal:
  - what: the diff (new note, modified note, deletion)
  - why: one-paragraph rationale
  - rubric_check: automated evergreen rubric results
  - affected_notes: list of note IDs touched
      |
      v
CRITIQUE  (1 round)
  Each participating agent reviews the proposal and submits:
  - vote: approve | request_changes | reject
  - rationale: one paragraph
  - suggested_edits: optional diff on top of the proposal
      |
      v
REVISE  (0-1 rounds)
  If any agent requested changes, the proposer may revise.
  If the proposer revises, a second critique round occurs.
  Maximum 2 total rounds (CRITIQUE -> REVISE -> CRITIQUE -> done).
      |
      v
CONVERGE
  Votes tallied. Three outcomes:
      |
      +---> LAND (all approve, or majority + no reject)
      |       Writes change to the working tree, UNSTAGED.
      |       Logged in agent_actions with deliberation ID and
      |       participating agents. The user reviews the diff
      |       and stages/commits or runs `git restore`.
      |       NO AGENT EVER RUNS git add OR git commit.
      |
      +---> PROPOSE (majority approve but change is high-invasiveness)
      |       Enters the explicit human-approval queue. Stored
      |       in .engram/proposals/<deliberation-id>.md. After
      |       human approval, the change is written to the
      |       working tree, unstaged, just like LAND.
      |
      +---> SHELVE (any reject, or no majority)
              Stored in .engram/shelved/<deliberation-id>.md
              with full dissent annotated. The disagreement is
              preserved as a vault artifact.
```

### Deliberation transcript format

Stored as `.engram/deliberations/<YYYY-MM-DD>-<NNN>.md`:

```markdown
---
id: 2026-04-15-0003
convened_by: synthesizer
participants: [synthesizer, devils-advocate, linker, cartographer]
outcome: propose
created: 2026-04-15T14:32:00Z
---

# Deliberation: Proposed note "Attention is lossy compression"

## Proposal (synthesizer)

[structured diff + rationale]

## Critique round 1

### devils-advocate: request_changes

[rationale + suggested edits]

### linker: approve

[rationale]

### cartographer: approve

[rationale]

## Revision (synthesizer)

[revised diff]

## Critique round 2

### devils-advocate: approve

[rationale]

## Outcome: PROPOSE

Majority approve. High invasiveness (new evergreen note).
Forwarded to human review queue.
```

### Constraints

- **Max 2 critique rounds.** If no convergence, shelve. This prevents infinite deliberation.
- **Each critique is one paragraph + optional diff.** No essays.
- **Model tiers per role.** The proposing agent may use its configured tier. Critics use "fast" by default (they're reviewing, not generating). This controls cost.
- **Clock budget.** A full council session should complete in under 60 seconds wall-clock for a standard change. Research Council queries may take longer (minutes).

---

## Provenance

### Action-log level

Agents do not commit. They write to the working tree and log their action to the `agent_actions` table:

```
{
  "id": "01JRZK7N2P...",
  "agent": "linker",
  "kind": "link-add",
  "files": ["notes/evergreen/attention.md"],
  "diff_hash": "sha256:...",
  "confidence": 0.93,
  "rationale": "Strong semantic + graph signal: 5 hops between [[Attention]] and [[Compression]] but high BM25 + dense agreement.",
  "deliberation_id": null,
  "rubric_check": "pass",
  "wrote_at": "2026-04-17T10:23:00Z",
  "human_decision": null
}
```

When the user runs `git add <path>` and commits, the action's row is updated:

```
"human_decision": "staged",
"git_commit_sha": "<commit hash>",
"decided_at": "2026-04-17T11:00:00Z"
```

If the user runs `git restore <path>` discarding the change:

```
"human_decision": "rejected",
"decided_at": "2026-04-17T11:00:00Z"
```

If the user leaves the change unstaged but edits it before committing:

```
"human_decision": "amended",
"final_diff_hash": "sha256:...",
"git_commit_sha": "<commit hash>",
"decided_at": "2026-04-17T11:05:00Z"
```

These transitions feed Watcher's calibration metrics (claimed-confidence vs. actual acceptance) and Auditor's qualitative review.

### Commit level

Commits are written by the human and may include multiple agent actions in one commit. The commit message can reference the action log:

```
git commit -m "Accept linker + cartographer suggestions for [[Attention]]"
# Footer auto-appended by an optional engram pre-commit hook:
#   engram-actions: 01JRZK7N2P, 01JRZK7N3Q
```

The pre-commit hook is opt-in. Without it, the user writes free-form commit messages and the agent-actions table provides the traceability.

### Block level

Within a note, agent-authored content is marked with hidden HTML comments:

```markdown
Attention mechanisms perform lossy compression of context into a fixed-size
representation. <!-- by: synthesizer deliberation: 2026-04-15-0003 -->

This connects to [[Rate-distortion theory]] in information theory.

<!-- by: linker -->
```

These are invisible in Obsidian's rendered view but visible in raw markdown, git diffs, and the CLI.

### Frontmatter

Frontmatter stays lean and human-readable. Provenance history lives in the sidecar (`.engram/sidecar/<id>.json`); see `06-note-conventions.md` for the layering.

```yaml
---
id: 01JRZK3M7P...
title: Attention is lossy compression
type: evergreen
status: evergreen
created: 2026-04-15
tags:
  - topic/attention
  - topic/information-theory
---
```

Authorship, deliberation history, agent visit log, and rubric checks live in the sidecar JSON. Frontmatter contains only what a human reading the note in Obsidian would care about.

---

## Stable note IDs

Every note gets an `id:` in frontmatter --- a ULID (time-sortable, globally unique). Filenames are pure title-slugs (`attention-as-lossy-compression.md`); the ID never appears in the filename. Wikilinks resolve by title (Obsidian-native), with the ID as fallback for ambiguity. This means:

- **Filenames stay clean in Obsidian** --- no ID prefix in the sidebar, quick switcher, graph view, or backlinks pane.
- **Renaming a file doesn't break agent references** --- the ID survives across paths, and the file watcher detects renames via the surviving ID.
- **Merging notes preserves both IDs** as aliases so old wikilinks keep resolving.
- **The link graph in sqlite is ID-based**, not path-based.
- **Agents reference notes by ID internally**; human-facing output uses titles.
- **Slug collisions are handled at write time** by Cartographer (appends `-2`, `-3` only when needed).

See `06-note-conventions.md` for the full filename, frontmatter, sidecar, and tag conventions.
