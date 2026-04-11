-- =============================================================================
-- app.db — initial schema
-- Global, shared across all profiles. Contains only the profile registry and
-- global application settings (language, theme, auto-start, minimize-to-tray).
-- =============================================================================

-- Profiles registry --------------------------------------------------------
CREATE TABLE profile (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    color_id        TEXT NOT NULL DEFAULT 'emerald',
    avatar_hash     TEXT,
    data_dir        TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    last_used_at    INTEGER NOT NULL,
    UNIQUE (name)
);

CREATE INDEX idx_profile_last_used ON profile(last_used_at DESC);

-- Global application settings (key-value store) ----------------------------
CREATE TABLE app_setting (
    key             TEXT PRIMARY KEY,
    value           TEXT NOT NULL,
    value_type      TEXT NOT NULL CHECK (value_type IN ('string','int','bool','json')),
    updated_at      INTEGER NOT NULL
);

-- Default global settings --------------------------------------------------
INSERT INTO app_setting (key, value, value_type, updated_at) VALUES
    ('ui.language',         'fr',     'string', strftime('%s','now') * 1000),
    ('ui.theme',            'system', 'string', strftime('%s','now') * 1000),
    ('app.auto_start',      'false',  'bool',   strftime('%s','now') * 1000),
    ('app.minimize_to_tray','true',   'bool',   strftime('%s','now') * 1000),
    ('app.scan_on_start',   'false',  'bool',   strftime('%s','now') * 1000);
