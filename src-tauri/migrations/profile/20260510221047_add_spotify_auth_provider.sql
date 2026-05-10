-- Add Spotify as a per-profile OAuth credential provider.
-- SQLite cannot alter a CHECK constraint in place, so rebuild the table.

PRAGMA foreign_keys = OFF;

CREATE TABLE auth_credential_new (
    provider                TEXT PRIMARY KEY
                            CHECK (provider IN ('lastfm','listenbrainz','deezer','spotify')),
    username                TEXT,
    token_encrypted         BLOB NOT NULL,
    refresh_token_encrypted BLOB,
    expires_at              INTEGER,
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL
);

INSERT INTO auth_credential_new
    (provider, username, token_encrypted, refresh_token_encrypted, expires_at, created_at, updated_at)
SELECT provider, username, token_encrypted, refresh_token_encrypted, expires_at, created_at, updated_at
  FROM auth_credential;

DROP TABLE auth_credential;
ALTER TABLE auth_credential_new RENAME TO auth_credential;

PRAGMA foreign_keys = ON;
