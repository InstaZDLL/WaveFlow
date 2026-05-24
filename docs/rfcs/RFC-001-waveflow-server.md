# RFC-001 — WaveFlow Server, Web App, and the path to multi-device

- **Status**: Accepted
- **Date**: 2026-05-24
- **Authors**: @InstaZDLL
- **Supersedes**: —
- **Implementation tracking**: to be opened as a GitHub project once 1.a lands

---

## 1. Context

WaveFlow has reached the end of Phase 0 of the [roadmap](../../README.md): animations and the 14-preset theme system shipped in v1.3.0. The next chapter is `waveflow-server` — the backend that unlocks library sync across devices, web playback, and (later) the mobile app.

This is the biggest architectural inflection point the project has had. Decisions taken here will constrain Phase 2 (community-DB), Phase 3 (plugin SDK), and Phase 4 (mobile). The purpose of this document is to lock those decisions in writing **before** writing code, so we don't paint ourselves into a corner discovered halfway through a 6-month build.

The desktop app stays the primary surface. Everything below is additive — no existing user is forced to set up a server. Local-only mode remains a first-class supported configuration forever.

## 2. Goals

- Multi-device sync of library metadata, playlists, likes, listening history.
- Web playback from any browser, including a public SEO-indexable surface for shared playlists.
- A clean separation that lets a mobile app (Phase 4) plug in without rework.
- Self-hostable single-binary server. No Docker required for a small install (LAN, family).
- Reuse the existing Rust scanner / metadata / Deezer / Last.fm code rather than rewrite it.

## 3. Non-goals

- **Hosted SaaS.** WaveFlow stays self-hosted. A hosted instance may exist later but is not part of Phase 1's design.
- **Real-time co-listening.** No "listen together" feature in this RFC.
- **Streaming from third-party services.** Spotify/Deezer/Apple integrations are not playback sources, only metadata enrichment.
- **Sharing the desktop React codebase with the web.** Confirmed in §6.1. The web is its own app with its own UI.
- **Sub-3-month delivery.** Phase 1 is planned as 7 sub-phases delivered incrementally. Estimated total ~4-6 months of focused work.

## 4. Architecture overview

```text
┌─────────────────────────────────────────────────────────────────┐
│                     shared PostgreSQL                           │
│  users · sessions · profiles · libraries · tracks · playlists   │
│  artists · albums · likes · history · sync_ops                  │
└──────┬──────────────────────────────────────────────────┬───────┘
       │                                                  │
┌──────┴──────────────────────┐              ┌────────────┴────────┐
│  waveflow-server (Rust)     │              │  waveflow-web (TS)  │
│  axum                       │              │  TanStack Start     │
│                             │              │  Better Auth        │
│  • streaming (HTTP range)   │              │                     │
│  • transcode cache (FFmpeg) │              │  • SSR / SSG pages  │
│  • sync ops + WebSocket     │              │  • browser player   │
│  • scanner / cron           │              │    (Web Audio API)  │
│  • REST API for all clients │              │  • public playlists │
│  • JWT verification (JWKS)  │              │    (SEO + OG)       │
│                             │              │  • BFF proxy → axum │
└─────────────────────────────┘              └─────────────────────┘
        ▲                ▲                            ▲
        │                │                            │
  Tauri desktop    Mobile (Expo,                Web browser
  (existing)        Phase 4)
```

Three independent processes, one shared database, one shared JWT format. No process talks to another's internal state directly — all communication is HTTP or WebSocket with versioned schemas.

## 5. Repositories

Three top-level repos, no cross-language monorepo:

| Repo | Language | Purpose |
|---|---|---|
| `waveflow` (existing) | Rust + TS | Desktop Tauri app. Hosts the extracted `waveflow-core` crate as part of its workspace. |
| `waveflow-server` | Rust | axum HTTP/WS server. Depends on `waveflow-core` via git dependency, then crates.io later. |
| `waveflow-web` | TS | TanStack Start frontend + Better Auth. Will become a TS monorepo when `waveflow-mobile` joins (shared design tokens, i18n, types). |

**Why not a single monorepo:** Rust + TS in the same workspace with Turborepo/Nx adds tooling pain (rust-analyzer scope, bun lockfile vs cargo lockfile, CI matrix complexity) without enough shared code to justify it. The only cross-language artifact is generated API types, which can be published as a tiny npm package from a CI step in `waveflow-server`.

