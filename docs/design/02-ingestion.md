# Ingestion Pipeline

## Purpose

Ingestion is how external material enters the vault. The user drops a file (PDF, image, audio, URL, document) into the Swift app, and engram produces a structured literature note linked to the original artifact. The pipeline is fully autonomous but the final literature note enters the review queue for human approval before landing.

The cardinal rule: **ingestion never produces an evergreen note directly.** It produces a literature note --- source-oriented, clearly attributed, linked to the raw artifact. Evergreen synthesis is a separate, council-gated step that happens downstream.

---

## Pipeline

```
Input (Swift app drop / share-sheet / CLI / API)
    |
    v
1. RECEIVE
   - File uploaded via POST /ingest (multipart, streamed)
   - Content-addressed storage: SHA-256 hash of bytes
   - Stored at .engram/artifacts/<sha256-prefix>/<sha256>.<ext>
   - Metadata row in sqlite: filename, mime, size, source_url, dropped_at
   - Dedup check: if hash exists, surface the existing literature note
   - Status: received
    |
    v
2. CLASSIFY
   - Ingestor agent determines the file type and extraction strategy
   - Categories: academic_paper, article, book_chapter, screenshot,
     diagram, voice_memo, podcast, video, web_page, document, raw_text, unknown
   - Classification stored in artifact metadata
   - Status: classified
    |
    v
3. EXTRACT
   - Dispatcher routes to the appropriate extractor(s):

     | Category        | Primary extractor            | Fallback              |
     |-----------------|------------------------------|-----------------------|
     | PDF/DOCX        | Claude vision API            | pdftotext / docx2txt  |
     | Image           | Claude vision API            | ocrs (Rust-native OCR)|
     | Screenshot      | Claude vision API            | ocrs                  |
     | Voice memo      | whisper.cpp (local)          | Whisper API (cloud)   |
     | Podcast/audio   | whisper.cpp (local, chunked) | Whisper API           |
     | Video           | audio track -> whisper.cpp   | key frames -> vision  |
     | Web URL         | readability extraction       | monolith snapshot     |
     | Raw text/md     | passthrough                  | ---                   |

   - Extracted text cached by artifact hash in sqlite
   - For web URLs: full HTML archived as artifact (link-rot protection)
   - Status: extracted
    |
    v
4. DRAFT
   - Scribe (literature mode) formats the extracted content into a literature note:
     - Frontmatter (lean, per 06-note-conventions.md): id, title, type, status,
       authors, published, source_url, tags
     - Body: structured summary, key claims, notable quotes (short, attributed)
     - Links: proposed connections to existing vault notes (via Linker)
   - Sidecar JSON at .engram/sidecar/<id>.json gets: ingestion metadata
     (source_artifact hash, source_type, ingested_at, ingested_by, extractor +
     version), provenance history, embedding metadata
   - Status: drafted
    |
    v
5. REVIEW
   - Literature note enters the human review queue
   - Visible in: Swift app review tab, .engram/proposals/, CLI
   - Human may: approve (lands in notes/literature/), edit + approve,
     reject, or request re-extraction with different settings
   - Status: approved | rejected
    |
    v
6. DOWNSTREAM (asynchronous, after approval)
   - Linker: proposes wikilinks from existing notes to the new literature note
   - Synthesizer: may propose evergreen notes derived from the content
   - Both go through standard council deliberation
```

---

## Artifact storage

### Content-addressed filesystem

```
.engram/
  artifacts/
    a3f4e2/
      a3f4e2...full-sha256.pdf       # original file, never mutated
    7b91c0/
      7b91c0...full-sha256.m4a       # audio recording
  index.sqlite                        # the `artifacts` table inside the main
                                      # index holds: hash, filename, mime, size,
                                      # source, dropped_at, classification,
                                      # extraction_status, literature_note_id.
                                      # See 03-architecture.md schema section.
```

### Why outside git

- A 30MB PDF in every clone forever is bad.
- Git-LFS adds setup friction for a personal tool.
- The literature note (plain text, small) is in git. The artifact is referenced by hash.
- If the artifact is missing on a given machine, the literature note still makes sense.
- Engram fetches artifacts on demand.

### Optional remote backup

