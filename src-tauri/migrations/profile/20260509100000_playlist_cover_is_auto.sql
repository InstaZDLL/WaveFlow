-- Track whether the playlist's cover should auto-regenerate from its
-- contents (Spotify-style 2×2 grid of the first 4 album artworks).
-- Defaults to 1 so brand-new user playlists get a real cover the moment
-- they accumulate ≥ 4 tracks. Switches to 0 when the user uploads their
-- own image; switches back to 1 when they "Remove photo".
--
-- Smart playlists (is_smart = 1) ignore this flag entirely — their
-- covers are managed by the smart-playlist regen flow.

ALTER TABLE playlist ADD COLUMN cover_is_auto INTEGER NOT NULL DEFAULT 1;
