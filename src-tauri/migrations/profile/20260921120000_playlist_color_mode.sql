-- Existing playlists with artwork adopt the image-derived header color.
-- A palette color remains available whenever no artwork can be loaded.
ALTER TABLE playlist ADD COLUMN color_mode TEXT NOT NULL DEFAULT 'auto'
    CHECK (color_mode IN ('auto', 'manual'));
