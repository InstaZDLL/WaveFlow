# Database & paths

## On-disk layout

```bash
<app_data_dir>/waveflow/
├── app.db                       (global registry + app settings)
├── avatars/                     (shared profile avatars, blake3-hash-addressed)
├── metadata_artwork/            (shared remote artwork cache, blake3-hash-addressed)
└── profiles/
    └── <profile_id>/
        ├── data.db              (per-profile database)
        └── artwork/             (per-profile embedded artwork cache)
```

`<app_data_dir>` resolves via Tauri's `app_data_dir()`, which honours the bundle identifier (`app.waveflow`):

- Windows: `%APPDATA%\app.waveflow\waveflow\`
- macOS: `~/Library/Application Support/app.waveflow/waveflow/`
- Linux: `~/.local/share/app.waveflow/waveflow/`

The inner `waveflow/` segment is a hardcoded subdirectory in [`paths.rs`](../../src-tauri/src/paths.rs). Don't rename it — existing user libraries point at it. The product display name is `WaveFlow` ([`tauri.conf.json`](../../src-tauri/tauri.conf.json)) but the path stays lowercase for backwards compatibility.

## Two databases

### `app.db` (global)

- `profile` — profile list (one row per profile).
- `app_setting` — typed key/value: `app.last_profile_id`, `lastfm_api_key`, `lastfm_session_key`, `app.theme`, `integrations.discord_rpc`, …
- `deezer_artist` / `deezer_album` — shared metadata cache (Deezer enrichment + Last.fm bios), 30-day TTL via `expires_at`.
- `lyrics` — shared LRCLIB cache (no TTL).

Migrations: [`src-tauri/migrations/app/`](../../src-tauri/migrations/app).

### `data.db` (per-profile)

- Library: `library`, `library_folder`, `track`, `artist`, `album`, `genre`, `track_artist`, `track_genre`, `artwork`, `track_analysis`, `playlist`, `playlist_track`, `liked_track`, `queue_item`, `play_event`, `scrobble_queue`, `profile_setting`, `track_fts` (FTS5 contentless).
- Profile-scoped pool: every command that touches user data goes through `state.require_profile_pool().await?`.

Migrations: [`src-tauri/migrations/profile/`](../../src-tauri/migrations/profile). Applied via `sqlx::migrate!()` at boot for each opened pool.

## Settings

Two flavours, two stores:

| Store                             | Scope       | Used for                                                                                           |
| --------------------------------- | ----------- | -------------------------------------------------------------------------------------------------- |
| `app_setting` (`app.db`)          | App-wide    | API keys, session keys, theme, last-active-profile                                                 |
| `profile_setting` (per `data.db`) | Per-profile | Output device, crossfade, normalize / mono / replaygain toggles, onboarding dismissal, sort memory |

Both follow the same `INSERT … ON CONFLICT DO UPDATE` typed-value pattern (`value_text` / `value_int` / `value_real` / `value_bool` columns + a `kind` discriminator).

## Migration policy

- One numbered SQL file per change, name format `YYYYMMDDHHMMSS_<short_description>.sql`. Sequential; sqlx records applied versions in `_sqlx_migrations`.
- Migrations are **append-only** in normal use. Schema is never re-baselined — new columns are added with `ALTER TABLE`, defaults provided so existing rows stay valid.
- Destructive changes (drop / rename) only after a backwards-compat shim has been live long enough that the worst-case downgrade window is closed.

## Asset protocol scope

Files under `metadata_artwork/`, `avatars/` and `profiles/<id>/artwork/` are served to the renderer via Tauri's asset protocol (`tauri.conf.json::app.security.assetProtocol`). Frontend code uses [`convertFileSrc()`](https://tauri.app/v2/api/js/core#convertfilesrc) to map an absolute path to an `asset://` URL the `<img>` tag can load.

Smart-playlist covers reuse `metadata_artwork/` (no extra scope needed).
