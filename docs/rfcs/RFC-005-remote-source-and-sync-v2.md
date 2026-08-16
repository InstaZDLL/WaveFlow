# RFC-005 — Remote music source and user-data sync v2

- **Status**: Accepted
- **Date**: 2026-08-10
- **Authors**: @InstaZDLL
- **Supersedes**: [RFC-003](RFC-003-sync-architecture.md) (desktop) — hybrid logical clocks, per-entity CRDT arbitration and digest reconciliation are all dropped, see [Why the v1 design retires](#why-the-v1-design-retires).
- **Server-side counterpart**: `waveflow-server` `docs/rfcs/RFC-003-waveflow-sync-v2.md` (accepted 2026-08-09) — **a different document with the same number**, see [the naming trap](#the-rfc-003-naming-trap).
- **Implementation**: `crate::remote` behind the `sync_v2` Cargo feature.

---

## The situation this RFC answers

The desktop's synchronization layer talks to a server protocol that no longer
exists. All six routes it consumes have zero occurrences in the server's current
source, and its sign-in flow depends on a web front-end that was removed. The
server is now authoritative over an ordered journal; the desktop was written
against a peer-to-peer model where clients arbitrated concurrent writes among
themselves.

This is not an adaptation. It is a replacement of the protocol, and — more
consequentially — a change in **what synchronization means for the user**.

## Decision 1 — the remote catalogue is a separate source, never merged

The single most important consequence, and the one that reshapes the UI:

> Synchronized state describes the **server's** playlists, favourites, ratings,
> history, queue and shares. Those reference the **server's** tracks. A server
> track has no local counterpart, and this protocol never invents one.

So the incoming projection cannot be written into `playlist`, `liked_track` or
`track.rating`. Doing so would either fabricate local tracks for rows that only
exist on the server, or silently drop every entry — the first corrupts the local
library, the second makes sync look broken. The projection therefore lands in
its **own tables** (`remote_*`), is presented as a distinct source in the
sidebar, and is reconstructible: dropping it and re-fetching a snapshot is
always a valid recovery.

Matching a local file to a server track is **out of scope** and needs its own
RFC. When it comes, the only automatic link allowed is an exact, unique
content-hash match; a MusicBrainz identifier is a suggestion the user confirms;
matching by title/artist/duration is explicitly forbidden.

**What this costs.** Local playlists no longer travel between machines. That
capability existed in the v1 design and is genuinely lost. It cannot be
recovered without the matching layer above, because a local playlist is a list
of local files and nothing in the protocol can name those on another install.

## Decision 2 — two seams, so sync stays a capability

The desktop must be able to connect to any server speaking the Subsonic
protocol, not only to WaveFlow. That requirement drives the shape: one
mandatory interface for what every server does, one optional interface for what
only WaveFlow offers.

```text
   MusicServer (mandatory)              SyncProvider (optional)
   catalogue, search, playback,         snapshot, changes, ack, socket
   user-data per capability
          │                                      │
   ┌──────┴────────┐                             │
SubsonicSource   WaveflowSource ─────────────────┘
  /rest/*          /api/v2/*  (native end to end)
```

Between WaveFlow Desktop and WaveFlow Server we go through `/api/v2`
**always** — catalogue and playback included, not just synchronization. Routing
our own traffic through the compatibility façade would forfeit three things we
already have: mutation idempotency (only the v2 routes read the operation-id
header), full-text search (the façade still filters in memory), and native
pagination with typed projections.

`WaveflowSource` is therefore an independent implementation, not a
`SubsonicSource` with sync bolted on.

**Detection.** A Subsonic `ping` against WaveFlow answers `type="waveflow"`.
That field — not the extension list — decides whether `SyncProvider` is
available.

> **Verified trap.** `getOpenSubsonicExtensions` returns an *empty* container
> today. A client that probed capabilities that way would conclude the server
> offers nothing, while it in fact offers the entire v2 API.

## Decision 3 — remote identity is polymorphic

```rust
enum RemoteIdentity {
    Waveflow { account_id: Uuid, device_id: Uuid, cursor: i64 },
    Subsonic { username: String },
}
```

A third-party server has no account UUID, no device notion and no cursor.
Putting those three fields in a shared struct would make them optional
everywhere and spread `unwrap` over cases that cannot occur.

One desktop profile binds to one server account. A profile stays the local unit
of identity and session; a library is a content resource, so an account exposing
several libraries selects one (`active_library_id`) rather than spawning
artificial profiles.

## Decision 4 — remote identifiers are opaque strings

WaveFlow serializes its UUIDs, which makes its two surfaces interchangeable
without a translation table. Other servers emit textual identifiers of another
shape. So: never parse a remote identifier into a `Uuid`, index on the composite
key `(profile_id, remote_id)` — two servers can legitimately emit the same
string — and keep the catalogue cache separate from synchronized state, since
one is reconstructible and the other is not.

## Decision 5 — Authorization Code + PKCE on loopback

The desktop is a public client. It opens `<server>/authorize` in the system
browser with `client_id`, `redirect_uri`, `code_challenge` (S256), `state` and
`device_name`; the consent screen posts them back with the browser session
attached and follows the redirect the server computes. The loopback listener
then exchanges `code` + `code_verifier` at `/api/v2/oauth/token`.

The loopback listener and the random generator already exist for another
provider; only the protocol changes.

Three details, all measured against a live server rather than inferred:

> **A code is spent on first presentation**, whatever the outcome. Redeeming
> with a wrong verifier burns it — presenting the *correct* verifier afterwards
> still answers 401. Retrying a code is not a recovery path; the flow restarts
> from the beginning.

> **The redirect URI is compared as a string at redemption.** Shape validation
> ignores the port, as RFC 8252 §7.3 asks, but the token endpoint compares the
> grant's URI with the presented one byte for byte — changing only the port
> answers 401. Binding the loopback listener *first* and building the URI once
> from the port obtained makes the two identical by construction.

> **The refresh token rotates and the device survives it.** A refresh returns a
> new refresh token, the old one answers 401, and `device_id` is echoed back
> unchanged. So a rotation never invalidates queued mutations, and the
> device-adoption branch in the client is defensive, not routine.

Worth recording as an observation rather than a decision: `/oauth/authorize`
authenticates with a **Bearer header**, not a browser cookie, so the consent
step is technically reachable without a browser at all. We still go through the
browser — it is what keeps the account password out of the desktop process —
but that is a deliberate choice, not a constraint the server imposes.

A third-party server authenticates by username/password, token/salt or API key
instead. That is a second authentication shape to carry, not a degraded first.

## Decision 6 — playback carries a Bearer header

`GET /api/v2/tracks/{id}/stream` with `Authorization: Bearer`, accepting
`format`, `bitrate` and `offset_ms`, answering 206/416 on ranges. Sealed tickets
exist for consumers that cannot set a header — a browser `<audio src>` — which
is not our case. `MusicServer` therefore exposes a stream URL *and the header to
attach*, never a hard-coded route.

## Decision 7 — the queue holds replayable business mutations

Offline writing stays. What changes is the payload: no longer generic ops but
typed, replayable REST calls, each carrying a stable `operation_id`.

```rust
enum Mutation {
    SetFavorite { operation_id: Uuid, entity: RemoteEntity, id: String, starred: bool },
    SetRating   { operation_id: Uuid, entity: RemoteEntity, id: String, rating: u8 },
    UpdatePlaylist { operation_id: Uuid, playlist_id: String, /* … */ },
}
```

Three server properties constrain this queue, and ignoring them fails in
perfectly ordinary situations:

- **A queued entry is immutable.** The server stores a canonical fingerprint of
  the action, target and normalized payload. Reusing an `operation_id` with a
  different fingerprint is *rejected as a conflict*, not treated as a replay. So
  if the user corrects a gesture before the queue drains — renaming an
  already-queued playlist — we emit a second mutation with a **new**
  `operation_id`, or merge the two before enqueueing and regenerate the
  identifier. Mutating a pending entry in place breaks.

- **Replaying a share creation returns the same URL.** The share token is
  derived deterministically from the identifier, so share creation replays like
  any other mutation, with no special casing and no second URL.

- **The ACK is not a prerequisite.** It feeds observability and future
  retention, not read authorization. A failing ACK must never block
  synchronization.

**Permanent versus transient failure.** `422 validation_error` (malformed) and
`409 conflict` (an operation id replayed with a different payload) are both
permanent: a retry cannot change the outcome, so the entry is marked and
surfaced rather than spun on. The fixes differ — correct the request versus mint
a new operation id — which is why the server keeps them apart. Only 5xx, 429 and
transport errors are retried.

**`409` carries two opposite meanings, so branch on the code.** It is also the
status for `cursor_expired` on a read, and confusing the two is destructive in
both directions: an expired cursor treated as a conflict abandons a write that
would have succeeded, and a conflict treated as an expired cursor throws away a
healthy projection. The client therefore keeps the server's `code` as a
structured field and never decides from the status alone.

**Order is load-bearing.** Strict FIFO, and the first retryable failure ends the
pass so nothing overtakes what it depends on. A permanent failure is the
exception: it can never succeed, so that entry is marked and the drain moves on
rather than deadlocking every later change behind it.

**Creating something the server has not named yet.** A playlist created offline
has no server identifier, so it gets a local placeholder — `local:<uuid>` —
which the projection uses as a real key, making it visible and editable
immediately. When the creation lands, the placeholder is resolved in three
places: the projection row, its ordered tracks, and any mutation still queued
behind it.

Two consequences that are easy to get wrong:

- The row is **copied, re-pointed, then dropped**, never renamed in place. A
  primary key cannot be updated out from under the rows referencing it —
  renaming first orphans the tracks, re-pointing first names a parent that does
  not exist. Either order violates the foreign key.
- A snapshot **must not delete a placeholder playlist**. The server has never
  heard of it, so its snapshot cannot be evidence that it was deleted; wiping it
  would destroy a playlist the user made offline while its creation sat queued.

Rewriting those queued payloads is the single exception to entry immutability,
and it is sound for a specific reason: FIFO guarantees an entry behind an
unlanded creation has never been presented to the server, so no fingerprint
exists for it to conflict with.

**Emptying a field is its own verb.** An update coalesces, so an absent field
and an explicit null are indistinguishable — "leave it" and "empty it" would
otherwise be the same request. Clearing is expressed by naming the field:
`clear: ["comment"]` on a playlist, `["description", "expires_at"]` on a share.
Two consequences the client is built around:

- **An unrecognized name is refused, not ignored.** Asking to clear `expiresAt`
  answers `422` rather than reporting success while nothing moved. That makes
  those field names load-bearing, so they are spelled in exactly one place
  instead of inline at each call site.
- **The clear belongs to the operation fingerprint.** Setting an expiry and
  removing it are two distinct mutations and cannot share a replay identifier —
  which the queue satisfies for free, since every enqueue draws a fresh one.

**A share's URL has exactly one moment.** The journal never carries it, and it
cannot be derived locally — the token is keyed on a server-side instance secret.
So the creation response is the only time this device can learn the link, and a
share created on another device stays link-less here permanently. Capturing it
at creation is not an optimisation; it is the only chance.

## Decision 8 — v2 landed beside v1, then replaced it

The v1 surface (~12 400 lines across `sync/`, `commands/sync.rs`,
`server_client.rs`, `commands/share.rs`) sat behind the `sync_v1` feature,
which was **not in `default`** — already absent from shipped binaries, with
`sync_stub.rs` keeping the ~70 CRUD emit call sites compiling.

v2 landed as a new tree, `crate::remote`, behind a new `sync_v2` feature (also
off by default), built *beside* v1 rather than on top so v1 stayed available as
the only recovery path while the snapshot bootstrap was unproven.

**Cutover (2026-08-15).** Once the bootstrap was validated end-to-end against a
live server — identify, PKCE sign-in, snapshot pull, optimistic create + drain,
and offline replay all confirmed by hand — the v1 tree was deleted: `sync/`,
`commands/{sync,server_auth,share}.rs`, `server_client.rs`, and the `sync_v1`
feature itself are gone, along with the `lamport`, `hlc`, `digest/` and
`backfill/` machinery. Two things survive the delete:

- the ~70 emit call sites are **untouched** — `mod sync` is now permanently
  `sync_stub.rs`, whose no-ops keep them compiling; local-entity CRUD is no
  longer synchronized, which is correct under v2;
- `save_share_image` — a generic PNG sink used by Wrapped and the Now-Playing
  card, mis-filed in the old `commands/share.rs` — moved to
  `commands/share_image.rs` and is no longer feature-gated (it had been silently
  absent from shipped builds while it rode the `sync_v1` gate).

The migrations that added the HLC columns stay — they are immutable once merged;
the columns are now unused but harmless.

## Decision 9 — remote playback is a parallel in-memory queue, not the local one

A remote playlist plays natively — start it and its tracks auto-advance, and
next / previous from the PlayerBar, the media keys, the tray and MPD drive it —
but its queue does **not** live in the local `queue_item` table.

`queue_item` rows are library row ids, joined to `track` / `album` / `artist` /
`artwork` to build [`QueueTrack`](../../src-tauri/crates/app/src/queue.rs), which
the local player, MPD and the PlayerBar payload all consume. A projected remote
track has no such row (Decision 1 keeps the projection out of local tables), so
teaching `QueueTrack` about id-less entries would ripple through every one of
those consumers. Instead the remote queue is a **parallel structure** —
[`remote_playback.rs`](../../src-tauri/crates/app/src/remote_playback.rs), an
ordered list of server track ids plus display metadata, held in memory like the
radio session and rebuilt from the projection each time a playlist starts. No
migration, no change to the local queue path.

Each track streams over the same single-URL path Web Radio uses
(`LoadUrlAndPlay`, negative sentinel id) via a **sealed stream ticket** — the
desktop mints `POST /api/v2/tracks/{id}/stream-ticket`, prepends its **own**
trusted `base_url` to the deliberately-relative URL the server returns (rejecting
any absolute or protocol-relative value, which would redirect playback to an
unauthenticated host), and hands the result to the engine. This is the ticket
consumer Decision 6 anticipated: the cpal HTTP source cannot attach a Bearer
header to the audio stream, so the credential rides inside the URL instead.

The seams that make advance work:

- a **finite** stream reaching EOF carries a negative id (radio is infinite and
  never reaches EOF on its own), which the decoder routes to a new
  `AnalyticsMsg::RemoteTrackEnded`; analytics advances the remote queue on it,
  but **only when one is active** — a dropped radio connection lands here too and
  is ignored;
- `player_next` / `player_previous` and `player_actions` check the session first
  and hand off to `remote::playback::advance`;
- the session is cleared at a single choke point — `emit_track_changed`, which
  every local-track start funnels through — plus `player_play_url` for radio, so
  a stale remote queue can never hijack the next advance.

The orchestration (fill from projection, mint ticket, dispatch) lives in the
`sync_v2`-gated `remote::playback`; the state and its clear/probe live in the
always-compiled `remote_playback` so the control seams stay feature-clean.

**UI.** The remote source is managed from the main UI, not Settings: a "Remote
source" section at the bottom of the sidebar (headed by the server host, listing
its playlists) and a `RemotePlaylistView` that plays, renames, deletes, removes
tracks from and reorders them like local playlists. Track edits go through the
server's `UpdatePlaylist` mutation: removal queues `remove_indexes`; reorder, for
which the mutation has no move, queues a full replace (`remove_indexes` for every
position + `add` in the new order). `RemoteServerCard` in Settings is connection-only —
identify, sign in, sync, sign out, forget. `CreatePlaylistModal` offers an "also
create on the server" checkbox when one is connected. All of it self-hides when
`sync_v2` is off (the frontend probes `remote_get_status`) and is intentionally
unlocalized until the feature ships, matching `RemoteServerCard`.

**The queue panel** switches to a dedicated `RemoteQueueView` while a remote
session plays (keyed on `isRemoteTrack`), reading `remote_get_play_queue` (an
in-memory snapshot) and jumping with `remote_queue_jump` — the local
`player_get_queue` / jump / reorder path acts on the library queue and would be
wrong here. **The seekbar** is bounded and scrubbable: the decoder stamps the current
entry's duration onto the radio-metadata event so the bar fills to a real total,
and `HttpMediaSource::open_seekable` reads `Content-Length` + `Accept-Ranges` to
advertise `is_seekable()`, so a drag drives `format.seek`, which reissues a
ranged GET. Radio stays forward-only (opened with ICY, never seekable).

Deferred: adding tracks (needs a remote catalogue picker).

## Local schema

Per-profile, since the binding is per-profile:

| Table | Role |
| --- | --- |
| `remote_binding` | server flavour, `account_id`, `device_id`, cursor, `active_library_id` |
| `remote_playlist` + `remote_playlist_track` | projected playlists, ordered |
| `remote_favorite` | `(entity_type, entity_id)` |
| `remote_rating` | `(entity_type, entity_id, rating)` |
| `remote_history` | scrobbles, append-only |
| `remote_queue` | single row, the server's saved queue |
| `remote_share` | non-secret share fields; **never** token from the journal |
| `remote_track` | cached song metadata, derived and droppable |
| `remote_mutation` | outbound queue, typed, keyed by `operation_id` |

`remote_track` exists because the two feeds are asymmetric: a snapshot
returns playlists and the queue with **whole song objects**, while a change
event carries a playlist upsert as bare `track_ids`. Without the cache, a
playlist edited after the bootstrap would arrive as identifiers with nothing to
render. Missing identifiers are fetched from `GET /api/v2/tracks/{id}` after a
pass — opportunistically, since ordering and identity are already stored and a
failed fetch costs a placeholder row rather than a wrong one.

Tokens keep using `auth_credential` under the existing `waveflow_server`
provider, which already carries a refresh token and an expiry.

## Target sequence

```text
LOGIN ──▶ account_id + device_id
   │
BOOTSTRAP ──▶ GET /sync/snapshot ──▶ atomic replace ──▶ cursor ──▶ ACK
   │
SYNC ──▶ GET /sync/changes?after=cursor ──▶ apply ──▶ cursor ──▶ ACK
   │
WS {"cursor": N} ──▶ if N > local cursor ──▶ GET /sync/changes?after=cursor
```

Upstream: optimistic local write, enqueue, then the business endpoint with
`X-WaveFlow-Operation-Id` and `X-WaveFlow-Device-Id`. The server applies or
recognizes the replay; the change comes back through `/changes`.

Unknown entity types, actions and payload fields are ignored. A client that
cannot apply a **known** event drops its projection and re-fetches a snapshot.

### An absent key means unchanged, not null

The single rule the apply path is built around, because getting it wrong loses
user data silently. Journal payloads are not uniform — measured against a live
server:

```text
cursor=1  playlist/upsert   payload keys = [id, name, track_ids]
cursor=2  playlist/upsert   payload keys = [comment, id, name, public, track_ids]
```

The first is a creation, the second an update. A share update likewise omits
`track_ids`, because updating a share cannot change them. So an apply that
decoded the payload into a struct of `Option` fields and wrote all of them
would blank a playlist's comment every time a create-shaped event is replayed,
and empty a share on every description edit.

Each field is therefore written only when its key is **present**; a key present
and null is a genuine clear. The projection's tests replay the server's own
captured journal and assert it converges on the server's own snapshot — the two
feeds have to agree, or the result would depend on which one happened to run.

## Why the v1 design retires

Every mechanism below exists because v1 accepted concurrent writes arbitrated
client-side. The v2 journal is strictly ordered and server-authoritative;
keeping them would carry complexity the protocol no longer asks for.

- **Logical clocks** (`lamport`, `hlc`) — the journal cursor is the global order.
- **Digest reconciliation** — a snapshot the client swaps in atomically replaces
  fingerprint comparison.
- **Last-writer-wins backfill** — a conflict is no longer arbitrated locally: a
  reused operation identifier with a different fingerprint is rejected, and a
  client that cannot apply a known event takes a fresh snapshot.
- **Compaction and `410 Gone`** — replaced, not dropped. The v2.0 journal is
  append-only, so no cursor is too old *yet*: a cursor beyond the last event
  returns an empty page rather than an error. But the contract for when
  compaction lands is now defined — `/sync/changes` answers `409` with
  `code: "cursor_expired"` for a cursor below the oldest retained event — and
  the client implements the recovery against it. Two triggers therefore lead to
  the same place: a known event that fails to apply, and an expired cursor.
  Both discard the projection and take a fresh snapshot.

## The RFC-003 naming trap

The desktop's own [RFC-003](RFC-003-sync-architecture.md) (hybrid logical
clocks, Draft, 2026-06-12) has nothing to do with the server's RFC-003 (sync v2,
Accepted, 2026-08-09). Comments in `sync/mod.rs` point at the **desktop** one.
Any instruction mentioning "RFC-003" must name the repository, or it will be
read as the wrong document. This RFC exists partly to end that ambiguity: on the
desktop side, the accepted design is **RFC-005**.

