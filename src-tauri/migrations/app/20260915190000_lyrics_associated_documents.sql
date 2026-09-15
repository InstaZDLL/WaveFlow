-- Translations and pronunciations, stored beside the lyrics they belong to.
--
-- A word-level provider (Apple Music, Musixmatch) returns more than one
-- document for a track: the original, zero or more translations, and a
-- Latin-script pronunciation. `lyrics` and `radio_lyrics` each hold exactly
-- one row per identity, and that is the property worth keeping -- they mean
-- "the lyrics currently chosen for this file", and every existing read
-- depends on that being structural rather than a query that has to ask which
-- row is the primary one. So the extra documents get their own child table
-- instead of a wider key on the parent.
--
-- One child table per parent rather than one polymorphic table for both:
-- a shared table could not carry a real foreign key to either parent, and
-- the two identities differ (`file_hash` vs `artist_title_key`). The columns
-- saved are not worth the referential integrity lost.
--
-- A bundle is replaced whole. When the host caches a new primary document it
-- deletes this track's associated rows and inserts the new provider's in the
-- same transaction, so a Musixmatch original can never end up paired with a
-- leftover Apple translation.
--
-- `format` deliberately carries NO CHECK constraint, unlike the parent
-- tables. SQLite cannot alter a CHECK in place, so `20260516120000` had to
-- recreate `lyrics` wholesale to add 'ttml' -- create-copy-drop-rename. That
-- pattern is now dangerous here: these children cascade, `app.db` opens with
-- `foreign_keys = ON`, and `DROP TABLE` fires an implicit DELETE, so
-- repeating it on a parent would silently erase every translation
-- (docs/architecture/invariants.md, "Never DROP TABLE a parent table").
-- Leaving the constraint out removes the only reason anyone would need to.
-- The value is validated before it is written: the host runs `detect_format`
-- on the document and refuses it unless the sniffed format matches what the
-- plugin declared.
--
-- ⚠️ `lyrics` and `radio_lyrics` are parent tables from this migration on.
-- Never recreate either with create-copy-drop-rename; use ALTER TABLE.

CREATE TABLE lyrics_associated (
    id          INTEGER PRIMARY KEY,
    file_hash   TEXT NOT NULL REFERENCES lyrics(file_hash) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('translation', 'pronunciation')),
    -- BCP-47 tag when the document is a translation. A pronunciation is a
    -- transliteration of the original, so it usually has none; NULL is the
    -- "no language" slot and takes part in the uniqueness rule below.
    language    TEXT,
    content     TEXT NOT NULL,
    format      TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL
);

-- At most one document per (kind, language) in a bundle. `language` is
-- nullable and SQLite treats NULLs as distinct in a UNIQUE index, so the
-- expression form collapses them onto one slot -- otherwise a provider
-- returning two untagged pronunciations would insert both.
CREATE UNIQUE INDEX idx_lyrics_associated_slot
    ON lyrics_associated (file_hash, kind, COALESCE(language, ''));

CREATE TABLE radio_lyrics_associated (
    id               INTEGER PRIMARY KEY,
    artist_title_key TEXT NOT NULL
                     REFERENCES radio_lyrics(artist_title_key) ON DELETE CASCADE,
    kind             TEXT NOT NULL CHECK (kind IN ('translation', 'pronunciation')),
    language         TEXT,
    content          TEXT NOT NULL,
    format           TEXT NOT NULL,
    fetched_at       INTEGER NOT NULL
);

CREATE UNIQUE INDEX idx_radio_lyrics_associated_slot
    ON radio_lyrics_associated (artist_title_key, kind, COALESCE(language, ''));
