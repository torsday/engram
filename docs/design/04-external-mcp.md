# External MCP: engram as your personal context layer

## Purpose

Engram exposes a second MCP server, distinct from the internal one used by Claude Desktop, designed for **the user's own client applications** to access the user's accumulated knowledge as context. Once you've built engram, every other app you build inherits a coherent picture of who you are: what you know, what you've thought about, what you prefer.

The motivating example: a travel app you build doesn't need to onboard you. It calls engram's `personal_context("travel")` and `preferences("travel")`, gets back a structured digest of your travel-related vault content + Biographer model + recent thinking + Trajectory data, and grounds its planning in that. After the trip, it deposits a literature note back into your vault. Your knowledge compounds; every app contributes to the same coherent model.

This reframes engram from "a personal knowledge tool" to "the persistent personal-context layer all your future apps build on."

## Design principles

1. **Two servers, one source of truth.** Internal MCP (full access, stdio, trusted) and external MCP (scoped, HTTP+SSE, authenticated) share the same underlying vault and tool implementations. The difference is the auth boundary.
2. **Default-deny.** Every external client gets only the scopes you explicitly approved. New scopes require new consent.
3. **Privacy zones are excluded by default.** `notes/work/`, `notes/medical/`, `notes/private/` (configurable) never leak through external MCP unless a scope explicitly opts in for that zone.
4. **High-level tools beat raw search.** External clients get tools shaped to _their_ use case (`personal_context`, `preferences`) in addition to the lower-level retrieval primitives. Reduces the chance the client's LLM mangles the data.
5. **Audited.** Every external call is logged with redacted args/responses. The user can review what each app accessed and revoke at any time.
6. **Bidirectional.** Clients can read context AND write back literature notes about what they did. Your vault grows from app activity.

## Transport and topology

- **Protocol:** MCP over HTTP+SSE (and WebSocket as a fallback). Not stdio --- external clients are not co-located with the engram process.
- **Local deployment:** When the client app and engram run on the same Mac, the external MCP server binds to `localhost:7878` (configurable). Apps connect via `http://localhost:7878/mcp`.
- **Mobile / remote:** Tunnel via Tailscale (recommended), local network with mDNS discovery (`engram.local:7878`), or self-hosted relay. iOS apps reach the Mac-hosted server through whichever transport the user has configured.
- **Cloud relay:** A future option (engram.cloud or self-hosted) for users who want their personal context accessible from anywhere without VPN. v2+; not required for v1.

### TLS

External MCP runs **without native TLS in v1**. The threat model (`09-threat-model.md`) treats the local machine as trusted; bearer tokens transit cleartext over loopback or LAN. For users needing remote access, the recommended path is:

- **Tailscale** (recommended): provides end-to-end WireGuard encryption transparently; the engram process sees plain HTTP on a tailnet IP.
- **Reverse proxy with TLS termination** (caddy, nginx, traefik): terminate TLS in front of `engram serve`; engram sees plain HTTP on `127.0.0.1`.

v2+ may add native TLS for cloud-relay scenarios. Until then, do not expose `engram serve` to the open internet without one of the above wrappers.

## Authentication and scopes

### API keys

Each registered client gets a unique API key (32-byte random, displayed once at registration, stored hashed with argon2). Sent as `Authorization: Bearer <key>` on every request.

Rotation: revocable from the Swift app. Revoking immediately invalidates the key; the client must re-register.

### Scopes (OAuth-style)

Permissions are expressed as scope strings, granted at registration time and modifiable later (with consent re-prompt for additions).

**Read scopes:**

| Scope                    | Grants                                                         |
| ------------------------ | -------------------------------------------------------------- |
| `personal_context:read`  | The high-level `personal_context` tool                         |
| `biography:read`         | Biographer's user model                                        |
| `preferences:read`       | Domain-specific preference extraction                          |
| `notes:read`             | All notes (subject to privacy zones)                           |
| `notes:read:tag/<tag>`   | Notes carrying a specific tag                                  |
| `notes:read:type/<type>` | Notes of a specific type (literature, evergreen, etc.)         |
| `notes:read:zone/<zone>` | Notes in a specific privacy zone (must be explicitly opted in) |
| `trajectory:read`        | Diachronic concept evolution data                              |
| `predictions:read`       | Prediction ledger and calibration data                         |
| `search:read`            | Hybrid search across permitted notes                           |

