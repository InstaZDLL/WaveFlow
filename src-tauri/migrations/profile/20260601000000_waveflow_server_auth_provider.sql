-- Add `'waveflow_server'` as a per-profile auth credential provider.
-- Phase 1.f.desktop.1 stores the JWT minted by Better Auth (via
-- `waveflow-web`) so the desktop can call `waveflow-server`'s
-- `/api/v1/*` endpoints with an `Authorization: Bearer …` header.
--
-- Per-profile, not app-wide: each desktop profile is meant to map to
-- one Better Auth account, and switching profiles should switch the
-- server identity along with the local library — same model the
-- existing `lastfm` / `listenbrainz` / `deezer` / `spotify` rows follow.
--
-- SQLite cannot alter a CHECK constraint in place, so rebuild the
-- table the same way `add_spotify_auth_provider` did. The
-- `token_encrypted` BLOB stores the raw JWT bytes today (matching the
-- existing `lastfm` row pattern — "encrypted" is aspirational, not
-- enforced); a future hardening pass can wrap it in OS-keyring storage
-- without changing the schema.

PRAGMA foreign_keys = OFF;

CREATE TABLE auth_credential_new (
    provider                TEXT PRIMARY KEY
                            CHECK (provider IN ('lastfm','listenbrainz','deezer','spotify','waveflow_server')),
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
