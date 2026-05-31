-- Local pending-ops queue. Phase 1.f.desktop.2 of RFC-001 §6.6.
--
-- Every CRUD command that the user fires while a `waveflow_server`
-- JWT is configured will enqueue a row here (the actual enqueue hooks
-- land in a follow-up 1.f.desktop.2b PR; this migration just stands
-- the table up). A future 1.f.desktop.4 drain task posts queued ops
-- to the server's `POST /api/v1/sync/ops` and removes the rows the
-- server accepts.
--
-- Per-profile (lives in the profile `data.db`) because every op is
-- tenant-scoped to the active Better Auth user. Switching profiles
-- mid-flight doesn't carry pending ops over — the queue and the JWT
-- are both per-profile by construction.
--
-- Invariants the queue keeps:
--
-- - `operation_id` is the idempotency key against the server's
--   `(user_id, device_id, operation_id)` UNIQUE. A retry of a
--   half-failed batch sends the same id and the server short-circuits
--   to the existing row instead of inflating the log.
-- - `lamport_ts` is strictly monotonic per device (server-side UNIQUE
--   on `(user_id, device_id, lamport_ts)` rejects regressions with
--   23505). Local UNIQUE here is defense in depth — a duplicate would
--   surface at INSERT instead of after a costly round-trip.
-- - `op` is gated by a CHECK so a typo'd write doesn't land bytes the
--   server would 400 on.
-- - `id INTEGER PRIMARY KEY AUTOINCREMENT` keeps the rowid strictly
--   monotonic across the table's lifetime — without `AUTOINCREMENT`,
--   SQLite reuses ids after deletes (the next insert picks
--   `MAX(rowid) + 1` of the surviving rows). A future drain task
--   that tracks a "last-processed id" high-water mark would silently
--   miss ops appended after the queue was fully drained, because
--   their reused ids would slot below the mark. The
--   `sqlite_sequence` bookkeeping row that AUTOINCREMENT plants
--   costs one extra small write per insert, which is unmeasurable
--   next to the JSON-payload serialisation the same INSERT already
--   does.

CREATE TABLE sync_pending_op (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    operation_id    TEXT NOT NULL UNIQUE,
    lamport_ts      INTEGER NOT NULL UNIQUE CHECK (lamport_ts > 0),
    entity          TEXT NOT NULL,
    entity_id       TEXT NOT NULL,
    field           TEXT,
    op              TEXT NOT NULL
                    CHECK (op IN ('set','delete','insert','noop')),
    payload         TEXT,
    created_at      INTEGER NOT NULL,
    -- Bookkeeping for the drain task. NULL on rows that haven't been
    -- attempted yet; updated by the drain loop on each retry.
    last_attempt_at INTEGER,
    attempt_count   INTEGER NOT NULL DEFAULT 0,
    -- Last error string the server returned (e.g. a 409
    -- lamport-regression hint or a 5xx burst). Cleared on success
    -- before the row is dropped.
    last_error      TEXT
);

-- Drain pass orders by `id ASC` (FIFO). The PK index already covers
-- that, but a paired index on `created_at` makes the diagnostic
-- "show me the oldest pending row" query cheap.
CREATE INDEX sync_pending_op_created_idx
    ON sync_pending_op (created_at);
