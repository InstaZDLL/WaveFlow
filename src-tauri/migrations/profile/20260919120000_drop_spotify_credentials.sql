-- Remove the Spotify integration's stored credential (issue #680).
--
-- The CHECK constraint on `auth_credential.provider` still lists
-- 'spotify'. It stays: narrowing it would mean rebuilding the table, and
-- a value nothing can ever insert again is inert. Merged migrations are
-- immutable, so 20260510221047 keeps its text as well.
--
-- What is NOT inert is the row itself: an encrypted OAuth refresh token
-- for an account the user can no longer disconnect from inside the app,
-- because the screen that did that is gone. It goes.

DELETE FROM auth_credential WHERE provider = 'spotify';

-- The sidebar-entry preference has no reader left either.
DELETE FROM profile_setting WHERE key = 'ui.show_spotify';