**Why not split `waveflow-core` immediately:** until its public API stabilizes (likely 2-3 months of usage), keeping it in the desktop repo avoids a versioning nightmare. The first refactor (1.a) extracts it as a workspace crate in `waveflow`. When stable, it moves out — that's a 1-day operation (`git filter-repo` + path update), not a design decision to make now.

## 6. Decisions

### 6.1 Distinct UIs for desktop and web

The desktop React tree under `src/` will **not** be reused for the web. Surfaces have genuinely divergent constraints:

| Surface | Session shape | Constraints |
|---|---|---|
| Desktop | Long, single window, keyboard | EQ, DSD, mini-player, immersive lyrics, OS integrations |
| Public web | Short, anonymous, often mobile browser | SEO, OG images, fast TTFB, no chrome |
| Logged web | Variable, browser, may be office | Lightweight player, playlist mgmt, no DSD/EQ |
| Mobile | Touch, lock screen, intermittent network | Vertical lists, gestures, offline cache |

Reusing the 80px PlayerBar and 240px sidebar on a phone via browser would be bad. Shared code happens at the data layer (API types) and design layer (tokens, i18n strings) — not the component layer.

**Shared TS packages** (`waveflow-web` monorepo from day one):
- `@waveflow/design-tokens` — OKLCH palettes for the 14 themes, spacing scale, radius scale. Consumed by web + desktop (desktop replaces hardcoded values in [src/lib/themes.ts](../../src/lib/themes.ts) progressively).
- `@waveflow/locales` — the 17 JSON locale files moved out of [src/i18n/locales/](../../src/i18n/locales/) and consumed by both.
- `@waveflow/api-types` — TypeScript types generated from utoipa OpenAPI spec emitted by `waveflow-server`.

### 6.2 Server stack: axum (Rust)

**Decision: axum.** Rejected Bun + Hono/Elysia.

Rationale: ~70% of the server's work is binary processing — BLAKE3 hashing, lofty tag parsing, ReplayGain analysis, FFmpeg transcode, range-streaming, MusicBrainz lookups. This code already exists in [src-tauri/src/](../../src-tauri/src/) and is battle-tested. Rewriting it in TypeScript would forfeit the existing scanner pipeline for marginal DX gain on CRUD endpoints.

Tradeoff accepted: writing CRUD handlers in axum is more boilerplate than Hono. We mitigate with `utoipa` annotations + macros.

**Crates locked in for v1:**
- `axum` 0.7+, `tokio` 1.x, `tower-http` for middleware
- `sqlx` 0.8+ with `postgres` feature (already used in desktop with `sqlite`)
- `utoipa` for OpenAPI spec + Swagger UI
- `jsonwebtoken` for JWT verification (JWKS endpoint cached)
- `tokio-tungstenite` for WebSocket
- `reqwest` with `rustls-tls` (already used)
- `notify` for filesystem watching (already used)

### 6.3 Web stack: TanStack Start + Better Auth

**Decision: TanStack Start.** Rejected Next.js and Nuxt.

| Option | Rejected because |
|---|---|
| **Nuxt** | Vue. Forces abandoning the React ecosystem. Massive sunk cost (i18n setup, theme system, component patterns). |
| **Next.js (App Router)** | App Router forces React Server Components, which add a learning curve and a deployment-shape constraint (Node runtime semantics). Vercel-isms leak into self-hosted deploys. Heavier framework footprint. |
| **TanStack Start** | ✅ Chosen. Closer to the metal, file-based routing, SSR/SSG/streaming without forced RSC, coherent with TanStack Query/Virtual already used on desktop, stable since late 2025. |

Tradeoff accepted: smaller community, fewer deploy adapters than Next. For a self-hosted project where we control the deploy target, this is acceptable.

**Better Auth** owns authentication entirely. Magic-link email for v1 (zero password storage, zero OAuth friction). OAuth (Google, GitHub) added in v2. Passkeys evaluated for v3.

Better Auth lives in `waveflow-web`. The server-side handlers run inside TanStack Start (Vinxi). axum only verifies the JWTs Better Auth issues — it does not handle login flows.

### 6.4 Authentication boundary

```text
Login flow:
  1. User → waveflow-web (TanStack Start)
  2. Better Auth issues JWT (RS256, exp = 1h) + refresh token (90 days)
  3. JWT is signed with the server's private key; public key exposed at
     https://web.example/.well-known/jwks.json
  4. waveflow-web stores tokens in HttpOnly Secure cookies for browser use
  5. Desktop / mobile receive tokens via in-app webview during login

API call flow:
  Client (web / desktop / mobile) → axum
    Header: Authorization: Bearer <JWT>
  axum verifies signature against JWKS (cached 1h)
  axum extracts sub (user_id) + scope claims
  axum queries Postgres
```

