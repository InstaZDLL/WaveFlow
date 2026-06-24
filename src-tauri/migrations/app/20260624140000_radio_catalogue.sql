-- Offline Web Radio catalogue.
--
-- A user-triggered download (Settings → Data) snapshots the radio-browser
-- station directory into a local searchable table so the Web Radio view can
-- browse + search ~35k stations without network — used automatically when
-- offline mode is on, or always when the `radio.catalogue.local_first`
-- setting is enabled. Lives in app.db (shared across profiles, like the other
-- caches): the directory is global, not user-data.
--
-- `id` is an explicit INTEGER PRIMARY KEY so the download path can keep the
-- contentless FTS rowid in lockstep with the base row (the project's FTS5
-- convention — see `track_fts` in the profile migrations). A full re-download
-- wipes both tables (`DELETE FROM radio_station` + the FTS `'delete-all'`
-- command) and repopulates from scratch.
CREATE TABLE radio_station (
    id            INTEGER PRIMARY KEY,
    stationuuid   TEXT NOT NULL,
    name          TEXT NOT NULL,
    -- Preferred playable URL resolved at download time (url_resolved, falling
    -- back to the raw author url). Never empty — rows with no usable stream
    -- are dropped before insert.
    stream_url    TEXT NOT NULL,
    homepage      TEXT,
    favicon       TEXT,
    country       TEXT NOT NULL DEFAULT '',
    country_code  TEXT NOT NULL DEFAULT '',
    tags          TEXT NOT NULL DEFAULT '',
    bitrate       INTEGER,
    votes         INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_radio_station_country_code ON radio_station (country_code);
CREATE INDEX idx_radio_station_votes ON radio_station (votes DESC);

-- Contentless FTS5 over the searchable text columns (name + tags + country).
-- Populated in lockstep with `radio_station` during the download transaction
-- with the matching rowid; wiped via the `'delete-all'` command on
-- re-download / clear. `content=''` keeps it from storing a second copy of
-- the source text — we own the base row and supply the rowid explicitly.
CREATE VIRTUAL TABLE radio_station_fts USING fts5(
    name,
    tags,
    country,
    content=''
);
