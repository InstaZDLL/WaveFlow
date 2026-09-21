-- =============================================================================
-- A cached lyrics row that is only good for a while (issue #720).
--
-- An empty row is a cached miss: the waterfall serves it and never asks
-- the network again for that file. That is right when every provider
-- answered "no lyrics". It is wrong when some of them never answered --
-- a host that timed out, an endpoint that refused the request -- because
-- the verdict is then partial, and caching it for good would freeze one
-- provider's outage into a permanent "no lyrics".
--
-- Such a row carries the epoch-millisecond moment after which it stops
-- being served and the track is looked up again. NULL -- every row
-- written before this migration, and every row that is not a partial
-- miss -- is kept for good, exactly as before.
--
-- No index: the column is read on a row already found by its primary key,
-- and the prefetch scans the join it already scans.
-- =============================================================================

ALTER TABLE lyrics ADD COLUMN retry_after INTEGER NULL;
