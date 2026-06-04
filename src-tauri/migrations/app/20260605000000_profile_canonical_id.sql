-- Phase 1.g.3 — desktop profile canonical id.
--
-- Server-side Phase 1.g.0 (waveflow-server PR #26) added
-- `sync_op.profile_canonical_id` so the apply pipeline can route each
-- desktop op to a materialised server profile. Phase 1.g.3-desktop
-- closes the loop: the drain task injects the active profile's
-- canonical id into every outbound op so the server can resolve
-- `(user_id, canonical_id) → server profile.id`.
--
-- The column lives in `app.db` (not the per-profile `data.db`) because
-- the profile registry itself is shared across profiles — same place
-- the rest of `profile.*` columns already live. Partial UNIQUE on
-- non-NULL so the backfill order doesn't matter and a freshly-
-- migrated install with zero profiles isn't constrained.
--
-- ## Backfill
--
-- Migration only widens the schema. UUID v4 generation for pre-
-- existing rows happens at startup in `crate::db::profile_meta::
-- backfill_canonical_ids` so the entropy + version/variant bits come
-- from Rust's `uuid` crate rather than a brittle hand-rolled
-- `randomblob(16)` byte-twiddle. The startup hook is idempotent —
-- only `WHERE canonical_id IS NULL` rows get touched, so re-runs
-- across reboots are cheap.

ALTER TABLE profile ADD COLUMN canonical_id TEXT;
CREATE UNIQUE INDEX idx_profile_canonical_id
    ON profile (canonical_id)
    WHERE canonical_id IS NOT NULL;
