-- WebDAV and SMB shares read directly as library folders.
--
-- A share is a transport, not a catalogue: its tracks are ordinary `track`
-- rows, read by our own scanner, whose `file_path` is a locator
-- (`wfnet://<uuid>/<path on the share>`). The uuid is the only link between
-- a row and this table, so changing a NAS's address rewrites nothing, and no
-- credential ever reaches a path, a log line or a playlist export.
--
-- `secret` is stored in clear, like `auth_credential.token_encrypted` and for
-- the same reason (RFC-007: a keyring is worth making for all of them at
-- once). It is never sent to the webview, and `profile_io` blanks it in an
-- exported copy.
CREATE TABLE network_source (
    id                INTEGER PRIMARY KEY,
    uuid              TEXT NOT NULL UNIQUE,
    kind              TEXT NOT NULL CHECK (kind IN ('webdav', 'smb')),
    name              TEXT NOT NULL,
    -- WebDAV: the base URL. SMB: `host` or `host:port`.
    endpoint          TEXT NOT NULL,
    -- SMB only; '' for WebDAV, so the UNIQUE below compares it.
    share             TEXT NOT NULL DEFAULT '',
    username          TEXT,
    secret            TEXT,
    -- The user accepted Basic credentials and audio in clear over http://.
    allow_plain_http  INTEGER NOT NULL DEFAULT 0,
    created_at        INTEGER NOT NULL,
    UNIQUE (kind, endpoint, share)
);
