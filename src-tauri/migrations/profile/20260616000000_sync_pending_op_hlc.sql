-- RFC-003 Phase A.4.2 — HLC pair on local pending ops.
--
-- Every row in `sync_pending_op` now carries the `(hlc_wall,
-- hlc_logical)` pair the v2 wire shape ships under
-- `SyncOpIn.hlc: Option<Hlc>` (waveflow-server #52). The drain task
-- (1.f.desktop.4a + A.4.2) reads these columns when serialising the
-- batch and includes them on the JSON when both are present and
-- > (0, 0); v1 (legacy) rows landed before this migration come up
-- with the DEFAULT 0/0 pair which the drain treats as "absent" and
-- falls back to the legacy shape. The server's dual-shape ingest
-- swallows either form transparently.
--
-- `origin_device_id` is intentionally NOT stored here — it's the
-- whole-install identity returned by `sync::device::ensure` (lives
-- in `app.db`, app-wide), shared across every row in this queue. The
-- drain plants it on the batch via `PushBatchRequest::device_id`,
-- exactly the way the legacy push already does (per A.1.1's header
-- the server treats this string as UUID-shaped TEXT and parses it
-- into the entity row's `origin_device_id`).
--
-- ## CHECK on hlc_logical (CodeRabbit follow-up from A.3)
--
-- The A.3 review flagged that the §2 total order keys on a 32-bit
-- logical counter, but SQLite's INTEGER column accepts any 64-bit
-- value. Without a guard, a future `sync::hlc::next` regression
-- (counter overflow, narrowing bug) could plant `hlc_logical >
-- i32::MAX` into a row, which would then refuse to round-trip
-- through the server's bind site (waveflow-server A.1.1 enforces
-- `0..=i32::MAX` on insert). CHECK at the storage layer surfaces
-- the bug local-first rather than at the server's 23505 path.
--
-- The bound is `0..=2147483647` (i32::MAX). `hlc_wall` is BIGINT-
-- range so no narrowing constraint applies there.

ALTER TABLE sync_pending_op ADD COLUMN hlc_wall    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sync_pending_op ADD COLUMN hlc_logical INTEGER NOT NULL DEFAULT 0
    CHECK (hlc_logical BETWEEN 0 AND 2147483647);