**Why JWT and not session cookies talking to axum directly:** the desktop and mobile apps need to make API calls outside browser context. JWT is the standard for bearer-token auth across heterogeneous clients. Better Auth supports JWT issuance with JWKS rotation out of the box.

**Token storage on desktop:** the Tauri app stores the refresh token in OS-keyring via `tauri-plugin-stronghold` or the equivalent secure-storage plugin. Access tokens stay in memory.

### 6.5 Database: PostgreSQL

**Decision: PostgreSQL** for the server. SQLite stays for desktop local-only mode.

Rationale:
- Multi-user concurrent writes break SQLite's single-writer model.
- Logical replication and `LISTEN/NOTIFY` give us free WebSocket fan-out for sync.
- `sqlx` already used in desktop; switching the feature flag from `sqlite` to `postgres` reuses the query layer.

**The `waveflow-core` crate exposes traits, not concrete connections.** It defines `trait TrackRepository`, `trait PlaylistRepository`, etc., with two implementations: `SqliteTrackRepository` (desktop) and `PostgresTrackRepository` (server). Same business logic, different storage.

Migration strategy: server migrations live in `waveflow-server/migrations/` and are separate from desktop. They are immutable once merged, same rule as desktop ([CLAUDE.md](../../CLAUDE.md)).

### 6.6 Sync protocol

**Decision: append-only operations log with a two-level clock — device-side Lamport clock for local ordering, server-assigned monotonic sequence as the authoritative conflict-resolution key. Tombstones for deletes.**

Rejected full CRDTs (Yjs / Automerge) — overkill for likes / playlists / history, and the JSON-binary overhead is non-trivial on a multi-thousand-track library.

Rejected "Lamport clock alone as LWW key" — vulnerable to clock-inflation by a misbehaving or out-of-date client. The server must remain the source of truth for ordering.