Artifacts may be synced to a user-configured backend, keyed by hash:

- Local-only (default)
- S3-compatible (Backblaze B2, MinIO, AWS)
- iCloud Drive folder (via symlink)

Configuration in `.engram/config.toml`:

```toml
[artifacts]
storage = "local"                          # or "s3", "icloud"
# s3_bucket = "my-engram-artifacts"
# s3_endpoint = "https://s3.us-west-001.backblazeb2.com"
```

---

## Literature note format

```markdown
---
id: 01JRZK4N8Q...
title: "Attention Is All You Need"
type: literature
status: approved
authors: ["Vaswani et al."]
published: 2017
source_url: https://arxiv.org/abs/1706.03762
tags:
  - topic/transformers
  - topic/attention
---

<!-- Sidecar at .engram/sidecar/01JRZK4N8Q.json holds:
     source_artifact hash, source_type classification, ingested_at,
     ingested_by, extraction model + version, and provenance history.
     See 06-note-conventions.md. -->


# Attention Is All You Need

## Summary

[Scribe-generated summary, 3-5 sentences. Attributed via HTML comment.]

<!-- by: scribe deliberation: none -->

## Key claims

- [Claim 1, one sentence]
- [Claim 2, one sentence]
<!-- by: scribe -->

## Connections

- Related to [[Self-attention as soft dictionary lookup]]
- Contrasts with [[Recurrence is necessary for sequence modeling]]
<!-- by: linker -->

## Raw extraction

[Extracted text, or link to extracted text if large]

<!-- extracted_by: claude-vision -->
```

---

## Privacy routing

Some artifacts should never leave the local machine. Engram supports per-drop and per-folder privacy controls:

### Per-drop

The Swift app offers a "Process locally only" toggle on the drop UI. When enabled:

- Extraction uses local-only tools (ocrs, whisper.cpp, pdftotext)
- No cloud LLM calls for this artifact
- Literature note drafting uses the local model (Ollama)
- Artifact is never synced to remote storage

### Per-folder

In `.engram/config.toml`:

```toml
[[privacy_zones]]
path_prefix = "notes/work/"
cloud_allowed = false

[[privacy_zones]]
path_prefix = "notes/medical/"
cloud_allowed = false
```

Any artifact whose literature note would land under a privacy zone is automatically routed to local-only processing.

### Frontmatter flag

Notes processed locally carry `privacy: local-only` in frontmatter. Agents respect this: they will not send the note's content to cloud LLMs.

---

## Deduplication

When a dropped file's SHA-256 matches an existing artifact:

1. The upload is not stored again.
2. The existing literature note is surfaced to the user.
3. The user may choose to:
   - View the existing note (most common).
   - Re-extract with different settings (e.g., better model, different extraction strategy).
   - Create a second literature note (rare; for when the same source is being read for a different purpose).

---

## Batch ingestion

Dropping a folder or multiple files triggers parallel processing:

- Each file enters the pipeline independently.
- Extraction is parallelized up to a configurable concurrency limit (default: 4).
- The Swift app shows per-file progress via SSE.
- Backpressure: if the extraction queue exceeds 20 items, new drops are queued with an ETA.
- Long extractions (2-hour podcast) run in the background; the user is notified on completion.

---

## Extraction quality and re-runs

Extraction results are cached by `(artifact_hash, extractor_version, model_version)`. This means:

- Upgrading whisper.cpp or the vision model invalidates the cache for affected artifacts.
- The user (or the Watcher agent) can trigger re-extraction when a better extractor is available.
- Re-extraction produces an updated literature note draft, which goes through the review queue again.

---

## Edge cases

- **Unsupported file types** (binary, executables, very large files): Store artifact, create a stub literature note with metadata only, no extraction attempted. Human can add notes manually.
- **Corrupt files**: Extraction fails gracefully. Status set to `extraction_failed`. User notified. Artifact preserved for manual inspection.
- **Partial extraction** (e.g., OCR confidence below threshold): Literature note includes a `extraction_confidence: low` flag. Scribe adds "[low confidence]" markers on uncertain sections.
- **Web URLs that require authentication**: Engram stores whatever is publicly accessible. If nothing can be extracted, the literature note contains the URL and metadata only.
