# User Scenarios

## Purpose

Design docs describe the system. Scenarios show what it feels like to use. Each walk-through grounds the design in real interaction. If a scenario feels awkward, something upstream is wrong.

These scenarios are not feature lists. They are narratives of a person doing a thing, with the system responding the way it should. Times, agents, and details are illustrative; the shape is what matters.

---

## Morning routine

**Setting:** Tuesday morning, mug of coffee, MacBook open. Engram has been running overnight.

The user opens the Swift app on macOS. The today widget shows:

```
3 pending diffs · 2 flashcards due · 1 prediction due
1 prep card for 10am with Alice · Pacekeeper: normal
```

She opens the diff queue. Three items:

1. `notes/evergreen/attention.md` — linker added a wikilink to `[[Compression]]` (confidence 0.94). She reads the diff: the link makes sense. Tap to stage.
2. `notes/evergreen/notes-on-rag.md` — gardener proposes removing a TODO that's been there for six months and has been resolved by a newer note. Confidence 0.91, with the new note linked as evidence. She reads the rationale (`Why?`), agrees, taps to stage.
3. `notes/literature/2026-04-15-podcast-transcript.md` — scribe rewrote a clunky transcript for readability. Confidence 0.88. She skims it, swipes one paragraph back to its original (long-press → amend), then stages.

She runs `git commit -m "morning review: link attention<->compression, prune resolved todo, polish transcript"`. Done in under a minute.

Then the prediction widget surfaces a card: "On 2025-09-12 you wrote 'transformers will hit a scaling wall by end of Q1 2026.' How did this hold up?" Three buttons: correct / incorrect / superseded. She taps incorrect, types one line of context. Calibration data accumulates without conscious effort.

She glances at the prep card for the 10am meeting — engram has assembled relevant notes about Alice and the project topic. She'll read it on her phone while walking to the meeting.

Total time so far: 3 minutes. She moves on with her day.

---

## Capture in the wild

**Setting:** A walk in the park. iPhone in pocket. iPhone has no signal.

A thought arrives. The user double-presses the Action Button — it's bound to engram's voice capture. She speaks for forty seconds:

> "The Karpathy-style index might actually compose with the rationality gate — the gate is checking critique quality, the index is the navigation surface; if I expose the gate's verdicts as inline annotations the index could surface 'notes that have survived heretical attack' as a quality signal. Worth thinking about."

The voice memo lands in the local SwiftData queue. The Watch shows a check-mark. Queue depth: 1, sync status: pending.

She continues walking. The phone has no signal. The capture sits in the queue. She thinks of two more things and captures them. Queue depth: 3.

Twenty minutes later she walks back into Wi-Fi range. The queue badge changes from "3 pending" to "2 syncing" to "all caught up." Each capture posts to the Mac with its ULID as idempotency key. The Mac transcribes them locally with `whisper.cpp`, runs them through Scribe (cleanup), and they enter the diff queue as fleeting notes.

When she sits down later, she sees three new fleeting notes ready for review. She decides to promote the first one (the index + rationality-gate idea) to `candidate-evergreen`. That triggers Inbox Triage to suggest classification, which she accepts. The note is now waiting for the evergreen birth ceremony when she's ready to develop it further.

---

## A hard problem

**Setting:** Wednesday afternoon. The user has been turning over a question for days: "Should engram's external MCP support per-app rate limiting?"

She's stuck. Not in a "I need an answer" way — in a "I don't even know what the right framing of this question is" way.

She opens the Swift app and taps **"I'm stuck on..."**. Untangler activates. She types: "External MCP rate limiting — should it exist, what shape?"

Untangler reads the vault. It assembles a sensemaking map note in `meta/untangling/2026-04-23-external-mcp-rate-limit.md`:

- **What I claim to know** (with quotes from existing notes): rate limits prevent runaway clients; OAuth-style scopes already provide one form of access control; the audit log already detects unusual patterns.
- **What I'm uncertain about**: whether the threat model needs rate limiting at all, given the consent flow; whether per-token vs. per-call vs. per-data-volume is the right axis.
- **Where I'm conflating things**: rate limits as a security measure vs. as a cost-control measure vs. as a user-experience measure for the third-party app — three different problems, possibly different answers.
- **Internal contradictions**: a note from last month argued "external apps should be unconstrained — the consent flow is the security boundary"; a recent note argued "any external surface needs defense in depth."
- **Possible reframings**: maybe the question isn't "rate limiting" but "what does it look like when an external MCP client misbehaves, and how does the user notice?"

She reads this. The reframing in the last bullet is the unlock. The right question wasn't about rate limits — it was about _visibility_ into client behavior. Rate limits are one possible answer; better audit-log surfacing is another.

She drafts a new fleeting note capturing the reframing. Tags it for next development. The Untangler map stays in `meta/untangling/` as a reference; she'll re-read it next time the question comes up.