## A note for whoever designs the matching layer

Still out of scope, but one constraint is now known and worth recording before
someone plans around an assumption that does not hold.

The server publishes `full_hash` on its tracks: BLAKE3, non-keyed, hexadecimal,
over the **whole file**, pinned as part of its contract. The projection captures
it — unread — because it arrives free with every snapshot, and the alternative
is re-downloading the catalogue on the day matching is designed.

**It is not comparable to the local `track.file_hash`.** The names invite the
assumption and the assumption is wrong. `scanner::extract::hash_file` computes

```text
blake3( file_length_le_bytes || head_1MiB || tail_1MiB )
```

for anything above 2 MiB, and `blake3(length || whole_content)` below it. The
length prefix alone means it can never equal a plain full-file digest, at any
size. That partial form is deliberate: full hashing was the scanner's dominant
cost, reading roughly 9 GB to scan 900 tracks.

The comparable local value is `scanner::extract::hash_file_full` — a plain
whole-file BLAKE3 in hex, therefore directly equal to `full_hash` on identical
bytes. It exists today only to confirm duplicates before a destructive delete,
and is stored nowhere.

So matching on content means reading local files in full, which is exactly the
cost the scan avoids. A cheap pre-filter exists: both sides already expose
`size`, so only the handful of candidates that agree on length ever need to be
read. That turns "hash the whole library" into "hash the few files that could
possibly match".

The scope rule is unchanged: automatic linking on a **unique** exact match only,
a MusicBrainz identifier as a suggestion the user confirms, and never a fuzzy
match on title, artist or duration. Note also what the fingerprint is — the
file, not the decoded audio — so two copies of the same recording with different
tags will not meet.

## Scrobbles and the queue wait on remote playback

Their mutations, commands and projection writes all exist, and nothing
drives them from the player — deliberately.

The local queue holds file paths and local row ids, and the scrobbler joins the
local `track` table. Those identifiers mean nothing to the server, which
validates them: every such mutation would come back `404`, be marked
permanently failed, and accumulate in the queue as garbage the user would then
have to be told about. Wiring them today would not be an incomplete feature, it
would be a defect.

The dependency is remote playback (§Decision 6 defines the transport; nothing
implements it). Until then the commands are the surface for a caller that
already holds genuine remote identifiers.

## Out of scope

- Matching local files to server tracks (own RFC; the note above is input to it,
  not a design).
- Full third-party Subsonic implementation: the seam is defined here and
  `WaveflowSource` implements it; the other branch is its own slice.
- Retention and compaction of the server journal.
