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

The inner `waveflow/` segment is a hardcoded subdirectory in [`paths.rs`](../../src-tauri/crates/app/src/paths.rs). Don't rename it — existing user libraries point at it. The product display name is `WaveFlow` ([`tauri.conf.json`](../../src-tauri/tauri.conf.json)) but the path stays lowercase for backwards compatibility.

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

#### Listening history outlives its tracks

`play_event.track_id` used to be `NOT NULL … ON DELETE CASCADE`, so removing a folder (`DELETE FROM track WHERE folder_id = ?`) or a library silently erased the matching history. One beta tester lost their stats five times that way, and no backup helps: the archive restores the *old* library, not the history plus a fresh scan (issue #367).

`track_id` is now nullable with `ON DELETE SET NULL` — deleting a track **orphans** its history instead of destroying it — and every event carries a snapshot of how to find its track again: `snapshot_hash`, `snapshot_path`, `snapshot_artist`, `snapshot_title`. The snapshot is written at insert time by `insert_play_event`, which is the only moment that information is guaranteed to still exist; by the time a folder is deleted, the row it would have been read from is already gone.

[`reattach_orphaned_play_events`](../../src-tauri/crates/core/src/scanner/upserts.rs) runs after every scan and gives orphans their track back, strongest key first:

1. **`file_hash`** — same bytes, moved or re-added. Exact, but a tag edit rewrites the file through lofty, so the blake3 changes even though the music didn't.
2. **`file_path`** — catches exactly that case: same file, different hash. Fails if the user reorganised their folders.
3. **artist + title** — a re-rip or a different encoding. Loosest, deliberately last: it can't tell a live version from the studio one.

Each step only claims what the previous one left, so a strong match is never overwritten by a weak one, and only `is_available = 1` tracks are matched (attaching to a vanished file would just re-orphan on the next pass).

This gives stats a coherent split, worth knowing before writing a new query: **aggregate totals** (play count, listening time, the monthly histogram) read `play_event` directly and therefore survive a library delete — which is the whole point. **Per-item breakdowns** (top tracks, top artists) join `track` and so exclude orphans, because a row you can't name can't be rendered; they come back when the files are re-scanned.

#### Pool lifecycle across a profile switch

`activate_profile` swaps the active [`ActiveProfile`](../../src-tauri/crates/app/src/state.rs) under the write lock, then closes the previous pool. Closing it *immediately* used to race any command that had already cloned it, surfacing as `PoolClosed` mid-command (issue #332).

The pool is therefore handed out **leased**. `require_profile_pool` / `require_profile_snapshot` return a `ProfilePool` that holds a refcount on the epoch it came from; the close path (`ActiveProfile::close_when_idle`) waits for that count to reach zero before calling `pool.close()`. Because the swap happens first, no new lease can be issued against the outgoing epoch, so the drain always terminates.

Three properties worth keeping in mind when writing commands:

- **The lease releases on drop**, including via `?`. Keep the handle bound for as long as you query — `let _ = state.require_profile_pool().await?;` releases it on the spot.
- **`ProfilePool` derefs to `SqlitePool`**, so it passes anywhere a concrete `&SqlitePool` is expected. sqlx's query methods are generic over `E: Executor` and deref coercion does not fire against a type variable, hence the explicit `&*pool` at query sites.
- **The wait is bounded** by `LEASE_DRAIN_TIMEOUT` (5 s), so the guarantee is time-bounded rather than absolute. A library scan legitimately holds its pool for minutes, and a leaked lease would otherwise wedge profile switching outright — so the timeout degrades to the pre-#332 behaviour (close anyway, race whatever remains) and logs at WARN rather than blocking forever. A command that can outlive the timeout must still tolerate `PoolClosed`; what the lease buys is that ordinary multi-step commands no longer race the close at all.

Holding a lease is not on its own enough for a **batch**: re-resolving the active pool inside the loop reintroduces the same straddle at a different layer, since the work list came from one profile and the remaining writes would land in whichever profile is active by then. Read the list and do the work against the same pool — [`enrich_artist_deezer_with_pool`](../../src-tauri/crates/app/src/commands/deezer.rs) exists for exactly that reason.

To give an owned pool to a `waveflow-core` type that knows nothing about leases, split it with `into_parts()` and park the lease alongside the value in `state::Leased<T>` — see the repository helpers in [`commands/library.rs`](../../src-tauri/crates/app/src/commands/library.rs) and [`commands/playlist.rs`](../../src-tauri/crates/app/src/commands/playlist.rs).

`into_unleashed()` deliberately opts out, for handles a worker holds for the life of the process rather than for the span of a command. Its only caller is the DLNA server: leasing there would stall every profile switch for the drain timeout without making the worker any more correct, because it does not re-resolve its pool on switch at all — a running server keeps serving the profile it was started with, and its pool is closed underneath it. That gap predates the lease work and is tracked in issue #399.

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