She didn't get an answer. She got a _better question_. That's the point.

---

## Year-end ritual

**Setting:** December 31st evening. The user has been using engram for 14 months.

She opens the Swift app. A new card: "Your Annual Review for 2026 is ready." She taps it.

The Swift app renders the long-form note `reflections/annual/2026.md` in a typography-tuned full-screen reader. Pacing is gentle; sections reveal as she scrolls. The Annual Review agent has read the year:

- Themes that crystallized (with linked evergreen notes that show the arc)
- Threads that were abandoned (with one-line "wasn't ready / lost interest / wasn't useful" annotations)
- Predictions made and resolved (with calibration commentary)
- Books read and ideas absorbed (from literature notes)
- A handful of quotes from her own writing that the agent identified as "characteristically you"
- Open questions she keeps circling without resolving

The voice is hers (Voice Keeper tuned the prose). The structure is the agent's. She reads it twice, edits a few sentences that land wrong, stages, commits.

She forwards the link to no one. This isn't for anyone else. It's for the version of her who reads it again next December.

---

## First time

**Setting:** Saturday morning. New user. Just installed engram from a Homebrew tap and the Swift app from TestFlight.

She launches the Swift app on her Mac. The first-run wizard opens.

1. **Vault.** "Greenfield (new vault)" or "existing Obsidian vault" or "I want to digest an old vault." She picks greenfield. Engram creates `~/engram-vault/`, initializes git, drops a `welcome.md` with a tutorial walkthrough.
2. **Models.** Three options. She picks "hybrid: local embeddings, cloud generation." Wizard prompts for an Anthropic API key. She pastes it; it goes to Keychain.
3. **Cost cap.** Defaults to $25/month. She accepts.
4. **Backup.** Wizard offers to create a private GitHub repo and push the vault. She agrees; it pushes. Backup Watcher activates with the new remote configured.
5. **Privacy zones.** Defaults look fine; she accepts.
6. **Agents.** v1 set, all enabled. She accepts.
7. **Tutorial.** "Spend 5 minutes learning conventions?" She says yes.

The tutorial walks her through:

- The `welcome.md` MOC (showing the navigation pattern)
- A sample fleeting note (showing capture format)
- A sample literature note (showing source-orientation)
- A sample evergreen note (showing the curated form)
- A demo of dropping a PDF into the Swift app (Ingestor produces a literature note that lands in the diff queue; she stages it)
- A demo of the diff review interface (Linker proposes a wikilink between the welcome note and the new literature note; she stages it)

Total time: 12 minutes. She has a working vault, knows the conventions, has seen agents work, and is in bootstrap mode (everything for the next 30 days will surface to her review queue conservatively).

She closes the laptop and goes to make breakfast.

---

## Migration

**Setting:** Two months in. The user has been keeping a second vault `notes-2022-03/` for years — accumulated thinking from before engram. It's grown to 9,000+ notes; mostly mediocre. She wants to digest the good parts into engram and let go of the rest.

She runs `engram digest /path/to/notes-2022-03`.

Curator's survey runs for an hour. It builds a structural map: tag taxonomy, link density, modification cadence, topic clusters. It produces `~/engram-vault/.engram/digestion/notes-2022-03/plan.md` with recommendations:

- 1,053 already-tagged-evergreen notes — evaluate individually
- 2,841 literature-style — convert
- 3,012 meeting-notes / fleeting — likely discard with summary
- 487 in-progress drafts — defer
- 1,243 cluster-redundant — synthesize at cluster level
- 611 other

She reviews the plan. Adjusts: never auto-discard `tag=poetry`, treat `path:journal/` as private, raise cluster-synthesis threshold to 10 (this corpus has many small clusters she wants to keep granular).

She runs `engram digest --resume`. Curator processes 50 notes per batch. Each batch surfaces in the Swift app review queue as a single unit. She reviews in 10-minute sessions, two or three a week, while doing other things.

Six weeks later, the digestion is complete:

- 9,247 source notes → ~600 evergreen drafts (now landed) + ~1,400 literature notes + ~1,800 archive notes + ~140 merged-into-existing + ~5,300 discarded
- Compression: 78%
- Auditor's post-digestion review samples 50 discards; she reviews them; agrees with all but two ("oh, I forgot I wrote that — bring it back"); restores those two from the archive of source-paths-and-summaries

The original `notes-2022-03/` is unchanged. She decides to keep it forever as a personal archive but no longer references it; engram has the curated version.

Her engram vault is now ~2,000 active notes plus archive. Substantially leaner than the source. The Linker spends the next week proposing connections between the newly-imported content and her existing engram notes; she reviews and stages them in the morning routine.

---

## Mobile work session

**Setting:** A long flight. Mac is at home, asleep. iPhone is the only device.

The user opens the Swift app. She's in flight mode but the offline cache has the recent notes.