```sql
CREATE TABLE sync_op (
  id              BIGSERIAL PRIMARY KEY,  -- server sequence, authoritative ordering
  user_id         UUID NOT NULL,
  device_id       UUID NOT NULL,
  operation_id    UUID NOT NULL,      -- client-generated, stable across retries (idempotency key)
  entity_type     TEXT NOT NULL,      -- 'track', 'playlist', 'like', ...
  entity_id       TEXT NOT NULL,      -- stable cross-device id
  op              TEXT NOT NULL,      -- 'upsert', 'delete'
  field           TEXT,               -- NULL for delete; column name for upsert
  value           JSONB,              -- new value
  lamport_ts      BIGINT NOT NULL,    -- device-side clock, used for local merging
  wall_clock      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE (user_id, device_id, operation_id),
  UNIQUE (user_id, device_id, lamport_ts)
);

CREATE INDEX sync_op_pull_idx ON sync_op (user_id, id);

-- Per-device pull cursor. Drives the compaction job (only ops older than
-- min(last_seen_id) across all of a user's devices are safe to compact).
CREATE TABLE device_sync_cursor (
  user_id         UUID NOT NULL,
  device_id       UUID NOT NULL,
  last_seen_id    BIGINT NOT NULL DEFAULT 0,
  last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY (user_id, device_id)
);

-- Composite index for the compaction MIN(last_seen_id) query with the
-- staleness predicate on last_seen_at. Lets Postgres do an index-only
-- scan instead of a heap fetch per row.
CREATE INDEX device_sync_cursor_compaction_idx
  ON device_sync_cursor (user_id, last_seen_at, last_seen_id);

-- High-water mark of compacted ops, per user. Pull handlers consult this
-- to detect resurrected devices whose requested `since` falls below the
-- last truncated id (those devices have lost ops and must full-resync).
CREATE TABLE sync_compaction_watermark (
  user_id           UUID PRIMARY KEY,
  compacted_up_to   BIGINT NOT NULL DEFAULT 0,
  updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

`operation_id` is a client-generated UUID (v4 or v7) attached to each op at the moment it is created locally and persisted with it. It must remain stable across retries — a client that re-sends an op after a 409 monotonicity reject **must** reuse the same `operation_id`, only the `lamport_ts` changes. Clients that cannot generate UUIDs may use a deterministic content-hash `BLAKE3(entity_id || field || value || device_id || local_serial)` instead; the value must remain stable across retries of the same logical operation.

**Device-side Lamport clock rules** (every client must implement these):

1. Each device maintains a single `local_lamport` counter, persisted across restarts.
2. **On every local op**, increment first, then assign: `local_lamport += 1; op.lamport_ts = local_lamport`.
3. **On every received remote op** (via pull or WebSocket), merge clocks before applying: `local_lamport = max(local_lamport, remote.lamport_ts) + 1`.
4. The counter is monotonic on a given device; it never decreases.

**Pull**: `GET /sync/ops?since=<last_seen_id>` returns ops with `id > since` ordered by `id` ASC. The endpoint requires the caller's `device_id` (extracted from a header or JWT claim).

- **First sync**: a brand-new device has no cursor. It calls `GET /sync/ops` (no `since` parameter) or equivalently `GET /sync/ops?since=0`; both are treated identically and return every op the user has from `id > 0`. The server lazily creates the `device_sync_cursor` row on first ACK (see below).
- **Subsequent sync**: client passes its locally-stored `last_seen_id`.
- **Resurrected-device guard**: before streaming ops, the server checks `sync_compaction_watermark`. If `since > 0 AND since < compacted_up_to`, the requested range has been partially or fully truncated by compaction. The server responds **`410 Gone`** with `{"type": "resync_required", "compacted_up_to": <n>}` (or, over WebSocket, a `{"type": "resync_required", "compacted_up_to": <n>}` frame followed by connection close). On receipt, the client wipes its local op-derived state for the affected entities and re-pulls from `since=0`. The `since > 0` half of the predicate is what makes the recovery loop terminate: a full-resync request (`since=0` or absent) always proceeds to the normal Pull path, even when `compacted_up_to > 0`, because the client is explicitly asking for "everything you still have". Without the guard, a resurrected device would silently apply a truncated delta — strictly worse than a hard error.
- **Client-side apply order**: apply ops in `id` ASC, run the Lamport merge rule (#3 above) for each before applying locally, then advance the local `last_seen_id` to the highest `id` durably persisted.
- **Server-side cursor advancement** — **ACK is the only authoritative source**. The server does **not** write `device_sync_cursor.last_seen_id` at the end of a Pull response (response sent ≠ client durably applied). The client must explicitly acknowledge applied ranges:
  - REST clients: `POST /sync/ack { "last_seen_id": <highest_id_durably_applied> }`.
  - WebSocket clients: periodic `{ "ack": <highest_id_durably_applied> }` frames on the same channel.
  - The server upserts `device_sync_cursor (user_id, device_id, last_seen_id, last_seen_at)` on each ACK whose `last_seen_id` is strictly greater than the stored value (older or equal ACKs are no-ops).
- **ACK debouncing on the server**: high-throughput clients may emit ACK frames every op. The server may debounce writes to `device_sync_cursor` — accumulate the latest ACK in memory per `(user_id, device_id)` and flush either every N seconds (default 5s) or on disconnect / shutdown. This is a write-amplification optimization only; the in-memory value is always used when computing the compaction MIN so debouncing never causes premature compaction.
  - **Crash-safety invariant**: if the server crashes before flushing an in-memory ACK, Postgres retains the previous (lower) `last_seen_id`. The next compaction reads the persisted (lower) MIN and is therefore *more* conservative than it would have been with the in-memory value. **A crash can never make compaction more aggressive** — it can only leave a slightly larger ops backlog for one more cycle. This is the property that makes debouncing safe; any future change that breaks this invariant (e.g., flushing optimistically before durable apply confirmation) requires updating this section.

**Push**: `POST /sync/ops` with the client's pending ops batch. Each op carries its client-generated `operation_id`. For every op the server processes:

1. **Idempotency check**: if a row with the same `(user_id, device_id, operation_id)` already exists, the server skips insertion and returns the stored row's `id` + `lamport_ts` in the response payload as if it had just been inserted. **The server responds `200 OK` with the existing row, never a raw DB unique-violation error.** This lets a client that crashed mid-push (or mid-ack) safely retry without producing semantic duplicates.
2. **Per-device monotonicity check** (only when step 1 did not match an existing op): rejects any op whose `lamport_ts` is `<=` the highest `lamport_ts` already stored for this `(user_id, device_id)`. A reject returns `409 Conflict` with the stored max so the client can re-merge its clock and retry. **The retry must reuse the same `operation_id`** so step 1 keeps the operation idempotent if the original somehow landed.
3. Assigns `id` via `BIGSERIAL` inside a transaction (the authoritative global order for this user).
4. Broadcasts the assigned op(s) via WebSocket to other devices of the same user.

**Conflict resolution**: per `(entity_id, field)`, **the op with the highest server-assigned `id` wins**. `lamport_ts` is no longer the resolution key — it is a device-side ordering hint that helps the client reason about its own pending ops before the server round-trip. Because `id` is `BIGSERIAL` assigned inside a transaction, ties are impossible by construction; the deterministic tie-breaker on `device_id` is therefore unnecessary.

**Why this hybrid**: Lamport clocks alone are correct only if every participant respects the protocol. A buggy or malicious device that ships `lamport_ts = i64::MAX` would win every conflict forever. By making the server-assigned `id` the LWW key, the server retains final authority while the Lamport clock still gives clients a useful local ordering signal between sync round-trips.

**What syncs**: playlists, playlist tracks, likes, listening history, ratings, smart-playlist rules.

**What does not sync**: local folder mounts (device-local), EQ presets per-device (Phase 1 — may change later), playback position (out of scope for v1).

### 6.7 Streaming

**Decision: HTTP range requests on flat files for native formats. Transcode on demand only when client signals incompatibility.**

```http
GET /stream/:track_id?format=auto
  Header: Accept: audio/flac, audio/mpeg, audio/ogg
  Header: Range: bytes=0-65535

