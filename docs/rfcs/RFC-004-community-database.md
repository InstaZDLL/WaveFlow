# RFC-004 — Community-DB: opt-in shared metadata for tracks, artists, albums

- **Status**: Draft
- **Date**: 2026-06-07
- **Authors**: @InstaZDLL
- **Supersedes**: —
- **Depends on**: [RFC-001](RFC-001-waveflow-server.md) — Community-DB runs on `waveflow-server` and reuses its auth, sync, and apply pipelines.
- **Implementation tracking**: opened as the `Phase 2 — Community-DB` GitHub milestone once 1.5.0 lands.

---

## 1. Context

WaveFlow already fetches metadata from three remote sources today: **Deezer** (artist pictures + album covers + biography hints), **Last.fm** (artist bios + similar-artist suggestions), and **LRCLIB** (synced lyrics). Each one is great at exactly one job — Deezer's coverage of mainstream pop is thorough, Last.fm has the longest tail of biographies, LRCLIB has community-contributed time-coded lyrics for over a million tracks.

All three have the same shortcomings for a self-hostable music app:

- **Coverage gaps.** Self-releases, niche genres, foreign-language artists, classical recordings, and anything older than a major-label digital re-issue regularly come back empty from all three. The user re-tags by hand, and that re-tag stays trapped on their device.
- **Drift and corrections.** When MusicBrainz updates an album's release date, when an artist's official name changes, when a translation gets fixed — the only signal the desktop sees is "the value I cached is wrong". The next user to ask the same question gets the same wrong answer.
- **No long-tail BPM, key, or musical-feature data.** Spotify has it (it was Echo Nest before); none of the three sources WaveFlow uses today does. The smart-playlist engine (#171 Web Radio, Daily Mix, On Repeat) can do tempo-bucketing only on tracks the desktop analyser has already scanned locally.
- **Privacy of cover art.** Bundled music files often ship with low-quality embedded artwork. The desktop's cover-pipeline already supports user-uploaded covers; nothing today shares that improvement back with the rest of the install base.

A **community database** — opt-in, federated through `waveflow-server`, modelled on the LRCLIB pattern — closes those gaps without forcing users into a centralised SaaS. Each user keeps a local cache; their server (when they run one) participates in a shared pool of contributions; the contributions are dedup'd, version-tracked, and vote-moderated.

This RFC locks the data model + API surface **before any contribution code lands** so the first opt-in user in v1.6.0 doesn't paint us into a corner.

## 2. Goals

- **A shared, opt-in pool** of crowdsourced metadata across all `waveflow-server` instances that opt in: lyrics (already covered by LRCLIB, mirrored for offline + privacy), artist bios, album metadata corrections, BPM + musical key + duration spectra (from the existing audio analyser). User-uploaded **cover art is a product goal but out of scope for v1 (planned v2)** — see §5.4 for the deferral rationale.
- **Privacy-first.** Contributions carry no listener identity. The contributing instance's user_id stays local; the upstream sees an anonymous BLAKE3 fingerprint of the track / album / artist key and the payload. No play history, no listening behaviour, no library contents.
- **Self-hostable at every level.** A single-binary `waveflow-server` ships with a built-in community-DB schema. Operators choose whether to mirror the public pool, run their own private pool (LAN family, school music club, fan community), or both. No mandatory dependency on a central hosted service.
- **Vote-based moderation, not algorithmic.** Contributions accrue +1 / −1 votes from users who looked at them. Conflicting versions are surfaced by the client with the winning vote count, not auto-merged. Same model LRCLIB ships and that's worked for two years.
- **Compatible with the fallback chain.** The existing desktop lookup order (embedded tags → local cache → Deezer → Last.fm → LRCLIB → empty) gains community DB as the **last source before empty**, so users who don't opt in see no behaviour change.
- **Federation-ready.** A future Phase 3+ release can let two `waveflow-server` instances pull from each other (pull-only — never push) so a curated server doesn't poison a more open one. v1 ships single-mirror, federation is a follow-up RFC.

## 3. Non-goals

- **Replacing Deezer / Last.fm / LRCLIB.** They stay in the fallback chain, just one rung above community DB. Users on free-tier servers without contributions still hit them first.
- **Hosted SaaS pool.** A reference public mirror may exist (run on the project's own infra) but is not a SaaS — it's just one mirror among many. Users self-host theirs.
- **Real-time push notifications** when someone contributes to a track in your library. Pull-only on a fixed cadence keeps the architecture simple. A polling client checks every 24 h by default.
- **User-to-user direct sharing.** Contributions go through a server. Two desktops can't gossip directly. Keeps the privacy boundary at the server.
- **Audio fingerprinting (Chromaprint / AcoustID).** Out of v1 scope. v1 keys contributions on scanner-extracted track metadata (artist + album + title + duration, NUL-separated and BLAKE3-hashed — see §5.3 for the exact recipe), not on the audio waveform. AcoustID integration is a Phase 3+ conversation.
- **Personal data.** Ratings, likes, listening history, playlists — none of those are community-shareable. Those stay in the per-user sync stream (Phase 1.f).
- **Spotify / Apple Music feature parity.** No mood vectors, no playlist recommendations, no editorial content. Community DB is a structured-facts pool — not a curation product.

## 4. Architecture overview

```text
┌─────────────────────────────────────────────────────────────────────┐
│  Public mirror(s) — community-mirror.waveflow.app, etc.             │
│                                                                     │
│  Postgres                                                           │
│   ├── community_contribution    (one row per submission)            │
│   ├── community_vote            (one row per (user, contribution))  │
│   ├── community_entity_key      (the lookup keys we accept)         │
│   └── community_moderation_log  (audit trail for invalidations)     │
│                                                                     │
│  HTTP                                                               │
│   ├── GET  /api/v1/community/lookup    (anonymous read)             │
│   ├── POST /api/v1/community/contribute (JWT-authed write)          │
│   ├── POST /api/v1/community/vote      (JWT-authed)                 │
│   └── GET  /api/v1/community/queue     (moderator-only)             │
└─────────────────────────────────────────────────────────────────────┘
        ▲                          ▲                          ▲
        │                          │                          │
   periodic pull              opt-in push                 admin tools
        │                          │                          │
┌───────┴──────────┐    ┌──────────┴──────────┐    ┌──────────┴──────┐
│  My server       │    │  Another self-hosted│    │  A LAN-only     │
│  (private pool   │    │  server (opt'd in)  │    │  family server  │
│   if disabled)   │    │                     │    │  (no mirror)    │
└──────────────────┘    └─────────────────────┘    └─────────────────┘
        ▲
        │  JWT bearer
        │
┌───────┴────────┐
│ Desktop client │
└────────────────┘
```

Each `waveflow-server` instance is independent. The arrows are **HTTP pulls only** — there's no peer-to-peer gossip, no shared write quorum. A misbehaving mirror gets unsubscribed by everyone who points at it; the rest of the network keeps working.

## 5. Decisions

### 5.1 LRCLIB as the reference pattern

LRCLIB ([lrclib.net](https://lrclib.net), open-source server at [github.com/tranxuanthang/lrclib](https://github.com/tranxuanthang/lrclib)) is the closest existing precedent. Two years of operation, ~1.4 M tracks indexed, no algorithmic moderation, no account requirement for reading. Their schema and API conventions are battle-tested.

We adopt:

- **Anonymous read.** No JWT needed to look up a contribution by entity key. LRCLIB does this, and the alternative (requiring sign-in to look up lyrics) defeats the offline-first goal of the desktop app.
- **JWT-authed write.** Contributions and votes need an account — same Better Auth flow the rest of the server already uses, so federated accounts (Google / Apple per RFC-004's-not-yet-written sibling on OAuth) just work.
- **Multiple versions per entity.** The same `(artist, album, title)` tuple can have N contributions, ranked by vote count. The client picks the highest-voted but knows the alternatives exist (useful for "this lyric is the wrong language for me" UX).
- **Vote count as the only quality signal.** No content-based filtering, no ML moderation, no edit history merging. The conflict-resolution UI in the desktop client picks the highest-voted version by default and lets the user switch.

We diverge from LRCLIB in three respects:

- **Multi-entity, not lyrics-only.** Schema accommodates lyrics, bios, covers, BPM, key, year. Same vote primitive, same lookup primitive, different `payload_kind`.
- **Federated mirrors.** LRCLIB is one canonical server. We let any operator run one. v1 ships single-mirror; v2 lets you point at multiple and merge.
- **Self-host first.** LRCLIB users hit lrclib.net by default. WaveFlow users hit *their* `waveflow-server`, which may or may not have a public mirror configured.

### 5.2 Opt-in granularity

A single `app_setting['community.contribute']` boolean is too coarse. We use three knobs:

| Setting key                              | Default | Effect                                                                                       |
| ---------------------------------------- | ------- | -------------------------------------------------------------------------------------------- |
| `app_setting['community.lookup_enabled']`   | `true`  | Whether to consult community DB in the fallback chain. Read-only access, no identity leaked. |
| `app_setting['community.contribute_enabled']` | `false` | Whether to push corrections + new entries upstream. Default off — explicit opt-in.            |
| `app_setting['community.mirror_url']`     | `null`  | The URL of the public mirror to pull from. `null` = no upstream (private pool only).         |

The desktop UI surfaces the three as a Settings → Community section. The default deployment is **lookup enabled, contribute disabled, mirror unset** — i.e. nothing changes for a user who doesn't touch the settings, and the community DB simply has no data to return.

### 5.3 Entity keys (how we identify what's being contributed)

Each contribution attaches to one **entity key**. Three kinds:

- **Track key**: BLAKE3 hex of `lowercase(artist) || "\0" || lowercase(album) || "\0" || lowercase(title) || "\0" || duration_ms_rounded_to_seconds`. Matches LRCLIB's input shape — chosen because file-BLAKE3 would tie contributions to a single rip (re-mastered re-releases would never benefit from a prior contribution).
- **Album key**: BLAKE3 hex of `lowercase(album_artist) || "\0" || lowercase(album_title) || "\0" || release_year`. `release_year` rounds down to the decade for albums whose year is unknown locally, with a `year_precision` discriminator on the contribution.
- **Artist key**: BLAKE3 hex of `lowercase(artist_name)`. Single-string key; no disambiguation — the highest-voted contribution per `(artist_name)` wins. Disambiguation by sub-genre / country is a Phase 3+ extension.

Lowercase before hashing because case is a font choice, not a music-identity choice. Unicode normalization (NFKC) before lowercase because we don't want `É` (composed) vs `É` (decomposed) to produce two contributions.

**Separator is `\0` (NUL byte), not the empty string.** An empty separator would collapse `("AB", "C")` and `("A", "BC")` into the same hash — a silent collision on every split-point ambiguity between adjacent fields. NFKC + lowercase never produces a NUL byte from real metadata, so the boundary parser can reject a NUL-containing field outright as malformed input and the separator stays a content-free marker.

**Why not MusicBrainz IDs:** they're the cleanest identifier in theory but require the desktop to have already resolved them, which most installations don't. Plus, MusicBrainz contributions belong upstream of WaveFlow — we don't want to host a parallel MBID database. The track/album/artist hash keys above accept a contribution without requiring any prior lookup.

### 5.4 Payload kinds

Each contribution declares its `payload_kind`. v1 ships five kinds:

| `payload_kind`     | Entity key kind | Payload shape                                                                                                  |
| ------------------ | --------------- | -------------------------------------------------------------------------------------------------------------- |
| `lyrics_plain`     | track           | `{ text: string, language?: ISO-639-1 }`                                                                       |
| `lyrics_synced`    | track           | `{ format: 'lrc' \| 'enhanced_lrc' \| 'ttml', content: string, language?: ISO-639-1 }`                          |
| `artist_bio`       | artist          | `{ language: ISO-639-1, short: string (≤ 280 chars), long?: string (≤ 4000 chars) }`                          |
| `album_metadata`   | album           | `{ release_date?: 'YYYY-MM-DD', label?: string, genre?: string, total_tracks?: int }` (each field independently votable) |
| `audio_features`   | track           | `{ bpm?: number, musical_key?: string (Camelot), tempo_confidence?: number, energy?: number, valence?: number }` (numeric, same shape the local analyser emits) |

**`cover_art` is a product goal but out of scope for v1 (planned v2).** It lands as a sixth `payload_kind` (upload-via-artwork-pipeline as a separate hash, with the contribution pointing at the artwork hash so we reuse RFC-001's existing pipeline). Deferred so the moderation surface stays small for the first ship — community-contributed bitmap content needs its own review pass (legal review for embedded photos, takedown flow) that the text-only kinds don't.

### 5.5 Vote-based moderation, not state machines

A contribution starts at score 0. Each `community_vote` row adds +1 or −1. A contribution drops out of the lookup response when `score < SCORE_FLOOR` (default `-3`); the row itself stays in the database for the moderation log but never lands in `/lookup` results.

A separate `moderator` role can flip a contribution to `state = 'invalidated'` with a reason. Invalidated contributions are excluded from lookup permanently, regardless of score. Used for blatant abuse (slurs, doxxing, non-music content) — the moderation log is public so a server operator can audit any invalidation.

Moderator role is granted by the server operator in `users.role`. No election, no DAO, no on-chain anything. Self-hosted means the operator decides.

### 5.6 Anti-abuse model

The combination "anonymous reads + JWT-authed writes + vote-moderated" is LRCLIB's, and it's held up for two years. We add three guardrails for the multi-mirror case:

- **Rate-limit per user.** 30 contributions / hour / user across all `payload_kind`. Tracked at the server, not at the client. A botnet would have to provision N Better Auth accounts to scale past it, which is real friction.
- **Same-content collapse.** Two contributions with the same `entity_key` + `payload_kind` + payload hash collapse to one row at insert time (`ON CONFLICT DO NOTHING`). Stops "submit the same thing 100 times to game vote count" — we vote on rows, not submissions.
- **Mirror reputation, not user reputation.** When a server pulls from a mirror that turns out to be poisoned, the operator can blocklist the mirror. We don't try to reputation-rate individual contributors. Self-host means the operator decides who they trust.

## 6. Schema

```sql
-- Top-level table. Each row is one (entity, kind, payload) submission.
-- Multiple rows per (entity_key, payload_kind) are normal — the
-- lookup endpoint orders by score and returns up to N variants.
CREATE TABLE community_contribution (
    id              BIGSERIAL PRIMARY KEY,
    entity_key      TEXT NOT NULL CHECK (entity_key ~ '^[0-9a-f]{64}$'),
    entity_kind     TEXT NOT NULL CHECK (entity_kind IN ('track', 'album', 'artist')),
    payload_kind    TEXT NOT NULL CHECK (payload_kind IN (
        'lyrics_plain', 'lyrics_synced',
        'artist_bio',
        'album_metadata',
        'audio_features'
    )),
    -- Payload is JSON; the per-kind shapes are documented in §5.4
    -- and enforced by the apply pipeline before INSERT.
    payload         JSONB NOT NULL,
    -- BLAKE3 of payload (canonical JSON) — used by the
    -- same-content-collapse UNIQUE below.
    payload_hash    TEXT NOT NULL CHECK (payload_hash ~ '^[0-9a-f]{64}$'),
    -- Author. NULL when the row was migrated from a federated pull
    -- (we don't track upstream users).
    user_id         BIGINT REFERENCES users(id) ON DELETE SET NULL,
    -- For provenance / dedup across mirrors.
    source_mirror   TEXT,
    created_at      BIGINT NOT NULL,
    -- Tally maintained by triggers on community_vote insert/update.
    score           INTEGER NOT NULL DEFAULT 0,
    state           TEXT NOT NULL DEFAULT 'active'
                    CHECK (state IN ('active', 'invalidated')),
    -- Stamped on `state = 'invalidated'`. Otherwise NULL.
    invalidated_at  BIGINT,
    invalidated_by  BIGINT REFERENCES users(id) ON DELETE SET NULL,
    invalidation_reason TEXT
);

-- Same-content collapse — keeps "submit 100 of the same thing"
-- from accumulating distinct rows.
CREATE UNIQUE INDEX uniq_contribution_content
    ON community_contribution(entity_key, payload_kind, payload_hash);

-- Hot lookup path: pull every active contribution for a (key, kind)
-- ordered by score.
CREATE INDEX idx_contribution_lookup
    ON community_contribution(entity_key, payload_kind, score DESC)
    WHERE state = 'active';

CREATE TABLE community_vote (
    contribution_id BIGINT NOT NULL REFERENCES community_contribution(id) ON DELETE CASCADE,
    user_id         BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    weight          SMALLINT NOT NULL CHECK (weight IN (-1, 1)),
    created_at      BIGINT NOT NULL,
    PRIMARY KEY (contribution_id, user_id)
);

CREATE TABLE community_moderation_log (
    id              BIGSERIAL PRIMARY KEY,
    contribution_id BIGINT NOT NULL REFERENCES community_contribution(id) ON DELETE CASCADE,
    actor_id        BIGINT NOT NULL REFERENCES users(id) ON DELETE SET NULL,
    action          TEXT NOT NULL CHECK (action IN ('invalidate', 'restore')),
    reason          TEXT,
    created_at      BIGINT NOT NULL
);

-- Score-sync trigger: keep `community_contribution.score` consistent
-- with the live tally in `community_vote` without an explicit
-- `recompute_score` call site. INSERT bumps by NEW.weight, DELETE
-- by -OLD.weight, UPDATE applies the delta. Pure-SQL trigger so
-- the score is the row state after the writing transaction commits;
-- the lookup endpoint reads `community_contribution.score`
-- directly and never has to JOIN against `community_vote`.
CREATE OR REPLACE FUNCTION update_contribution_score()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE community_contribution
            SET score = score + NEW.weight
          WHERE id = NEW.contribution_id;
        RETURN NEW;
    ELSIF TG_OP = 'UPDATE' THEN
        -- The PK is `(contribution_id, user_id)` so this branch
        -- only fires when `weight` flips (`+1` ↔ `-1`); contribution
        -- moves are impossible from this trigger's POV.
        UPDATE community_contribution
            SET score = score + NEW.weight - OLD.weight
          WHERE id = NEW.contribution_id;
        RETURN NEW;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE community_contribution
            SET score = score - OLD.weight
          WHERE id = OLD.contribution_id;
        RETURN OLD;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER community_vote_score_sync
    AFTER INSERT OR UPDATE OR DELETE ON community_vote
    FOR EACH ROW EXECUTE FUNCTION update_contribution_score();
```

The trigger keeps `community_contribution.score` in lockstep with `community_vote` writes — the lookup endpoint at `GET /api/v1/community/lookup` reads `community_contribution.score` directly and never joins against `community_vote`.

**Archival procedure for `community_vote` (operational, not auto-run).** Once `community_vote` rows are older than the archival cooldown (target: 1 year), an operator can move them out of the hot table without corrupting `community_contribution.score`. The naïve approach — `DELETE FROM community_vote WHERE created_at < $cutoff` — would fire the `community_vote_score_sync` trigger and decrement every contribution's score back down to zero, which is precisely what we want to avoid (the score is the historical tally; the votes themselves are auditing). The supported procedure is a single transaction that:

1. Opens `BEGIN; SET LOCAL session_replication_role = 'replica';` — suppresses user-defined row triggers (`community_vote_score_sync` included) for the duration of THIS transaction only. `LOCAL` scopes the setting to the txn so a peer write on another connection still fires the trigger normally.
2. `SELECT id FROM community_contribution WHERE id IN (SELECT contribution_id FROM community_vote WHERE created_at < $cutoff) FOR UPDATE;` — locks every contribution whose votes are about to move, so a concurrent vote on those rows blocks until the archival commits. Without this, a vote landing mid-archival could see a stale score and roll back the running tally.
3. `INSERT INTO community_vote_archive SELECT * FROM community_vote WHERE created_at < $cutoff;` — moves the rows to the archive table (same shape as `community_vote`, no triggers attached).
4. `DELETE FROM community_vote WHERE created_at < $cutoff;` — fires nothing because of step 1, so `community_contribution.score` is preserved bit-for-bit.
5. `COMMIT;` — releases the row locks and restores normal trigger behaviour for the connection.

`community_vote_archive` is created on the fly the first time the procedure runs; the operator's cron job (no automated archival job ships in v1) is the only caller. A future Phase 2.d sub-task can wrap this in a `waveflow-server-admin community vote archive --before <date>` CLI for ergonomics.

Epoch-millis BIGINT for every timestamp (per the [server CLAUDE.md](../../CLAUDE.md) convention).

## 7. API surface

### 7.1 Lookup (anonymous)

```http
GET /api/v1/community/lookup?entity_kind=track&entity_key=<64-hex>&payload_kind=lyrics_synced
```

Returns at most 5 active contributions, ordered `score DESC, id ASC` for determinism. Empty array when no contribution exists. **No JWT required** — the lookup endpoint is the read side of the offline-first fallback chain, asking the user to sign in just to discover lyrics would be the wrong call.

```json
{
  "entity_kind": "track",
  "entity_key": "af1349…",
  "payload_kind": "lyrics_synced",
  "results": [
    {
      "id": 4421,
      "score": 27,
      "payload": { "format": "enhanced_lrc", "content": "[00:00.00] …" },
      "created_at": 1717545600000
    }
  ]
}
```

### 7.2 Contribute (JWT-authed)

```http
POST /api/v1/community/contribute
Authorization: Bearer <jwt>

{
  "entity_kind": "track",
  "entity_key": "af1349…",
  "payload_kind": "lyrics_synced",
  "payload": { "format": "enhanced_lrc", "content": "[00:00.00] …" }
}
```

Server canonicalises the JSON (BTreeMap-ordered keys), BLAKE3-hashes it, and INSERTs with `ON CONFLICT (entity_key, payload_kind, payload_hash) DO NOTHING`. Response carries the resolved `contribution_id` so the client can immediately follow up with an upvote.

### 7.3 Vote (JWT-authed)

```http
POST /api/v1/community/vote
Authorization: Bearer <jwt>

{ "contribution_id": 4421, "weight": 1 }
```

Idempotent. Re-vote with the same weight is a no-op; flipping weight (`+1` → `-1`) UPDATEs the row. The trigger keeps `score` in sync.

### 7.4 Moderation queue (moderator-only)

```http
GET /api/v1/community/queue?state=pending&limit=50
```

Returns contributions below `SCORE_FLOOR` that haven't been explicitly invalidated yet — the operator's review backlog. The mutation endpoint at `POST /api/v1/community/moderate` flips state + writes the moderation log entry.

## 8. Client integration: extending the fallback chain

The current desktop lookup chain for lyrics (from [`docs/features/playback.md`](../features/playback.md)):

```text
embedded TXXX / USLT tags  →  local cache  →  Deezer  →  Last.fm  →  LRCLIB  →  empty
```

The proposed chain after RFC-004 ships:

```text
embedded TXXX / USLT tags
    → local cache
    → Deezer / Last.fm / LRCLIB (parallel, first non-empty wins)
    → community DB (new)
    → empty
```

Community DB sits **last** before empty because:

- LRCLIB is already crowdsourced and has higher coverage today. No point ranking community DB ahead until it has catalogue depth.
- Putting community DB ahead would mean a poorly-voted contribution beats a high-quality LRCLIB match. We'd rather miss than mislead.
- A future v2 can let users re-order the chain in Settings → Sources. Out of v1 scope.

The same chain applies to artist bios, album metadata, and audio features — each kind gets its own chain ordering pinned in `lib/lookup-chain.ts` (desktop) and `lookup_chain` (waveflow-core).

A failed lookup logs the entity key locally (in a circular buffer of the last N misses per user) so a user who opted in to contribute can see "you've tried looking up these 12 tracks today, none have community data — contribute one?". UI only, no upstream telemetry.

## 9. Privacy boundary

Three commitments encoded into the design above:

- **No listener identity in the contribution stream.** The `entity_key` is a content-derived hash, not a per-user signature. Server logs MUST NOT correlate a contribution with the contributor's library — they only see "user X contributed Y bytes at time T", which is the same information any HTTP request leaks.
- **No play history.** Listening behaviour (which tracks the user plays, when, how often) stays in the per-user sync stream from Phase 1.f. It's deliberately separate from community contributions — there's no `audio_features.played_by` field, no `lyrics.requested_by` log.
- **No remote query of "does user X have track Y in their library?"** Lookup is by content-derived `entity_key`, so the server cannot reverse a lookup into a library inventory. (A server operator can SEE the keys their users look up, the same way an LRCLIB operator can — that's true of any read API. Local-cache the result so the cold lookup happens once.)

For users running a server alone (LAN, family, school), this is a non-issue — there's nothing community-shaped happening. For users mirroring a public pool, the threat model is "what can the upstream learn about me?" and the answer is "the same things the upstream of any HTTP API can learn — which `entity_key`s you queried, how often, from which IP — but nothing about your library identity, ratings, or play history".

## 10. Open questions

- **Localisation of contributions.** A `lyrics_plain` row carries a `language` field, but how does the client filter "give me the Spanish version of this lyric, not the auto-translated English one"? Initial answer: the highest-voted contribution per `(entity_key, payload_kind, language)` wins; a missing language returns the highest-voted across all languages. Wire this up only when there's a real Spanish ↔ English conflict in the wild; until then, language is an advisory field.
- **Audio features without local re-analysis.** A user with the existing analyser disabled can't pull `audio_features` from community DB and then re-derive BPM at playback time — the BPM is just a number, not the time-domain features. Open question: do we ALSO publish a per-track time-domain summary that lets a remote client validate the contribution? Probably no in v1, but worth thinking through before audio_features ships.
- **Federation read-merge.** Two mirrors disagree on the score of the same `entity_key + payload_hash`. Whose wins on a pull? Probably "the local server treats every mirror as a separate source — the contribution is duplicated, scores are summed locally, the original mirror's ID is preserved in `source_mirror`". To be locked in a follow-up RFC.
- **Mirror discovery.** Today the operator sets `community.mirror_url` by hand. A future "list of known mirrors" file pulled from a well-known location would lower the friction — but introduces a centralised list nobody wants to maintain. Out of v1 scope.
- **GDPR right-to-be-forgotten.** A user who contributed and later deletes their account: do we strip `user_id` (`ON DELETE SET NULL` is already in the schema, so yes) AND invalidate every contribution they authored? Probably keep their contributions live but unattributed — they were submitted as "community" data, not personal data. To confirm with the project's privacy lead before 1.6.0.

## 11. Implementation phasing

Implementation is gated by 1.5.0 cut (per the post-1.g sprint plan). Once 1.5.0 lands, three sub-phases:

| Phase    | Scope                                                                                                                |
| -------- | -------------------------------------------------------------------------------------------------------------------- |
| **2.a**  | Server schema + endpoints (`/lookup`, `/contribute`, `/vote`). No moderation queue yet, no client integration.        |
| **2.b**  | Desktop integration: add community DB to the fallback chain for lyrics + artist bios. Settings → Community panel.    |
| **2.c**  | Moderation queue + moderator role + invalidation flow. Audio features + album metadata payload kinds.                 |
| **2.d**  | Federated pulls (one mirror at a time). Cover-art payload kind via the artwork pipeline. UI for switching versions. |

Each is its own PR series; no big bang. Phase 2.a is the smallest viable thing (read + write + vote, lyrics only) that LRCLIB-style services have shown sufficient.

Federation across multiple mirrors (the merge logic) gets its own RFC once 2.d lands — we'll have learned enough by then to lock the model.

## 12. Acceptance criteria for this RFC

This RFC is "Accepted" when:

- The schema in §6 passes a SQL review (cardinality, index coverage, FK behaviour) and the payload-kind JSON shapes in §5.4 round-trip cleanly through serde + a hand-rolled wire format test.
- The API endpoints in §7 have been sketched as OpenAPI specs in `waveflow-server` (no implementation, just the spec). The spec compiles and lints clean.
- The privacy boundary in §9 has been reviewed by at least one external contributor flagged as @InstaZDLL's reviewer-of-record for community features.
- A short companion `docs/features/community.md` page has been opened against this RFC (placeholder content — fills in during Phase 2.a).

Until then the status stays `Draft` and the implementation milestone stays unopened.
