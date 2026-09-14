# Database & paths

## On-disk layout

```bash
<app_data_dir>/waveflow/                 # AppPaths::root
├── app.db                       (global registry + app settings)
├── avatars/                     (shared profile avatars, blake3-hash-addressed)
└── profiles/
    └── <profile_id>/
        ├── data.db              (per-profile database)
        ├── motion/              (motion covers the user picked, never evicted)
        ├── canvas/              (Canvas clips the user picked, never evicted)
        └── remote-downloads/    (offline copies the user asked for)

<cache_root>/                            # AppPaths::cache_root — `root` unless moved
├── metadata_artwork/            (shared remote artwork cache, blake3-hash-addressed)
├── motion_cache/                (shared animated-cover LRU)
├── canvas_cache/                (shared per-track Canvas LRU)
└── profiles/
    └── <profile_id>/
        ├── artwork/             (per-profile embedded artwork cache)
        ├── remote-artwork/      (per-profile remote cover cache)
        └── remote-stream/       (per-profile remote audio cache)
```

`<app_data_dir>` resolves via Tauri's `app_data_dir()`, which honours the bundle identifier (`app.waveflow`):

- Windows: `%APPDATA%\app.waveflow\waveflow\`
- macOS: `~/Library/Application Support/app.waveflow/waveflow/`
- Linux: `~/.local/share/app.waveflow/waveflow/`

The inner `waveflow/` segment is a hardcoded subdirectory in [`paths.rs`](../../src-tauri/crates/app/src/paths.rs). Don't rename it — existing user libraries point at it. The product display name is `WaveFlow` ([`tauri.conf.json`](../../src-tauri/crates/app/tauri.conf.json)) but the path stays lowercase for backwards compatibility.

## Moving the caches (#619)

Artwork landed on the system drive whatever drive WaveFlow was installed on, and a `C:` that is critically low on space takes the machine down with it. Settings → Data can point `cache_root` elsewhere; the choice lives in `app_setting['storage.cache_root']`, app-wide.

**The split is not "big things move" — it is *evictable moves, chosen stays*.** Everything under `cache_root` is content-addressed or LRU-evicted, so the worst case of a failed move, a missing drive or a half-copy is a re-fetch. Databases, hand-picked motion covers and Canvas clips, and offline downloads would have to be recreated by hand, so moving them would be a migration rather than a setting — that is the heavier option the issue offers, and deliberately not the one taken.

Four things follow from that, each of which is a silent failure if skipped:

- **The move copies, persists, restarts, and only then deletes.** `AppState` hands out a plain `AppPaths` captured at boot and ~95 call sites read it directly, so the running process cannot adopt a new root; the old tree is removed by a startup pass (`cleanup_moved_caches`) once a fresh process is reading the new one. Power loss between any two steps leaves a whole copy on disk and a setting naming a whole copy.
- **The asset scope has to be widened at runtime.** See below.
- **A missing drive falls back without forgetting.** `resolve_cache_root` drops to the default location for that session and leaves the stored choice alone, so plugging the drive back in is enough. The Settings card says so: caches reappearing at the default location is, from the user's side, indistinguishable from them having been wiped.
- **`reset_app` has to wipe both roots.** It removes `AppPaths::root`, which stopped being the whole story here; `wipe_targets_outside_root` covers the difference.

The startup order matters and is easy to get backwards: `app.db` lives at the *default* root, so it is opened first, and only then is the cache root read out of it. `ensure_dirs` therefore runs after that read — running it before would create the cache tree at the default location a moment before learning it belongs somewhere else.

## Two databases

### `app.db` (global)

- `profile` — profile list (one row per profile).
- `app_setting` — typed key/value: `app.last_profile_id`, `lastfm_api_key`, `lastfm_session_key`, `app.theme`, `integrations.discord_rpc`, …
- `deezer_artist` / `deezer_album` — shared metadata cache (Deezer enrichment + Last.fm bios), 30-day TTL via `expires_at`.
- `lyrics` — shared LRCLIB cache (no TTL).

Migrations: [`src-tauri/migrations/app/`](../../src-tauri/migrations/app).

### `data.db` (per-profile)

- Library: `library`, `library_folder`, `track` (which also carries the ReplayGain the file's own tags declare, in `rg_track_gain_db` / `rg_track_peak` / `rg_album_gain_db` / `rg_album_peak` — a property of the file, refreshed by every scan, as opposed to what `track_analysis` measured), `artist`, `album`, `genre`, `track_artist`, `track_genre`, `artwork`, `track_analysis`, `playlist`, `playlist_track`, `liked_track`, `queue_item`, `play_event`, `scrobble_queue`, `profile_setting`, `track_fts` (FTS5, **trigram**-tokenised and content-owning since #579 — see [library.md](../features/library.md#search) for why neither is optional).
- Remote covers (`sync_v2`): `profiles/<id>/remote-artwork/`, an evictable disk cache of the server's hash-addressed cover art. Unlike `motion/` and `canvas/`, nothing here was chosen by the user — every file is a reproducible download ([RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md#cover-art-is-cached-on-disk-not-inlined)).
- Remote source (`sync_v2`): `remote_binding`, `remote_playlist`, `remote_playlist_track`, `remote_favorite`, `remote_rating`, `remote_history`, `remote_queue`, `remote_queue_track`, `remote_share`, `remote_share_track`, `remote_track`, `remote_album`, `remote_library`, `remote_mutation`, `remote_track_link`. All derived from the server and droppable — dropping them and re-fetching a snapshot is always a valid recovery. `remote_track.in_catalogue` marks the rows the catalogue walk owns, so purging the mirror cannot take a playlist's titles with it ([RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md#the-catalogue-mirror)).
- Profile-scoped pool: every command that touches user data goes through `state.require_profile_pool().await?`.

Migrations: [`src-tauri/migrations/profile/`](../../src-tauri/migrations/profile). Applied via `sqlx::migrate!()` at boot for each opened pool.

#### Listening history outlives its tracks

`play_event.track_id` used to be `NOT NULL … ON DELETE CASCADE`, so removing a folder (`DELETE FROM track WHERE folder_id = ?`) or a library silently erased the matching history. One beta tester lost their stats five times that way, and no backup helps: the archive restores the _old_ library, not the history plus a fresh scan (issue #367).

`track_id` is now nullable with `ON DELETE SET NULL` — deleting a track **orphans** its history instead of destroying it — and every event carries a snapshot of how to find its track again: `snapshot_hash`, `snapshot_path`, `snapshot_artist`, `snapshot_title`. The snapshot is written at insert time by `insert_play_event`, which is the only moment that information is guaranteed to still exist; by the time a folder is deleted, the row it would have been read from is already gone.

[`reattach_orphaned_play_events`](../../src-tauri/crates/core/src/scanner/upserts.rs) runs after every scan and gives orphans their track back, strongest key first:

1. **`file_hash`** — same bytes, moved or re-added. Exact, but a tag edit rewrites the file through lofty, so the blake3 changes even though the music didn't.
2. **`file_path`** — catches exactly that case: same file, different hash. Fails if the user reorganised their folders.
3. **artist + title** — a re-rip or a different encoding. Loosest, deliberately last: it can't tell a live version from the studio one.

Each step only claims what the previous one left, so a strong match is never overwritten by a weak one, and only `is_available = 1` tracks are matched (attaching to a vanished file would just re-orphan on the next pass).

This gives stats a coherent split, worth knowing before writing a new query: **aggregate totals** (play count, listening time, the monthly histogram) read `play_event` directly and therefore survive a library delete — which is the whole point. **Per-item breakdowns** (top tracks, top artists) join `track` and so exclude orphans, because a row you can't name can't be rendered; they come back when the files are re-scanned.

#### Cover provenance lives on the album, not the artwork row

`artwork` rows are deduped on the content hash alone and `source` is only written on INSERT, so that column records **whoever put those bytes in the library first** — not where a given album got its cover. An image embedded in one album's tags and shipped as a `cover.jpg` beside another is a single row labelled `embedded`, which used to make the sidecar album look untouchable to [`refresh_folder_covers`](../../src-tauri/crates/core/src/scanner/upserts.rs) and freeze its cover permanently (issue #401).

Provenance is a property of the **link**, not of the bytes, so `album.artwork_source` carries it. Every site that writes `album.artwork_id` writes it too — the scanner, the tag editor, Deezer enrichment, manual upload, and the sidecar pass. `artwork.source` stays as a rough origin label for the bytes; do not read it to decide what may be overwritten.

`artist` deliberately has no such column: its guard is `artwork_id IS NULL` ([`link_local_artist_image`](../../src-tauri/crates/core/src/scanner/upserts.rs)), which never consults a source.

> **Never `DROP TABLE` a parent in a migration.** Widening `artwork`'s uniqueness to `(hash, source)` was the other candidate fix and needs the create-copy-drop-rename rebuild. The profile pool opens connections with `foreign_keys = ON`, and SQLite's `DROP TABLE` performs an implicit `DELETE` that **fires foreign-key actions** — verified against a real database, inside a transaction and out: rebuilding `artwork` blanks `album.artwork_id` and `artist.artwork_id` across the whole library, and `PRAGMA foreign_keys` cannot be toggled from inside the transaction sqlx wraps migrations in. Prefer `ALTER TABLE … ADD COLUMN`.

#### Pool lifecycle across a profile switch

`activate_profile` swaps the active [`ActiveProfile`](../../src-tauri/crates/app/src/state.rs) under the write lock, then closes the previous pool. Closing it _immediately_ used to race any command that had already cloned it, surfacing as `PoolClosed` mid-command (issue #332).

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
- **Downgrades are refused, not survived.** Append-only means a database names the newest build that ever opened it, so an older binary always finds a `_sqlx_migrations` row it has no migration for. [`db::schema_guard`](../../src-tauri/crates/app/src/db/schema_guard.rs) catches that before the migrator runs — and before the checksum heal pass writes anything — so startup can show a dialog and exit instead of panicking out of the Tauri `setup` hook (#526). [`preflight`](../../src-tauri/crates/app/src/db/schema_guard.rs) asks the same question from `run`, before the event loop exists — inside `setup` a native dialog deadlocks on Linux instead of appearing, measured on Fedora 44, and macOS is expected to fail the same way — read out of `rfd`'s sources, never run (#529), so `setup` keeps the guard and exits, while the sentence the user reads comes from the earlier pass. It applies to both databases and to every path that opens one, so importing a profile archive exported by a newer build says so too, rather than failing on a checksum. A build that ships *different SQL under the same migration id* is the other half of the same accident — sqlx calls it `VersionMismatch` — and `ensure_no_foreign_migration` gives it the same dialog, minus anything the heal pass is about to fix on its own.

## Asset protocol scope

Files under `metadata_artwork/`, `avatars/` and `profiles/<id>/artwork/` are served to the renderer via Tauri's asset protocol (`tauri.conf.json::app.security.assetProtocol`). Frontend code uses [`convertFileSrc()`](https://tauri.app/v2/api/js/core#convertfilesrc) to map an absolute path to an `asset://` URL the `<img>` tag can load.

Smart-playlist covers reuse `metadata_artwork/` (no extra scope needed).

**The declared scope is static, and a moved cache root matches none of it.** `tauri.conf.json` lists `$APPDATA/…` and `$APPLOCALDATA/…` patterns only, so artwork on another drive fails to load with no error and no console message — just an image that never appears. `commands::storage::grant_asset_scope` widens the scope at startup through `asset_protocol_scope().allow_directory(…)`. It runs on **every** launch: a scope grant lives in the process, not on disk.
