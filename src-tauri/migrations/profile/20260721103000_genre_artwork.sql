-- Manual per-genre picture (issue #424). `genre` isn't a parent of any
-- ON DELETE SET NULL/CASCADE FK, so a plain ALTER TABLE is safe here (see
-- CLAUDE.md's "never DROP TABLE a parent table" note — that risk doesn't
-- apply to ADD COLUMN).
ALTER TABLE genre ADD COLUMN artwork_id INTEGER REFERENCES artwork(id) ON DELETE SET NULL;
