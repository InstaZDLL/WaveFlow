-- RFC-003 §3.1 — desktop app-wide HLC columns. Phase A.3.
--
-- Sibling of `migrations/profile/20260615000000_rfc003_hlc.sql`.
-- The `profile` registry row lives in `app.db` (shared cross-
-- profile) so its HLC + digest version live here, while every other
-- synced entity (`library` / `track` / `playlist` / `playlist_track`
-- / `liked_track` / `track_rating`) lives per-profile and its
-- HLC columns ship in the profile-side migration of the same date.
--
-- Mirrors the server-side schema landed in waveflow-server #51 (the
-- `profile` table addition) plus #53 (`user_metadata_digest_version`
-- which on desktop collapses into the same shape since the per-DB
-- scope already disambiguates tenants).
--
-- ## Scope
--
-- ALTER + DEFAULT only. Existing rows backfill to `(0, 0, NULL,
-- NULL)`; no emit-side code change ships here (A.4 owns the v2
-- wire format and the `payload_hash` move into `waveflow-core`).

-- =============================================================================
-- 1. profile registry HLC columns (mirror waveflow-server #51 §1)
-- =============================================================================

ALTER TABLE profile ADD COLUMN hlc_wall         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE profile ADD COLUMN hlc_logical      INTEGER NOT NULL DEFAULT 0;
ALTER TABLE profile ADD COLUMN origin_device_id TEXT;
ALTER TABLE profile ADD COLUMN payload_hash     BLOB;

-- =============================================================================
-- 2. App-wide entity digest version (mirror waveflow-server #51 §2)
-- =============================================================================
--
-- Only the `profile` entity lives in `app.db`, so this counter has a
-- single seeded row. We keep the same `(entity, version)` schema as
-- the per-profile sibling table so the digest endpoint client can
-- query both with identical SQL.

CREATE TABLE metadata_digest_version (
    entity      TEXT PRIMARY KEY,
    version     INTEGER NOT NULL DEFAULT 0
);

INSERT INTO metadata_digest_version (entity, version) VALUES
    ('profile', 0);
