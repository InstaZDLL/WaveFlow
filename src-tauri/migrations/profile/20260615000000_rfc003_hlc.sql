-- RFC-003 §3.1 — desktop per-profile HLC columns. Phase A.3.
--
-- Mirror of the server-side schema landed in waveflow-server #51
-- (migration `20260613000000_entity_hlc.sql`). Every entity row this
-- desktop emits or could re-emit through the sync pipeline grows
-- four columns:
--
--   * `hlc_wall`            BIGINT, Unix epoch milliseconds of the
--                           Hybrid Logical Clock the apply pipeline
--                           stamped on the most-recently-accepted op.
--                           Server-side BIGINT NOT NULL DEFAULT 0; we
--                           match the wire shape with SQLite INTEGER
--                           (which is a 64-bit signed value here).
--   * `hlc_logical`         INTEGER, the i32-range logical counter
--                           paired with `hlc_wall` (§2 total order
--                           sorts by `(wall, logical, origin_device_id)`).
--   * `origin_device_id`    TEXT (UUIDv4-shaped), identifies the device
--                           whose op produced the current state. NULL
--                           when the row predates v2 (Lamport-era)
--                           and the origin is unknown.
--   * `payload_hash`        BLOB, BLAKE3-256 of the canonical wire
--                           form (§4). NULL pre-v2; populated by the
--                           A.4 emit path once we ship it.
--
-- ## Scope on A.3
--
-- This migration ONLY widens the schema. The desktop produces ops
-- but is not yet a consumer of the v2 wire shape — that ships in
-- Phase A.4 alongside the `payload_hash` helper move from
-- `waveflow-server` into `waveflow-core`. Until then every column
-- stays at its DEFAULT (`0` / `NULL`) and round-trips against the
-- server without behavioural change. The server treats absent v2
-- fields on inbound ops as a legacy push (dual-shape ingest landed
-- in #52) so existing 1.f drains keep working untouched.
--
-- ## Rating is a column on `track`, not a sibling table
--
-- The server splits `user_track_rating` into its own table (different
-- tenant scoping — rating is user-scoped, the track row itself is
-- profile-scoped). On desktop the per-profile DB IS the user scope,
-- so we keep `track.rating` as a column and co-locate the rating's
-- HLC + payload_hash on the same row under a `rating_` prefix. Same
-- physical row, two logical sync entities (`track` vs
-- `track_rating`), independent HLCs. This matches the emit-time
-- distinction already in `commands/track.rs` where the
-- `entity: "track_rating"` op carries `track.file_hash` as
-- `entity_id` while metadata-edit ops carry `entity: "track"`.
--
-- ## Backfill policy
--
-- Existing rows get `(0, 0, NULL, NULL)` via the DEFAULTs. That maps
-- 1:1 to the server's "row predates v2" state — when the desktop
-- starts emitting v2 in A.4, its first push for any pre-A.4 entity
-- carries fresh HLC values derived from the local lamport counter
-- (server-side dual-shape ingest in #52 does the rest).

-- =============================================================================
-- 1. Profile-scoped entities (mirror waveflow-server #51 §1)
-- =============================================================================

ALTER TABLE library ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE library ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE library ADD COLUMN origin_device_id TEXT;
ALTER TABLE library ADD COLUMN payload_hash     BLOB;

ALTER TABLE track ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN origin_device_id TEXT;
ALTER TABLE track ADD COLUMN payload_hash     BLOB;

-- Rating sub-entity on the same `track` row. Independent HLC because
-- a metadata edit and a rating change are separate ops that ship at
-- different times — without separate HLC fields one would clobber the
-- other's stamping on read-modify-write.
ALTER TABLE track ADD COLUMN rating_hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN rating_hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE track ADD COLUMN rating_origin_device_id TEXT;
ALTER TABLE track ADD COLUMN rating_payload_hash     BLOB;

ALTER TABLE playlist ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playlist ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playlist ADD COLUMN origin_device_id TEXT;
ALTER TABLE playlist ADD COLUMN payload_hash     BLOB;

ALTER TABLE playlist_track ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playlist_track ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playlist_track ADD COLUMN origin_device_id TEXT;
ALTER TABLE playlist_track ADD COLUMN payload_hash     BLOB;

ALTER TABLE liked_track ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE liked_track ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE liked_track ADD COLUMN origin_device_id TEXT;
ALTER TABLE liked_track ADD COLUMN payload_hash     BLOB;

-- =============================================================================
-- 2. Per-entity digest version (mirror waveflow-server #51 §2 +
--    #53 user_metadata_digest_version)
-- =============================================================================
--
-- One row per synced entity name. `version` is bumped in the same
-- transaction as every op-derived write whose `payload_hash`
-- actually changes (§invariant: bump iff hash differs). The digest
-- endpoint (server #56) joins this counter with the per-entity
-- members hash to give two devices a cheap "are we in sync?" check.
--
-- On the server this is split in two tables (one keyed by
-- `profile_id`, one keyed by `user_id`) because a single Postgres
-- DB hosts every tenant. On desktop the per-profile `data.db`
-- already IS the profile/user scope, so a single keyed-by-entity
-- table covers both `library` / `track` / `playlist` /
-- `playlist_track` (profile-scoped server-side) AND `liked_track` /
-- `track_rating` (user-scoped server-side) without further splitting.
-- The `profile` entity's digest lives in `app.db` (see the matching
-- migration there) because the row itself lives in `app.db`.

CREATE TABLE metadata_digest_version (
    entity      TEXT PRIMARY KEY,
    version     INTEGER NOT NULL DEFAULT 0
);

INSERT INTO metadata_digest_version (entity, version) VALUES
    ('library',        0),
    ('track',          0),
    ('playlist',       0),
    ('playlist_track', 0),
    ('liked_track',    0),
    ('track_rating',   0);