**Write scopes:**

| Scope                         | Grants                                                            |
| ----------------------------- | ----------------------------------------------------------------- |
| `notes:write:type/literature` | Create literature notes (most common write scope for client apps) |
| `notes:write:type/fleeting`   | Create fleeting notes (capture from external apps)                |
| `notes:append`                | Append to existing notes (e.g. activity log entries)              |
| `ask_user`                    | Route a question to the user via Swift app and await response     |

**Operational scopes:**

| Scope              | Grants                                             |
| ------------------ | -------------------------------------------------- |
| `events:subscribe` | Subscribe to SSE notifications about vault changes |
| `metrics:read`     | Read vault health metrics (no content)             |

### Privacy zones

Zones are configured in `.engram/config.toml`:

```toml
[[privacy_zones]]
path_prefix = "notes/work/"
external_default = "deny"

[[privacy_zones]]
path_prefix = "notes/medical/"
external_default = "deny"

[[privacy_zones]]
path_prefix = "notes/journal/"
external_default = "deny"
```

Default-deny means scopes like `notes:read` exclude these zones unless the client also holds the explicit `notes:read:zone/<zone>` scope. Zone opt-ins always require consent re-prompt.

### Consent flow

When a new client first connects:

1. Client sends a `register` request with: name, requested scopes, optional purpose statement.
2. Engram surfaces a consent prompt in the Swift app (push notification + in-app card):

   ```
   "Travel App" requests:
     • Read your personal context
     • Read travel-related notes
     • Read your preferences
     • Write literature notes about places researched
     • Ask you clarifying questions

   Purpose: "Plan trips grounded in your travel history and preferences."

   [Approve all]   [Customize]   [Deny]
   ```

3. On approval: API key generated, returned once to the client. Recorded in `mcp_clients` table with hashed key + scope list.
4. On future scope expansion requests: re-prompt for the new scopes only.

### Audit log

Every tool call by an external client is recorded in `mcp_access_log` with:

- Client ID + name
- Tool called
- Args summary (redacted: e.g. query strings preserved, but PII-detected fields hashed)
- Response summary (size, count, redacted)
- Timestamp, success/error

The Swift app exposes per-client audit views: "Travel App accessed your vault 47 times in the last 30 days. Most-called tool: `personal_context`. Last access: 2 hours ago. [Revoke]"

## HTTP endpoints (auth + lifecycle)

These are the non-tool endpoints supporting registration, key management, and the `ask_user` round-trip. Tools themselves are invoked via the MCP protocol over the established connection.

### `POST /mcp/register`

Initiates a registration request. The server stores it in `mcp_register_requests` with `status = pending` and pushes a consent card to the Swift app. The client polls or holds the connection until decision (or 5min timeout).

**Request:**

```json
{
  "name": "Travel App",
  "purpose": "Plan trips grounded in your travel history and preferences.",
  "requested_scopes": [
    "personal_context:read",
    "preferences:read",
    "notes:read:tag/travel",
    "notes:write:type/literature",
    "ask_user"
  ],
  "redirect_uri": "travelapp://engram/registered"
}
```

**Response (immediate):**

```json
{
  "request_id": "01JRZK...",
  "status": "pending",
  "expires_at": "2026-04-17T15:32:00Z",
  "poll_url": "/mcp/register/01JRZK.../status"
}
```

**Polling (`GET /mcp/register/:id/status`):**

Returns one of:

```json
{ "status": "pending" }

{ "status": "approved",
  "client_id": "01JRZK...",
  "api_key": "engram_sk_...32_bytes_hex...",
  "granted_scopes": [...] }

{ "status": "denied",
  "reason": "user_denied" }      // or "expired", "scopes_unavailable"
```

