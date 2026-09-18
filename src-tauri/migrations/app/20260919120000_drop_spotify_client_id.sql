-- Remove the Spotify Developer Client ID (issue #680).
--
-- The token lives per profile, in `auth_credential`; the Client ID is
-- app-wide and sits here, which is why it needs its own migration. It is
-- not a secret in the way the token is — a Spotify PKCE client id is
-- public by design — but nothing reads the key any more, and leaving an
-- identifier for a developer application behind serves no one.

DELETE FROM app_setting WHERE key = 'app.spotify_client_id';
