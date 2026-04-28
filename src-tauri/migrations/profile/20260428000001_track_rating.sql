ALTER TABLE track ADD COLUMN rating INTEGER;
CREATE INDEX IF NOT EXISTS idx_track_rating ON track(rating);
