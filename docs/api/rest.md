# REST / SSE API reference

The engram daemon exposes a REST API for local tooling and the SwiftUI app.
Server-Sent Events (SSE) endpoints stream real-time agent activity and
diff-review queue updates.

> **Not yet implemented** — the REST/SSE server (`engram-api` crate) is under
> active development. This stub will be replaced with auto-generated content
> from the OpenAPI spec once the server ships. See `docs/design/03-architecture.md`
> §API surface for the planned endpoint surface.

## Planned base URL

```
http://127.0.0.1:PORT   (port configurable; default TBD)
```

## Planned endpoints (from design)

| Method | Path              | Description                        |
| ------ | ----------------- | ---------------------------------- |
| GET    | `/health`         | Daemon liveness check              |
| GET    | `/status`         | Full status (vault, index, agents) |
| GET    | `/notes`          | List notes (filtered)              |
| GET    | `/notes/:id`      | Fetch a single note                |
| GET    | `/diff-queue`     | Pending agent proposals            |
| POST   | `/diff-queue/:id` | Approve / reject a proposal        |
| GET    | `/events`         | SSE stream of agent activity       |

Full OpenAPI spec and Swagger UI (`/docs`) will be available when the server
ships. The static markdown version of the spec will live here.
