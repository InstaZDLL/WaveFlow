# RFC-003 — Sync architecture v2

- **Status**: Draft
- **Date**: 2026-06-12
- **Authors**: @InstaZDLL
- **Supersedes**: RFC-001 §Phase 1.f sync (the practical parts — apply pipeline + ops log stay; semantics and protocol are redesigned).
- **Depends on**: [RFC-001](RFC-001-waveflow-server.md) — reuses its apply pipeline, JWT boundary, and entity dispatch shape.
- **Touches**: desktop ([`InstaZDLL/WaveFlow`](https://github.com/InstaZDLL/WaveFlow)), monorepo ([`InstaZDLL/waveflow-server`](https://github.com/InstaZDLL/waveflow-server), root + `web/`).
- **Implementation tracking**: to be opened as a GitHub project once Phase A lands.

---

## Why this RFC exists

Visual QA against the dev stack (2026-06-11) caught that **the sync only emits ops on changes**, never on existing data. A device that signs in with an already-populated library never reaches a synced state: the server stays at zero profiles forever, the WebSocket fans out an empty stream, and the user has no signal that anything is wrong.

This is one of several gaps. Putting them in one place:

1. **No backfill on first sign-in.** Library scanned before sign-in → ops never enqueued → server never sees the data.
2. **No `profile` entity in the sync wire shape.** Profiles are auto-provisioned server-side when the first profile-scoped child op (library / playlist / track) arrives — but if no child op ever fires, no profile materialises, so the web shows "No profiles yet" indefinitely.
3. **Lamport scope is wrong.** The current per-`(user, device)` monotonic counter makes inter-device ordering ambiguous: device A's `lamport_ts = 5` and device B's `lamport_ts = 5` are not directly comparable, but the apply pipeline treats them as equally recent. Combined with the in-memory `SyncHub::broadcast` (no per-pair causality tracking), concurrent edits race on apply order.
4. **Last-writer-wins for everything.** The apply pipeline blanket-overwrites every scalar column on conflict. Two devices renaming a profile at the same wall-clock time can flap the name back and forth on every reconnect.
5. **Playlist track ordering is fragile.** `playlist_track.position` is a stable integer that's reset wholesale on `set tracks`; concurrent inserts at the same position produce a position collision that the server resolves by tiebreaker on `track_id`. The desktop and the web then disagree on the visible ordering until the next refresh.
6. **No conflict resolution UI.** Today every conflict is silent. There's no surface telling the user a remote op overwrote their local edit.
7. **Folder removal isn't a cascade.** Per the current contract, when the desktop removes a library folder, it has to per-row emit deletes for every track because the server has no `library_folder` entity. Library removal IS a cascade. The mismatch is implicit; a peer device that signs in mid-deletion sees a half-deleted state.
8. **No catchup on first connect.** A device that's been offline for a month signs back in and pulls the entire `sync_op` log via `GET /api/v1/sync/ops?since=<cursor>`. That's fine on a fresh log but bills proportional to history once an active install has 100k+ ops.

The fixes form one coherent change rather than five isolated patches. This RFC describes that change.

## Scope

In scope:

- **Backfill.** First-sign-in protocol that materialises every existing local row on the server.
- **Lamport revisit.** A causal ordering scheme that's correct across N devices without requiring a global clock.
- **Per-entity conflict resolution.** Designed per-CRUD-shape (LWW for scalar names, OR-Set for collection membership, Fractional Index for tree positions).
- **Profile + library_folder as first-class sync entities.** No more implicit auto-provisioning.
- **Status UI on the desktop.** Pending ops count, last sync, last error, "Sync now", "Re-sync everything" buttons.
- **Catchup compression.** Server-side digest the device can compare against without pulling N ops.

Out of scope:

- **Conflict resolution UI surfaced to the user.** When LWW or OR-Set rules give a clean answer, ship it silently. Only the truly ambiguous cases (concurrent rename + delete on the same playlist) deserve a UI prompt — and we deliberately defer that to RFC-003.1 once we have field data on how often it happens.
- **Streaming / artwork / share.** Those don't go through the sync pipeline.
- **Plugin-source data (Web Radio favourites, etc.).** Plugins manage their own storage today; sync hooks into the plugin runtime is a separate question for RFC-002.x.
- **Track metadata sync (tag editor → server → other desktops).** Already out of scope per RFC-001 §1.f, stays that way.

## Current state — what stays, what goes

```text
✓ STAYS                                  ✗ GETS REPLACED
─────────────────────────                ─────────────────
sync_op log (append-only)                Per-(user,device) Lamport
WebSocket fan-out                        Auto-provision of `profile`
JWT auth boundary                        Implicit folder-delete cascade
apply pipeline shape (entity dispatch)   Per-entity blanket LWW
playlist_track snapshot fields           `set tracks` wholesale reorder
device_sync_cursor                       Catchup-by-pull-everything
```

The wire shape evolves; the storage layout is largely preserved.

## Design

### 1. Entity model

Every syncable thing carries:

- `canonical_id: UUID v7` (instead of v4: time-ordered, sortable, doubles as a tiebreaker).
- `created_at: i64` — wall-clock millis since Unix epoch (UTC), originating device's read of its system clock. Used for human-facing display ("created 2 days ago"); never for ordering or conflict resolution. Ordering uses `hlc` (see §2).
- `updated_at: i64` — same shape as `created_at`, refreshed on every write that mutates the entity.
- `hlc: (wall: u64, logical: u32)` — the causal ordering primitive (see §2). Authoritative for conflict resolution; supersedes the legacy `lamport_ts: u64` (kept during Phase A for back-compat, dropped in Phase D).
- `origin_device_id: UUID` (which device originated this op; the lex-compare tiebreaker for equal `hlc` values, and needed for OR-Set conflict resolution).

Six top-level entities:

| Entity | Scope | Conflict model | Notes |
| --- | --- | --- | --- |
| `profile` | user | LWW on scalar fields | name, color, settings JSON. Sticky `is_active` flag is local-only. |
| `library` | profile | LWW on scalar fields, OR-Set on `folders` | Adds `library_folder` as a sub-entity (no more cascade ambiguity). |
| `track` | library | LWW with `file_hash` tiebreaker | `file_path` keying preserved; tag-editor re-emit still uses path identity. |
| `playlist` | profile | LWW name, Fractional-Index `tracks` | Solves the position-collision problem. |
| `liked_track` | user | OR-Set on `(user_id, file_hash)` | Already commutative today; we just make it explicit. |
| `track_rating` | user | LWW on `(user_id, file_hash) → 1-10` | Half-step UI preserved. |

`library_folder` is a sub-entity of `library`, not a top-level. It carries its own `canonical_id` so insert / delete are commutative.

### 2. Lamport ordering

The current per-`(user, device)` counter is replaced with a **hybrid logical clock (HLC)** per device:

```text
hlc = (wall_clock_millis, logical_counter)
```

On every event:

```python
local_wall = now_millis()
if local_wall > last_hlc.wall:
    new_hlc = (local_wall, 0)
else:
    new_hlc = (last_hlc.wall, last_hlc.logical + 1)
```

On every received remote op (push or WS):

```python
remote = op.hlc
local_wall = now_millis()
new_wall = max(remote.wall, last_hlc.wall, local_wall)
if new_wall == remote.wall == last_hlc.wall:
    new_logical = max(remote.logical, last_hlc.logical) + 1
elif new_wall == remote.wall:
    new_logical = remote.logical + 1
elif new_wall == last_hlc.wall:
    new_logical = last_hlc.logical + 1
else:
    new_logical = 0
```

Why HLC and not pure Lamport: pure Lamport drifts arbitrarily far from wall clock, which makes "show me the last 30 days" client queries non-trivial. HLC stays bounded near wall time while still preserving the causality property (`a → b ⇒ hlc(a) < hlc(b)`).

The (`hlc`, `origin_device_id`) pair is a **total order** without needing a central authority: two ops with identical `hlc` tiebreak on `origin_device_id` (lex-compared).

Server stores `hlc_wall: BIGINT`, `hlc_logical: INT`, `origin_device_id: UUID`. The existing `lamport_ts` column is retired (kept as a view for legacy queries during transition).

### 3. Conflict resolution per entity

#### LWW on scalar fields

Profile name, library name, playlist name, track rating: each gets its own (`hlc`, `origin_device_id`) version vector. On conflict (two writes with overlapping hlc range), pick the (`hlc`, `origin_device_id`) max — same total-order rule as §2.

Caveat: this drops the loser's write. For names this is acceptable (the user can re-edit). For irrecoverable data we'd need MVCC; we don't have irrecoverable scalar data in scope.

#### OR-Set on collection membership

`library_folder`, `liked_track`: `(canonical_id, add/remove)` pairs.

Insert stamps `{canonical_id, add_at: (hlc, origin_device_id)}`. Delete stamps `{canonical_id, delete_at: (hlc, origin_device_id)}`. Membership uses the **same total order** as §2: a row is "in the set" iff `add_at > delete_at` under lex-compare on the full `(hlc.wall, hlc.logical, origin_device_id)` tuple — never a bare `hlc` comparison. This matters because two replicas would otherwise converge to different verdicts when an add and a delete share the exact same `hlc` but came from different devices: the §2 total order makes both replicas agree on which device wins. Add-bias on the boundary: `add_at >= delete_at` ⇒ present (concurrent add wins, classic OR-Set semantics).

Why add-bias: a user re-adding a folder after a remote delete is the common case; remote delete after local re-add is the rare case. Bias matches user intent.

#### Fractional Index for tree position

`playlist_track.position`: a `String` Fractional Index (FI), not an integer.

```text
[a]   pos = "5"
[a,b] insert b after a: pos = "55"   (lex-sorted between "5" and any "6+")
[a,c,b] insert c between a and b: pos = "53"
```

Two concurrent inserts at the same logical position produce different FI strings — no collision, deterministic ordering by lex-compare. Server stores `position TEXT` instead of `INT`; client renders by lex sort.

Why FI and not list-CRDT (RGA, etc.): FI is O(1) per op, no garbage, no tombstones, and the algorithm fits in 30 LOC. RGA is correct but heavy for the playlist size we ship (max ~5k tracks per playlist in the field).

### 4. Backfill protocol

On first sign-in OR on user "Re-sync everything":

```text
1. Desktop fetches GET /api/v1/sync/digest
   → server returns {entity → {
         count,
         set_hash,                  # MerkleHash of (canonical_id, payload_hash) pairs, sorted
         rows: [{canonical_id, payload_hash}, …]
       }} per profile
2. Desktop computes its local digest the same way and diffs row-by-row
3. For each canonical_id:
   3a. Server has it, desktop doesn't        → pull (catchup, see §5)
   3b. Desktop has it, server doesn't        → push as insert (backfill)
   3c. Both, same payload_hash               → no-op (verified identical state)
   3d. Both, different payload_hash          → conflict — apply the §3 rule for
                                                 the entity (LWW / OR-Set / FI),
                                                 the side with the lower
                                                 (hlc, origin_device_id) is the
                                                 one whose op gets pushed or
                                                 pulled to converge
4. Mark this profile as "backfilled" in profile_setting['sync.backfill_done']
   (per-profile, because the active profile's SQLite is the per-device scope
   that owns this marker; signing in to a different profile starts its own
   independent backfill against the same device + server)
```

The digest stays compact (`count + set_hash + per-row (canonical_id, payload_hash)` is ~48 bytes per row at BLAKE3-128). `payload_hash` is BLAKE3 over the entity's canonical wire form — every synced field plus `(hlc.wall, hlc.logical, origin_device_id)` — serialised in a deterministic shape (sorted JSON keys, lower-case hex for binary blobs) so identical row state on two replicas hashes identically regardless of platform endianness or JSON-encoding quirks. The set is sorted on `canonical_id` before the MerkleHash so a transposed pair doesn't flap the top-level hash.

Server-side caching is keyed on `(profile_id, entity, set_hash)`: the cache invalidates whenever the apply pipeline lands a row whose payload_hash differs from the cached value, not just when a new `canonical_id` enters or leaves the set — a rename of an existing playlist mutates `payload_hash` but leaves the set unchanged, and silently serving the stale cache would re-introduce exactly the divergence this digest is meant to catch. Implementation: the `apply::*` handlers recompute the row's `payload_hash` post-write and bump a `metadata_digest_version` counter per `(profile, entity)`; the digest endpoint reads the counter and rebuilds the cache lazily on miss.

**Streaming chunked push** for the backfill: desktop fans out `POST /api/v1/sync/ops` with batches of 200 ops at a time, throttled so the server's apply pipeline doesn't queue indefinitely.

The "Re-sync everything" button re-runs the same flow but ignores the `backfill_done` flag. A missing key in `profile_setting` is treated as "not backfilled", which also covers the legacy migration window: any installs that wrote an app-wide `app_setting['sync.backfill_done.<device_id>']` from an earlier draft get a fresh per-profile backfill on first boot of this version. The old app-wide key is deliberately not consulted on read so a backfill against profile A can't silently mask the need for one on profile B.

### 5. Catchup compression

When a device reconnects after a long offline window, instead of pulling every op since the cursor:

```text
1. Server runs `GET /api/v1/sync/ops?since=<cursor>` AS A DIGEST FIRST
   → returns {count, hash_of_canonical_id_set, max_hlc}
2. Device asks: "how many ops will it take to converge?"
3. If count > THRESHOLD (10k by default): device falls back to FULL digest exchange (§4)
   Otherwise: device pulls the ops list as before
```

Threshold is per-device (heuristic from connection speed) eventually; v1 ships a fixed 10k.

### 6. Status UI on the desktop

Settings → "WaveFlow server" card gains:

```text
[ ● Signed in as you@example.com ]

  Sync status:    All synced (2 minutes ago)
  Pending ops:    0
  Last error:     —

  [Sync now]                          [Re-sync everything…]

  Recent errors (last 24h):
    None
```

States:

- **All synced**: cursor matches server's `max_hlc`, no pending ops, last drain succeeded.
- **Syncing**: drain in flight or pending ops > 0.
- **Backfill in progress (X / Y)**: §4 protocol running.
- **Error**: drain failed within the retry window; surface the cause inline.
- **Offline**: `offline::is_offline()` true.

"Re-sync everything…" is gated by a confirm modal (heavy operation, will re-emit every local row). Includes an `ETA` estimate based on op count × per-op latency from the last successful backfill.

### 7. New entity: `library_folder` as first-class

Currently:

```text
library
  ↳ tracks (resolved scan-time from folder path)
```

After:

```text
library
  ↳ library_folder (canonical_id, path, OR-Set)
       ↳ tracks
```

Folder add/remove is now a normal sync op. Folder removal on device A propagates to device B without per-row track deletes; the server's apply pipeline cascades via the schema FK once the folder OR-Set delete wins.

Migration: existing libraries get a default `library_folder` per scan-root at the first boot after upgrade.

### 8. Profile as first-class

`profile + insert` becomes an explicit op fired on:

- Profile create (desktop UI).
- Sign-in if `app_setting['sync.profile_emit_done.<profile_id>']` is unset (catches profiles created pre-sync).

Drops the server-side auto-provisioning in `apply.rs::profile_resolve::find_or_provision` — replaced by a hard "fail if profile_canonical_id is unknown" (matches the current behaviour for `liked_track` / `track_rating`'s user_id resolution).

## Migration plan

The desktop has tens of installs in the wild; we can't break them. Phased:

### Phase A — wire shape additive (no behaviour change)

- Add `hlc_wall`, `hlc_logical`, `origin_device_id` columns to every sync entity (desktop SQLite + server Postgres).
- Backfill them from the existing `lamport_ts` (treat the legacy counter as the logical, set wall = epoch).
- Wire shape v2: ops carry both v1 `lamport_ts` and v2 `hlc`; server prefers v2 when present, falls back to v1.
- Server applies the existing LWW behaviour to both v1 and v2 ops — no semantic change.

### Phase B — backfill + status UI

- Ship §4 backfill protocol behind an `app_setting['sync.v2.backfill_enabled']` flag (default off).
- Ship §6 status UI in Settings.
- Internal dogfood for one cycle.

### Phase C — per-entity conflict resolution

- Activate §3 per-entity rules.
- OR-Set wins over the old "blanket LWW everything" for `liked_track` and `library_folder` membership.
- Fractional Index for `playlist_track.position`. **Schema change**: `position TEXT` instead of `INT`. Desktop migration backfills FI strings from current integer positions.
- Profile + library_folder as first-class entities.

### Phase D — Lamport retirement

- HLC is the authoritative ordering.
- v1 `lamport_ts` accepted on ingest for one more cycle (so a stale desktop can still push), but server emits v2 only.
- Wire shape v1 deprecated.

### Phase E — clean-up

- Drop the `lamport_ts` column.
- Drop the dual-shape ingest path.
- Drop the v1 → v2 backfill code.

Each phase is one or two PRs, gated by a feature flag, individually revertable.

## Open questions

1. **HLC wall_clock source.** Should we use system time or NTP-corrected time? Desktops with skewed clocks will produce HLCs that look "from the future" to peers — OR-Set rules handle it but the user might see "Last edit: in 3 hours" in the status UI. Decision: NTP-correct lazily on boot; fall back to system time and tolerate up to 5 min skew before warning.
2. **Fractional Index density attack.** A malicious device (or a bug) can produce ever-longer FI strings ("531415926…"). Cap at 64 bytes; require a server-side rebalance once any playlist hits the cap.
3. **Backfill ordering.** Does the desktop emit `profile` → `library` → `library_folder` → `track` → `playlist` → `playlist_track` → `liked_track` → `track_rating` in a strict topological order, or in parallel with retry-on-parent-missing? Strict ordering is simpler to reason about but slower (serialised over a slow link); parallel with retry is faster but the apply pipeline already has the `Skipped` state for that. Lean parallel; revisit if the retry storm is real.
4. **Conflict UI for the truly ambiguous.** Two devices simultaneously rename a playlist and re-order its tracks. LWW + FI resolves it without ambiguity, but the loser of the rename has no UI signal that their edit was overwritten. Defer to RFC-003.1.
5. **library_folder vs implicit scan roots.** Today every desktop has implicit "scan everything under D:\Music". The library_folder migration needs a clear answer for "what is the canonical_id of the root folder?". Lean: hash of the absolute path + library_id at first boot.
6. **Web client backfill.** Does the web client also "backfill" from the server (it has no local state)? Yes — its initial render is a backfill pull from the server's digest. Same protocol, opposite direction.

## What we won't have to change

- The apply pipeline shape (entity dispatch on `payload.entity`) stays. Only per-entity handlers gain conflict resolution logic.
- The WebSocket fan-out stays. Bandwidth is the same.
- The streaming + artwork + share endpoints. Independent.
- The auth boundary. JWT + JWKS unchanged.

## Estimated effort

| Phase | Effort | Risk |
| --- | --- | --- |
| A — wire-shape additive | 1 week, 1 PR per repo | Low (additive) |
| B — backfill + status UI | 2 weeks, 4-5 PRs | Medium (UX) |
| C — per-entity conflict resolution | 2-3 weeks, 6-8 PRs | High (schema change on position + OR-Set semantics + dogfooding) |
| D — Lamport retirement | 1 week, 2 PRs | Low (cleanup) |
| E — clean-up | 1 week, 1 PR | Low |

Total: **7-8 weeks of focused work**, but pipelinable. Phase B can start before phase A merges as long as the backfill code is gated by the same feature flag.

## Open items to validate before starting

- [ ] Confirm Fractional Index is acceptable for the playlist size we ship (one-time spike: build a 5k-track playlist on the desktop, measure FI string distribution).
- [ ] Confirm HLC wall-clock fallback strategy with the user (NTP vs system, skew tolerance).
- [ ] Confirm the v1 → v2 dual-shape transition is acceptable to release-please (no version mismatch flags between desktop releases).
- [ ] Decide where `library_folder.canonical_id` originates (desktop-only or server-coordinated).
- [ ] Sketch the "Re-sync everything" confirm modal copy + ETA estimation heuristic.

## References

- HLC paper: Kulkarni et al., "Logical Physical Clocks" (2014).
- Fractional Index: Atlassian's "Fractional indexing" blog post (2017); subsequent implementations in Figma, Notion, Linear.
- OR-Set: Shapiro et al., "Conflict-Free Replicated Data Types" (2011).
- RFC-001 §Phase 1.f — the original sync protocol this RFC supersedes.
- RFC-002 §plugin storage — orthogonal but informs how Web Radio favourites would join if needed.
- Issue #43 — review backlog from the monorepo merge (some P1 items related to session error handling intersect with the sync retry surface).
