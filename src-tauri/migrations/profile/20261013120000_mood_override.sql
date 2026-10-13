-- A track the user put into a mood radio, or took out of one, by hand.
-- The tempo and loudness gates cannot tell every slow ballad from a rock
-- track the estimator read at 60 BPM; this is the user's last word.
CREATE TABLE mood_override (
    mood     TEXT    NOT NULL
             CHECK (mood IN ('focus', 'chill', 'workout', 'party', 'sleep')),
    track_id INTEGER NOT NULL REFERENCES track(id) ON DELETE CASCADE,
    kind     TEXT    NOT NULL CHECK (kind IN ('include', 'exclude')),
    added_at INTEGER NOT NULL,
    PRIMARY KEY (mood, track_id)
);

CREATE INDEX idx_mood_override_track ON mood_override(track_id);