Server logic:
  1. Resolve track → file path
  2. Probe file format via lofty (cached)
  3. If client Accept includes the file's native format → stream raw with Range
  4. Else → spawn FFmpeg subprocess, transcode to FLAC (lossless) or Opus 192k
     (lossy fallback), cache the result keyed by (track_id, target_format)
  5. Cache eviction: LRU, configurable cap (default 5 GB)
```

DSD source files: always transcoded to FLAC 24/96 via the existing in-house DSD→PCM converter ([src-tauri/src/audio/dsd/](../../src-tauri/src/audio/dsd/)) wrapped as a `waveflow-core` function.

**No HLS / DASH for v1.** Music files are small enough that progressive download with Range works fine. Reconsider if Phase 4 mobile shows bandwidth issues.

### 6.8 Service discovery

**Decision: zero-conf mDNS** advertising `_waveflow._tcp.local` with a TXT record carrying server name + version + TLS flag. Manual URL entry as fallback.

Desktop client scans on the Settings → "Server mode" screen. Selecting a discovered server pre-fills the URL field.

## 7. Phase 1 delivery plan

Seven sub-phases, each shippable independently. The desktop app continues to receive normal feature work in parallel.

| Phase | Scope | Repo(s) touched | Visible to user? |
|---|---|---|---|
| **1.a** | Extract `waveflow-core` crate as workspace member. Move scanner, metadata, Deezer, Last.fm, audio-analysis, DSD converter. Define repository traits. | `waveflow` | No (zero behavior change) |
| **1.b** | `waveflow-server` skeleton: axum + Postgres + CRUD for profile/library/track/playlist. No auth yet (dev-only `X-User-Id` header). OpenAPI spec emitted. | `waveflow-server` | Self-host beta |
| **1.c** | `waveflow-web` skeleton: TanStack Start + Better Auth. Login page, empty dashboard, server URL config. JWKS endpoint live. | `waveflow-web` | Login works, no data yet |
| **1.d** | Wire Better Auth JWKS → axum verification. Replace `X-User-Id` with real auth. Refresh-token flow tested. | `waveflow-server`, `waveflow-web` | End-to-end auth on web |
| **1.e** | HTTP range streaming + transcode cache. Browser player in `waveflow-web` (Web Audio API, gapless via two `<audio>` elements). | `waveflow-server`, `waveflow-web` | Web playback works |
| **1.f** | Sync ops protocol + WebSocket fan-out. Desktop integration: Settings → "Server mode" toggle, repository swap at runtime. | All three repos | Multi-device sync |
| **1.g** | Public playlist pages (SSG with ISR fallback), OG image generation, sitemap, robots.txt. | `waveflow-web` | Shareable links |

Estimated cadence: ~3 weeks per sub-phase = ~5 months for Phase 1.

## 8. Open questions (deliberately deferred)

These are flagged and will be resolved at the relevant sub-phase, not now:

| Question | Defer to | Why deferred |
|---|---|---|
| Should EQ presets sync across devices? | Phase 2 | Need user feedback on whether per-device EQ (different headphones) is the common case. |
| How to handle the same file existing on two devices with different `file_hash`? | 1.f | Need a real dataset to design the dedup rule. Likely: prefer device-local file over server stream when hashes match. |
| Does the server scan its own library, or does it receive scans from desktop clients? | 1.b | Both have merit. Server-side scan = single source of truth. Client-pushed = lighter ops on the server. May support both. |
| Self-update mechanism for `waveflow-server`. | Post-1.g | Out of Phase 1 scope. Likely a `waveflow-server self-update` subcommand. |
| Rate-limiting / abuse prevention on the public web surface. | 1.g | Need real traffic shape to size limits. |

## 9. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `waveflow-core` extraction reveals tight coupling that requires major refactor | Medium | High | Treat 1.a as research-grade work. If it explodes, scope-creep is acceptable — everything downstream depends on this being clean. |
| TanStack Start matures slower than expected, blocks 1.c | Low | Medium | Fallback to Next.js Pages Router (no RSC). Decision deadline: start of 1.c. |
| Better Auth JWKS rotation breaks desktop clients with cached keys | Medium | Medium | Cache TTL = 1h, keys rotate quarterly, overlap window = 1 week. Document the rotation runbook. |
| Postgres feels heavy for a single-family self-host | Medium | Low | Document that SQLite is supported on the server too via `--features sqlite-server` build flag (single-writer, but fine for ≤ 5 concurrent users). |
| Sync ops table grows unboundedly | High | Low | Background compaction job (Phase 1.f): reads `MIN(last_seen_id)` from `device_sync_cursor` across all of a user's devices and only collapses ops with `id < min` for the same `(entity_id, field)`. Run nightly. Devices that haven't synced in N days (configurable, default 90) are excluded from the `MIN` so a single stale device can't block compaction forever. **Atomicity invariant**: the delete/collapse and the `sync_compaction_watermark.compacted_up_to` update **must run inside the same Postgres transaction**. A crash window between the two would leave the resurrected-device guard reading a stale (lower) watermark, exposing the Pull handler to false negatives — it would happily stream ops that no longer fully exist. |
| Resurrected device returns past the staleness cutoff and silently misses compacted ops | Medium | Medium | Pull handler checks `sync_compaction_watermark` first; if `since > 0 AND since < compacted_up_to` it returns `410 Gone` with `compacted_up_to`, forcing a client full-resync from `since=0`. The `since > 0` half of the predicate matches the normative rule in §6.6 and is what makes the recovery loop terminate (a `since=0` request bypasses the watermark check). Without this guard the client would apply a truncated delta and drift permanently out of sync. |

## 10. Alternatives considered

| Alternative | Why rejected |
|---|---|
| Server in Bun + Hono | Would jettison the existing Rust scanner / metadata / Deezer / Last.fm code. Marginal DX gain not worth the rewrite. |
| Web in Nuxt | Vue. Abandons the React ecosystem and forces re-implementing i18n + theme system. |
| Web in Next.js | App Router forces RSC (complexity tax) + Vercel-isms. Pages Router is viable but feels like a step back. |
| Single React codebase for web and desktop | Web cases of use diverge enough that "sharing" would mean lowest-common-denominator design. Confirmed in §6.1. |
| Monorepo with all three projects | Rust + TS tooling friction without enough shared code. Three repos is cleaner. |
| Full CRDTs (Yjs / Automerge) | Overkill for the entity types we sync. LWW + tombstones is sufficient. |
| Session cookies + axum-managed auth | Doesn't work for native clients (desktop, mobile) outside a browser context. |
| MongoDB / DynamoDB for the server DB | Relational model fits the data; existing `sqlx` knowledge transfers; Postgres `LISTEN/NOTIFY` gives WebSocket fan-out for free. |

## 11. Glossary

- **BFF (Backend-For-Frontend)**: pattern where a frontend has its own thin server layer that proxies and adapts a heavier upstream API.
- **JWKS (JSON Web Key Set)**: standard endpoint at `/.well-known/jwks.json` exposing public keys for JWT signature verification.
- **Lamport clock**: a monotonic counter incremented on each event; used for distributed event ordering without wall-clock sync.
- **LWW (Last-Write-Wins)**: conflict resolution rule where the op with the latest timestamp overwrites earlier ones.
- **RSC (React Server Components)**: React 19 feature where components execute on the server and ship serialized output to the client.
- **SSG / SSR / ISR**: Static Site Generation / Server-Side Rendering / Incremental Static Regeneration — rendering strategies.

---

**Next step after acceptance**: open the GitHub project board for Phase 1, then start work on 1.a (`waveflow-core` extraction) in the existing `waveflow` repo.