The `api_key` is returned **once only** at the moment of approval. The server stores only its argon2id hash (parameters per `03-architecture.md` §argon2 parameters). If the client loses the key, it must re-register.

### `POST /mcp/clients/:id/scopes` (scope expansion)

Existing client requests additional scopes. Same flow as initial registration but only the _new_ scopes appear on the consent card. On approval, the existing API key gains the new scopes; no new key is issued.

### `DELETE /mcp/clients/:id`

Revokes a client. Idempotent. Sets `revoked_at` on `mcp_clients`. Subsequent calls with the revoked key return 401.

### `GET /mcp/pending-questions/:question_id`

The client polls this endpoint for the answer to an outstanding `ask_user` call. See the `ask_user` round-trip schema below for full details.

## Tools exposed via external MCP

The external server exposes a curated subset of the internal tools, plus several higher-level tools designed for external client use cases.

### Personal context tools (the headline)

#### `personal_context(query, max_tokens=4000)`

The one tool every client should use first. Returns a structured digest combining:

- Relevant excerpt from `meta/biography.md` (Biographer's model, filtered to query topic)
- Top N relevant notes (hybrid search, scope-filtered)
- Trajectory snapshot for the query topic (if the diachronic trace feature has data for it)
- Recent thinking (last 30 days, scope-filtered)
- Stated preferences (if Biographer has captured any in this domain)

Output is a structured JSON document optimized for being injected into the client LLM's system prompt or context. Token budget enforced via summarization.

```json
{
  "query": "travel preferences and recent thinking",
  "biography_excerpt": "Lives in Seattle. Travels 3-4 times/year, mostly internationally. Strong preference for cities under 1M population. Avoids touristy zones. Has expressed repeated interest in Japan, Portugal, Slovenia.",
  "preferences": {
    "trip_length_typical_days": 10,
    "lodging_style": "boutique hotels or apartments, not chains",
    "pace": "1-2 cities per trip, slow",
    "avoid": [
      "all-inclusive resorts",
      "cruises",
      "highly-touristed sites at peak times"
    ]
  },
  "relevant_notes": [
    {
      "id": "...",
      "title": "Why I prefer second cities",
      "type": "evergreen",
      "snippet": "..."
    },
    {
      "id": "...",
      "title": "Tokyo trip 2025",
      "type": "literature",
      "snippet": "..."
    }
  ],
  "trajectory_summary": "2023: travel as productivity escape. 2024: shifted to travel as deliberate cultural immersion. 2026: growing interest in language-learning trips.",
  "recent_thinking": [
    {
      "id": "...",
      "title": "Possible Lisbon trip Q3",
      "modified": "2026-04-10",
      "snippet": "..."
    }
  ]
}
```

#### `preferences(domain, depth="summary")`

Domain-scoped preference extraction. `depth` is `"summary"` (one-paragraph) or `"detailed"` (structured).

Domains are open-ended strings; the agent layer (Biographer) maintains a list of well-developed domains based on vault content. Common ones: `"travel"`, `"reading"`, `"cooking"`, `"work-collaboration"`, `"learning"`.

```json
{
  "domain": "travel",
  "summary": "Prefers slow travel through smaller cities, with strong cultural and linguistic emphasis. Avoids touristy or commercial experiences. Travels in shoulder seasons.",
  "evidence_notes": ["...", "..."],
  "confidence": "high"
}
```

#### `recent_thinking_on(topic, days=30)`

Last N days of vault activity touching a topic. Lighter-weight than `personal_context` --- just modified notes with summaries.

#### `ask_user(question, context, urgency="normal", expires_in_seconds=86400)`

The client can route a question to the user via the Swift app:

```
Travel App asks:
  "I'm planning your Lisbon trip. Do you want this trip to lean toward
  relaxing or adventurous?"

  Context: "Researching Q3 trip per your earlier note."

[Reply]   [Skip]   [Mute this app for 24h]
```

`urgency` controls notification behavior: `low` (no notification, surface in inbox), `normal` (badge), `high` (push notification).

##### Round-trip schema

The call is asynchronous. Sequence:

1. **Client invokes the tool** with question + context + urgency + optional `expires_in_seconds` (default 86400 = 24h, max 604800 = 7d).
2. **Server inserts a row** into `pending_questions` (`status = pending`) and pushes a notification to the Swift app per urgency.
3. **Server returns immediately** with a `question_id` and the polling URL. The MCP tool call does not block.

   ```json
   {
     "question_id": "01JRZK...",
     "status": "pending",
     "expires_at": "2026-04-18T15:32:00Z",
     "poll_url": "/mcp/pending-questions/01JRZK..."
   }
   ```

4. **The user takes one of three actions** in the Swift app:
   - **Reply:** types or speaks an answer. `pending_questions.status = answered`, `answer = <text>`, `user_action = reply`.
   - **Skip:** dismisses without answering. `status = skipped`, `user_action = skip`.
   - **Mute this app for 24h:** dismisses + adds the client to a 24h notification mute list. `status = skipped`, `user_action = mute_app_24h`. Subsequent `ask_user` calls from this client during the mute window return immediately as `skipped` without notifying.

5. **Client polls** `GET /mcp/pending-questions/:question_id`:

   ```json
   { "status": "pending" }                              // not yet acted on
   { "status": "answered", "answer": "adventurous" }    // user replied
   { "status": "skipped", "reason": "user_skipped" }    // user dismissed
   { "status": "skipped", "reason": "muted_for_24h" }   // app is muted
   { "status": "expired" }                              // past expires_at
   ```

   Recommended polling cadence: client-side exponential backoff starting at 5s, max 60s. Or subscribe via the `events:subscribe` scope and SSE for push notification.

6. **Client handles each terminal status** appropriately. A skipped or expired question typically means the client proceeds with a default or asks differently next time.

##### Quotas and abuse prevention

Per `03-architecture.md` §Rate limiting: 20 `ask_user` calls per client per day. Hitting the quota returns 429 with `Retry-After` set to seconds-until-midnight-UTC.

A client that accumulates many `skipped` responses in a short window is auto-muted: > 3 consecutive skips → 6h mute; > 5 → 24h. The user can manually un-mute from the MCP client manager.

##### Privacy implications

Questions and answers are stored in `pending_questions` and visible in the Swift app's MCP audit view. Answers are visible only to the requesting client (delivered once, then redacted from polling responses after 5min of completion to bound exposure window). The user can purge `pending_questions` history at any time via the Swift app.

#### `record_session(summary, tags, type="literature")`

The client writes back a literature note about what it did. Goes through the standard ingestion pipeline (Scribe formats, Linker proposes connections, human approves before landing).

```json
{
  "summary": "Researched 3 Lisbon neighborhoods: Alfama, Príncipe Real, LX Factory area. Decided on Príncipe Real for boutique character + walkability.",
  "tags": ["travel", "lisbon", "trip-planning"],
  "type": "literature",
  "metadata": {
    "session_started": "2026-04-15T14:00:00Z",
    "session_duration_minutes": 45,
    "external_links": ["https://...", "https://..."]
  }
}
```

### Lower-level tools (subset of internal MCP)

These are also exposed externally for clients that want raw access. Scope-gated.

- `search_notes(query, filters)` --- requires `search:read`. Filters honor scopes.
- `read_note(id)` --- requires `notes:read` (or zone scope). Returns 404 if scope insufficient.
- `list_tags()` --- requires `notes:read`. Returns only tags from accessible notes.
- `follow_links(id, direction)` --- requires `notes:read`. Returns only accessible neighbors.
- `recent_changes(days, filters)` --- requires `notes:read`. Scope-filtered.

### What is NOT exposed externally

The following internal tools are never exposed to external clients:

- `write_note` (raw, unstructured) --- external writes go through `record_session` which enforces type and reviewability
- Direct deliberation control
- Direct agent invocation (`engram run <agent>`)
- Direct sqlite access
- The internal `vault_health` tool (a sanitized `metrics:read` exists for external use)

## Concrete: a travel app, end to end

You're building a travel app. Walkthrough of how it integrates:

### Registration (one-time)

App opens, prompts you: "Connect to engram?" You approve. App calls:

```http
POST http://localhost:7878/mcp/register
Content-Type: application/json

{
  "name": "Travel App",
  "purpose": "Plan trips grounded in your travel history and preferences.",
  "requested_scopes": [
    "personal_context:read",
    "preferences:read",
    "notes:read:tag/travel",
    "notes:write:type/literature",
    "ask_user"
  ]
}
```

Engram pushes a consent card to your Swift app. You approve. The travel app receives:

```json
{
  "client_id": "01JRZK...",
  "api_key": "engram_sk_...32_bytes_hex...",
  "granted_scopes": [...]
}
```

### Trip planning session

You tell the travel app "plan a trip for Q3." It calls:

```http
GET /mcp/tools/personal_context
Authorization: Bearer engram_sk_...

{
  "query": "travel preferences, recent travel thinking, and any Q3 plans",
  "max_tokens": 4000
}
```

Engram returns the structured digest above. The travel app's LLM has rich grounding.

App calls `preferences("travel", depth="detailed")` to confirm specifics. Plans an outline. Has one ambiguity, calls:

```http
POST /mcp/tools/ask_user
Authorization: Bearer engram_sk_...

{
  "question": "Lean relaxing or adventurous for this one?",
  "context": "Q3 trip planning, currently considering Lisbon vs. Slovenia",
  "urgency": "normal"
}
```

You get a Swift app card, tap "Adventurous." App receives the answer asynchronously, finalizes plan.

### Writing back

App finishes with research. Calls:

```http
POST /mcp/tools/record_session
Authorization: Bearer engram_sk_...

{
  "summary": "Q3 trip plan: 10 days Slovenia. Ljubljana 3 nights, Lake Bled 2, Soča valley 5. Booked Hotel Cubo for Ljubljana. Researched paragliding in Bovec. Concerns: shoulder-season weather variability.",
  "tags": ["travel", "slovenia", "trip-2026-q3"],
  "type": "literature"
}
```

This drops into your review queue as a proposed literature note. You approve in the Swift app. Now the note is in your vault. Linker proposes connections to your existing travel evergreen notes. Biographer updates next pass. Next time _any_ app calls `personal_context("travel")`, the picture is richer for it.

## Future client app sketches

The pattern works for many apps. A few that motivate the design:

- **Reading queue app.** Reads `personal_context("reading interests")`, suggests books from external catalogs that match your stated curiosities and don't duplicate what you've already read (per literature notes).
- **Health journal app.** Holds `notes:read:zone/medical` (explicitly opted in), tracks symptoms/measurements, writes back literature notes summarizing trends.
- **Code review companion.** Reads `personal_context("software architecture preferences")`, applies your stated principles when reviewing your own PRs.
- **Therapy / reflection prompt app.** Reads `personal_context("recent emotional themes")` (with strict zone gating), suggests reflection prompts; never writes back without explicit consent per session.
- **Calendar prep.** Reads upcoming events (via OS calendar API) + `personal_context(person_name)` for each attendee, surfaces a brief before each meeting (essentially Conversation Prep as a standalone app).

## Open questions

- **Multi-user.** v1 is single-user. If engram becomes multi-tenant later, the external MCP layer needs per-user namespacing of clients, scopes, and audit logs. Defer.
- **Cross-device latency.** When the client is on iOS and engram runs on the Mac, network round-trips may matter for `personal_context` (large response). Caching strategy at the client side, with cache-invalidation hints from engram via SSE.
- **LLM-side prompt injection.** A malicious note in the vault could try to manipulate the client's LLM through `personal_context` output. Mitigation: structure outputs as data (JSON), not free prose; client-side input validation; engram-side filtering of suspicious patterns. Real but not unique to engram.
- **Quotas.** Should clients have rate limits or token quotas to prevent runaway calls? Probably yes; configurable per client. v1 may ship with simple per-minute rate limits and add quotas later.
- **App-to-app context flow.** Could the travel app's `record_session` output be made discoverable to the calendar app for an upcoming trip? Probably yes via tags/types, mediated through the vault. Worth thinking through, not implementing yet.
