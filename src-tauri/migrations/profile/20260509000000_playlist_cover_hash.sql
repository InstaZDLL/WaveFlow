-- Add an optional cover image hash to playlists. NULL means "no custom
-- cover" (the icon_id + color_id gradient fallback is rendered instead).
-- Smart auto-playlists (Daily Mix, On Repeat, …) populate this with a
-- composite image generated from the cluster's top artist photos and
-- stored as `<root>/metadata_artwork/<hash>.jpg`, alongside the existing
-- Deezer artwork cache.

ALTER TABLE playlist ADD COLUMN cover_hash TEXT;