She wants to write. She opens a note (or rather, creates a new fleeting note via the capture button) and types for twenty minutes — thoughts about a half-formed concept she's been circling. The note lands in the local SwiftData queue with a placeholder ID; the actual ULID is assigned when it syncs.

She also wants to look up something she wrote months ago. The offline-FTS index in SwiftData lets her search; she finds the note, reads it from the cache, copies a quote into her draft.

The Swift app's queue badge shows: 1 pending capture, 0 syncs, offline.

Five hours later, the plane lands. iPhone reconnects. The capture syncs to the Mac. She walks off the plane already moving on to something else.

When she gets home and opens the Mac, the indexer has already processed the new fleeting note. It's in the diff queue with a Linker proposal pre-attached. She'll review it in tomorrow's morning routine.

---

## A travel app uses engram

**Setting:** The user built a travel-planning app. It registered with engram's external MCP server.

She opens the travel app. "Plan something for late summer." The app calls engram:

```
GET /mcp/tools/personal_context
  query: "travel preferences and recent travel thinking"
  max_tokens: 4000
```

Engram returns a structured digest: Biographer's user model excerpt (city preferences, pace, lodging style, dislikes), top relevant literature notes (recent trip notes), trajectory snapshot ("travel thinking has shifted from productivity-escape to language-immersion over the last 2 years"), preferences in the travel domain, recent thinking ("possibly Lisbon Q3" from a fleeting note last week).

The travel app's LLM, grounded in this digest, drafts an outline: "Slovenia in late August — 10 days, Ljubljana base, Lake Bled, Soča valley." It surfaces three clarifying questions back to engram via `ask_user`. Engram pings the Swift app: "Travel App asks: relaxing or adventurous?" She taps "adventurous" and dismisses.

Travel app finalizes the plan. Calls `record_session` to deposit a literature note in engram about what it researched and decided. The note enters the diff queue (proposal route — write scopes always go through review). She approves it later that day.

Now her vault has a `notes/literature/travel-slovenia-2026-q3.md` that Linker connects to her earlier travel-evergreen notes. The next time _any_ app calls `personal_context("travel")`, the picture is richer for it.

She didn't onboard the travel app. The app already knew her.

---

## A bad agent moment

**Setting:** Three months in. Heretic's quarterly run produces a counter-argument to one of her favorite evergreen notes. It survives the rationality gate (Steelman approves: argument is coherent, has real-world adherents, engages the actual claim). It lands as an unstaged proposed note in the diff queue.

She reads it. She disagrees. The counter-argument is technically rational but it misreads what she meant. She taps **Discard with reason**. A picker: hallucinated, wrong direction, redundant, out of scope, voice-mismatch. She picks "wrong direction" and adds a one-line note: "missed that the original note is about X, not Y."

`git restore` runs. The unstaged change vanishes. The discard is logged in `agent_actions` with her reason. The original evergreen note is untouched.

A month later, Auditor's quarterly run notices that Heretic's discard rate on her vault has trended slightly up, with "wrong direction" overrepresented. Auditor proposes a prompt-evolution variant: a sharper instruction in Heretic's prompt to re-read the original twice for intent before drafting an alternative. The variant runs in shadow mode for 30 days, demonstrates measurably lower discard rate, and Auditor proposes the swap. She approves; the prompt updates.

The swarm got better at being useful to her, specifically.

---

## The "stuck for a week" pattern

**Setting:** Travel week, sick week, busy week — whatever. The user hasn't reviewed her diff queue in 9 days.

Pacekeeper noticed at day 3 that the queue depth was growing faster than her review rate. It started throttling: raised auto-land thresholds (more goes through review, but less goes through), deferred non-urgent agent runs (Heretic, Annual Review prep, Synthesizer scheduled passes), batched notifications, paused Scout's RSS polling.

Day 9: she opens the Swift app. The standup says:

```
Pending: 14 diffs (oldest: 9 days ago)
Pacekeeper: throttled (most non-urgent agents paused)
Scout: paused; 38 articles queued for relevance check on resume
Cost: $4 month-to-date; 91% headroom
```

Most of the queue is not urgent. She bulk-stages four obviously-good link additions (`⌘-Shift-S`), discards two that are no longer relevant given a new note she made on the trip, snoozes three for next week, and reviews the remaining five carefully.

She commits with a one-line message. Pacekeeper detects the queue depth dropping and the recent activity rate; it relaxes back to normal pacing over the next day. Scout resumes; she gets a digest of the 38 articles ranked by relevance, accepts six for ingestion, dismisses the rest.

The system didn't punish her for being away. It throttled itself.

---

## The shape these scenarios share

Across all of them: **the human is the only entity that touches git history.** Every agent action is reviewable. Most actions are mechanical and quiet. The ones that aren't surface clearly. Capture works everywhere. The vault is always the user's; the agents are infrastructure.

If a scenario above feels right, the design is on track. If one feels awkward, that's a flag for design re-examination.
