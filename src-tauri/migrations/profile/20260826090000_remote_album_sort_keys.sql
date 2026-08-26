-- Sort keys for the mirrored albums, on the same terms as the local ones.
--
-- Browsing both sources as one list means sorting them against each other,
-- and that only works if both sides spell the key the same way. The local
-- half sorts on `album.canonical_title` / `artist.canonical_name` — forms
-- produced by `waveflow_core::metadata::name_match::normalize_name`, which
-- lowercases, folds diacritics and drops punctuation. SQLite cannot
-- reproduce any of that: `COLLATE NOCASE` is ASCII-only, so sorting the
-- remote half on its raw display name puts "Björk" and the canonical
-- "bjork" in two different places and splits one artist into two groups
-- in the middle of the list.
--
-- So the mirror computes the same normalised forms in Rust, at the moment
-- it writes the row, and the unified listing sorts on these.
--
-- Nullable because rows mirrored before this migration have none. The walk
-- upserts every album it lists, not only the ones it fetches, so a single
-- pass fills them in; until then the listing falls back to the display
-- title, which is exactly the pre-migration behaviour.
ALTER TABLE remote_album ADD COLUMN sort_title TEXT;
ALTER TABLE remote_album ADD COLUMN sort_artist TEXT;

CREATE INDEX idx_remote_album_sort ON remote_album (sort_artist, sort_title);
