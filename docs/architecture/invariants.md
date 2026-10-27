# Cross-cutting invariants

The contract the rest of the codebase is built on. [`CLAUDE.md`](../../CLAUDE.md) carries the one-line version of each rule; this page is the long form — the _why_, the failure mode it prevents, and the exceptions.

Rules are grouped by layer: [IPC & state](#ipc--state) · [Database](#database) · [Audio](#audio) · [Frontend](#frontend) · [Network & sync](#network--sync) · [Plugins](#plugins).

---

## IPC & state

### Tauri commands

`#[tauri::command]` lives in `commands/*.rs`, is registered in `lib.rs::generate_handler![]`, and is called from React with `invoke("command_name", { args })`. Frontend uses camelCase, backend snake_case — Tauri converts at the boundary. A command missing from `generate_handler![]` compiles fine and fails at runtime.

### Profile-scoped pool

`state.require_profile_pool().await?` — every command that touches user data goes through this. The shared `app.db` is for the profile list and cross-profile settings (Last.fm key, Discord opt-in, offline mode, backup config).

It returns a **leased** [`ProfilePool`](../../src-tauri/crates/app/src/state.rs), not a bare `SqlitePool` (issue #332): a profile switch drains outstanding leases before closing the old pool, so a multi-step command holding one across awaits can't fail with `PoolClosed` mid-flight. Consequences:

1. `ProfilePool` derefs to `SqlitePool`, but sqlx query methods take a generic `E: Executor` and deref coercion doesn't fire against a type variable — write `.fetch_one(&*pool)`, not `&pool`.
2. **Keep the handle bound** for as long as you query. Binding to `_` releases it immediately.
3. To hand an owned pool to a `waveflow-core` type, use `into_parts()` and park the lease next to it via `state::Leased<T>` — the repo helpers in `commands/{library,playlist}.rs` show the pattern. `into_unleashed()` opts out on purpose and is only for worker-lifetime handles (the DLNA worker).

**The guarantee is time-bounded.** The drain gives up after `LEASE_DRAIN_TIMEOUT` (5 s) and closes anyway, so a command holding a lease longer than that across a switch — a full library scan — can still see `PoolClosed` and must keep tolerating it. What the lease buys is that _ordinary_ multi-step commands no longer race the close at all.

Same reasoning applies to re-resolving the pool inside a loop: a batch that reads its work list from one pool must keep using **that** pool for the whole run (see `enrich_artist_deezer_with_pool`), otherwise a mid-batch profile switch writes the remainder into the other profile.

### Persistence of settings

Per-profile settings live in `profile_setting` (typed key-value); app-wide settings live in `app_setting` with the same shape. Write pattern is `INSERT ... ON CONFLICT DO UPDATE`, naming all four columns: `value_type` and `updated_at` are `NOT NULL` with no default, and SQLite checks them on the row the upsert _would_ insert before it looks for the conflict — so a write that leaves them out fails every time, for a key that already exists as much as for a new one. That is how the DLNA settings went unsaved.

**A React preference hook does NOT hand-roll this** (issue #485). Build it on [`useProfileSetting`](../../src/hooks/useProfileSetting.ts) (or its `useProfileBooleanSetting` shorthand), which owns:

- the optimistic update and the serialized write chain,
- read/write cross-invalidation,
- rollback to the last backend-confirmed value,
- the `ready` gate,
- the profile-switch reset,
- **cross-window propagation** — the write is re-broadcast to every other webview as `profile-setting:changed` and turned back into the window event by [`profileSettingBridge`](../../src/lib/profileSettingBridge.ts), and a hook that mounted before that listener went live re-reads once. `listen()` is asynchronous, so a window is deaf between its first mount and the promise resolving, which is exactly when a window opens (issue #741).

The hand-rolled copies had drifted apart, each carrying a different subset of those guarantees.

**A preference's default is written once, in [`settingsRegistry.ts`](../../src/lib/settingsRegistry.ts).** The hook, the Settings view's initial state and the header that counts the settings differing from their default all read it there; a literal default in a hook is a second copy that drifts from the count. A new preference shown in Settings gets an entry there and an accessor in [`settingsReset.ts`](../../src/components/views/settings/settingsReset.ts) (the accessor map is typed over the registry, so forgetting it does not compile). See [ui.md](../features/ui.md#modified-settings-and-reset).

Converted so far — a sample, not a roster, and deliberately not kept exhaustive: `useVisualizerStyle`, `useEstimatedKaraoke`, `useLyricsLookupSettings`, `useDesktopLyricsStyle`, `useContrastMode`, `useTrackColumns`, `useScrollLongTitles`, `useArtistBioCollapsed`, `useHiddenKpis`, `useCoverSlideshow`, `useVisualizerColor`, `useArtistHero`, `useWrappedBannerVisibility` (two keys ⇒ two instances, each on its **own** broadcast channel — sharing one would make a write to either key re-read the other and clobber its in-flight optimistic value).

Still standalone, by shape rather than by oversight: `useWebRadioFavorites` (persists through the plugin-favorites commands, not `profile_setting`), `useHiResBadgeVisibility` (module-level store + subscription so a non-React consumer can read it), `useSortMemory` (many dynamic keys), [`useProfileScopedToggle`](../../src/hooks/useProfileScopedToggle.ts) (the engine toggles push their value to the audio threads on the way in, so they own their own commands rather than going through `profile_setting`), and the direct `setProfileSetting` calls in `ThemeContext` / `SkinContext` / `shortcuts.ts` / a few Settings cards.

The hook also passes the captured profile id to [`set_profile_setting` / `get_profile_setting`](../../src-tauri/crates/app/src/commands/profile.rs), which validate it through [`AppState::require_profile_pool_for`](../../src-tauri/crates/app/src/state.rs) **under the same lock `switch_profile` takes**. A JS-side "is this still my profile?" check runs before the IPC hop, so only the backend can stop a queued write from landing in the profile the user just switched to. Passing `None` opts out and targets whatever is active — correct for a fresh user action, wrong for anything that awaited first.

### Events

The backend emits Tauri events; the frontend listens via `listen()` from `@tauri-apps/api/event`:

`player:state` · `player:position` · `player:track-changed` · `player:queue-changed` · `player:options-changed` · `player:volume-changed` · `player:error` · `player:ab-loop` · `player:spectrum` · `track:updated` · `track:liked-changed` · `artist:updated` · `profile-setting:changed` · `library:rescanned` · `scan:progress` · `lyrics:updated` · …

**The playback position is not context state.** `player:position` ticks four times a second; held in `PlayerContext`, each tick re-rendered every `usePlayer()` consumer (38 of them, the library view included) to redraw the handful that show it. It lives in a small external store instead, [`usePlayerPosition`](../../src/hooks/usePlayerPosition.ts): a component that renders the position calls `usePlayerPosition()`, and code that only needs it in a handler calls `getPlayerPosition()` and does not re-render at all. `PlayerProvider` is the store's only writer. Putting `positionMs` back in the context value brings back the 4 Hz re-render of the whole tree.

**Shared state needs an event, because there is more than one window.** The mini-player is a second webview with its own provider tree, so anything a user can change from both places has to be broadcast or the two copies drift — the engine stays right, the two UIs disagree, and the user "fixes" the one that looks wrong and breaks the one that was. `player:options-changed` (repeat + shuffle), `player:volume-changed` and `track:liked-changed` all exist for that reason, and are emitted by the in-app commands too, not only by external surfaces like MPD (#523).

The window that made the change has already updated optimistically, so it receives an echo of what it holds — a no-op. With one exception: volume is pushed through a 60 ms debounce, so a returning value can be one the user has already dragged past. That window therefore ignores `player:volume-changed` for 500 ms after its own last change. Discrete toggles need no such guard.

**Subscribe first, then snapshot.** When a listener keeps a set in sync and a command loads its initial value, the two must be ordered, not fired in parallel: `listen()` is a round-trip and Tauri replays nothing emitted before it resolves, so a change landing in that gap is missed by the event _and_ can be missed by a read that raced the write — the copy stays wrong until something else forces a refetch. [`useLikedChanged`](../../src/hooks/useLikedTracks.ts) returns a readiness flag its callers gate their fetch on; anything committed before the snapshot is in it, anything after arrives as an event. Changes that land between the two are replayed on top of the snapshot rather than overwritten by it.

### Non-frontend control surfaces go through `player_actions`

The tray menu, the OS media keys, the Windows taskbar thumbnail buttons, the system-wide shortcuts and the MPD server all need the same sequence the frontend gets from `commands::player`: advance the queue → `emit_track_changed` → `emit_queue_changed` → hand the track to the decoder.

That sequence was copy-pasted in `lib.rs` and `media_controls.rs` until #471 (the second copy was literally commented "Mirror of `lib.rs::spawn_next`") and the two had already drifted. Forgetting one emit leaves a surface showing the previous track — exactly the bug duplication kept producing. So a new surface calls [`player_actions::{next, previous, play_at_index}`](../../src-tauri/crates/app/src/player_actions.rs) rather than re-deriving it. The same goes for the volume (`player_actions::set_volume`, which emits `player:volume-changed` so every window's slider follows — MPD's `setvol` used to skip that) and for liking the current track (`toggle_like_current`).

They're `async` and await rather than spawn: sync callers (souvlaki, tray, taskbar buttons) wrap in `tauri::async_runtime::spawn` themselves, async ones report the outcome back to their client. `player_actions::toggle_play_pause`, shared by the tray and the taskbar buttons, is sync: it pauses or resumes directly, and only spawns `player_actions::resume_last` from `Idle` / `Ended`. From those states the decoder has no track open and drops `AudioCmd::Resume`, so resuming means loading the persisted resume point — what the in-app Play button does through `player_resume_last`.

`player_actions::play` is the same shape for surfaces with a _separate_ Play button (the OS media overlay, MPD's `play` / `pause 0`): it resumes or loads the resume point and never pauses, so Play on a playing track does nothing instead of pausing it. Sending `AudioCmd::Resume` straight to the engine from those surfaces is what #609 was.

Both funnel into `resume_last`, which holds a one-at-a-time guard (`AudioEngine::begin_resume`, released on every exit path). Two Play events landing together would otherwise each read `Idle`, each await the database, and the second would restart the track the first had just started.

---

## Database

### Anything WaveFlow writes into a library folder goes under `.waveflow/`

`mp4` is in `AUDIO_EXTENSIONS` and [`is_scannable_audio`](../../src-tauri/crates/core/src/scanner/extract.rs) takes it **on sight** (only `ogg` / `oga` pay for a header read), so any clip written into somebody's music folder becomes a track on the next scan. The walk has no other directory filter, and a leading dot is a display convention rather than something a file walker obeys — so the single reserved name in [`scanner::reserved`](../../src-tauri/crates/core/src/scanner/reserved.rs) is what keeps our own files out of the library, and both walkers honour it: `collect_scannable_files` prunes the branch, the fs watcher drops an event whose every path is inside it.

Two rules that are easy to get wrong: the check is **rooted** (a library that itself lives under a directory of that name is still an ordinary library, and testing every component of an absolute path would empty it), and the walk never prunes **its own root** for the same reason. Writing anywhere else in a library folder needs the same treatment or it will be indexed — including by the watcher, which would otherwise answer our own writes with a rescan. Layout: [`storage.md`](storage.md#inside-the-users-library-issue-695).

### Library folders never overlap

No library folder is, lies under or holds another, in any library of the profile. A folder owns the tracks its scan finds (`track.folder_id`), and removing it deletes them. Two folders where one holds the other both claim the files they share: each scan reads them again, the last to run takes them, and removing either can take tracks the other still shows — with their likes, ratings and playlist places.

`add_folder_to_library` refuses an overlap with `AppError::FolderOverlap`, and the drop never registers a folder at all: it copies into the one folder WaveFlow manages ([drag-and-drop import](../features/library.md#drag-and-drop-import)). Compare folders with [`scanner::folders::relation`](../../src-tauri/crates/core/src/scanner/folders.rs), never with a string prefix: `Music2` starts with the characters of `Music` and is not under it, and Windows and macOS ignore case.

### Never `DROP TABLE` a parent table in a migration

The profile pool opens with `foreign_keys = ON` and SQLite's `DROP TABLE` fires an implicit `DELETE`, so `ON DELETE SET NULL` / `CASCADE` on children run — dropping `artwork` blanks every `album.artwork_id`, dropping `artist` empties `track_artist`. `PRAGMA foreign_keys` is a no-op inside the transaction sqlx wraps migrations in, so it cannot be turned off there. Use `ALTER TABLE … ADD COLUMN` (issue #401 took this route for exactly this reason).

A consequence that is easy to miss: a `CHECK` constraint on a parent table is **frozen**. SQLite changes a constraint only by rebuilding the table, which is this `DROP TABLE`. `artwork.source` is one — `IN ('embedded','folder','deezer','manual')` since the initial migration — so its values live in code as the [`ArtworkSource`](../../src-tauri/crates/core/src/scanner/extract.rs) enum, and a test reads the constraint out of the real migration file to hold the two together. A free string there once wrote `'theaudiodb'`, which the database refused on every write while every test, run against hand-typed fixtures, stayed green (#750).

### Migrations are immutable once merged

sqlx records a SHA-384 checksum in `_sqlx_migrations.checksum` at apply time, so editing a merged migration crashes every existing install at boot with `"migration <id> was previously applied but has been modified"`. For any schema evolution, **create a new dated migration** `YYYYMMDDhhmmss_<slug>.sql`. Same rule for `migrations/app/`.

**Line-ending drift is a non-event.** [`db::migration_heal`](../../src-tauri/crates/app/src/db/migration_heal.rs) reconciles stored checksums against the compiled-in migrator before each `Migrator::run`: when the stored hash matches the LF or CRLF variant of the same SQL (the Windows `core.autocrlf=true` regression), it silently rewrites the row to the canonical hash and logs a warning. A real SQL change still panics, because neither LF nor CRLF normalization will rescue it. Importing a profile archive asks the same question of the archive's `_sqlx_migrations` (`normalise_migration_checksums` in [`profile_io`](../../src-tauri/crates/app/src/commands/profile_io.rs), through the same `is_line_ending_drift`): line-ending drift is rewritten, any other difference refuses the import and names the migration — rewriting it would have let a schema this build does not know in under its checksum.

**A database from a newer build is refused, out loud.** Because migrations only ever get appended, a database records the newest build that ever touched it, and an older binary finds a `_sqlx_migrations` row it has no migration for. Refusing is right — replaying a newer schema through older code corrupts it — but the refusal used to leave `AppState::init`, and an error out of the Tauri `setup` hook is a panic: the splash painted, the process died with it, nothing explained (#526). [`db::schema_guard`](../../src-tauri/crates/app/src/db/schema_guard.rs) now detects the case ahead of the migrator, `run` asks it once more before the event loop even starts, and the answer reaches the user as a native dialog and a clean exit (#529 is why that ordering is not cosmetic).

**Two shapes, one accident.** A downgrade lands as an unknown `version` only when the builds do not share the migration id. When they do — a migration amended before release, a beta whose file was edited after the stable cut — sqlx reports `VersionMismatch` on the checksum instead, and that one still panicked out of `setup`. `ensure_no_foreign_migration` covers it, next to `ensure_not_from_the_future` and with the same treatment. The version guard answers first when both apply, because naming how far ahead the database is gives the user more to act on. Line-ending drift is deliberately excluded: [`migration_heal`](../../src-tauri/crates/app/src/db/migration_heal.rs) repairs that a moment later, and refusing on it would turn a Windows checkout quirk into a dead install — both passes share the one `is_line_ending_drift` helper so they cannot drift apart on what counts.

Four rules come out of that, in order:

- **Guard before heal, heal before run.** The guard runs first because the heal pass _writes_ — no rewriting checksums into a database this build has already decided not to touch.
- **The guard must not outlive its reason.** `the_guard_fires_on_exactly_what_sqlx_refuses` pins it to sqlx's own `VersionMissing`; if sqlx ever stops refusing, the guard has started deciding policy on its own and that test says so.
- **Say it before the event loop starts.** `setup` runs from _inside_ the loop (`RuntimeRunEvent::Ready`), so nothing queued from there is dispatched until it returns — and a fatal path never returns. One failure per platform. Two are measured: on Linux `rfd`'s GTK backend hands every dialog to a global thread that iterates the _default_ GLib main context, which `tao` owns on the main thread we would be blocking, so no window is ever created and the process hangs (#529); on Windows `TaskDialogIndirect` creates its window and never shows it (`WS_VISIBLE` stays clear, #528). The third is **inferred, never run** — read out of `rfd` 0.16's sources, macOS should fail twice over, since `run_on_main` dispatches to a main queue nobody is draining and panics outright before `NSApplication` is running. There is no macOS job in CI, so treat it as the reason the design avoids the situation rather than as an observation. What _is_ observed, on all three, is the shape that replaced it: the pre-flight dialog was watched appearing on Fedora 44 (GNOME/Wayland) and on macOS 26.6.1, and on Windows in #528 — each time blocking until dismissed, then exiting 1 with the fatal line already flushed to the log file. So the inference above is about the road not taken, not about whether this works. So the explanation moved to [`preflight`](../../src-tauri/crates/app/src/db/schema_guard.rs), called from `run` before `tauri::Builder` is even constructed — no main context held, no splash to hide. The guard inside `setup` stays as a backstop and may only **exit**, never present. One Linux-only precondition survives: `rfd` cannot report a failed `gtk_init_check` — `run_blocking` just parks on a condvar forever — so the dialog is skipped outright when neither `DISPLAY` nor `WAYLAND_DISPLAY` is set. Text is English like the tray's seed labels: the language preference lives in the database we just refused to open. **A dialog the app goes on after is asked from a child process.** The pre-flight also asks whether to migrate without a pre-migration copy ([`storage.md`](storage.md)), and there the answer may be to carry on — which a dialog shown in-process cannot survive: `rfd`'s GTK thread keeps the default GLib main context for the rest of the process, and `tao` then panics failing to acquire it (`Failed to initialize gtk backend!`, measured on Fedora 44); on macOS an alert shown first would create the `NSApplication` before `tao` registers its own (read in the sources, not run). So [`backup_prompt`](../../src-tauri/crates/app/src/backup_prompt.rs) starts the binary again with `--ask-pre-migration-backup`, which shows the dialog, exits with the answer as its status, and takes the toolkit with it.
- **The pre-flight may not create, and may not refuse on its own doubt.** It opens with `create_if_missing(false)` — materializing `app.db` would turn "no install yet" into "install with an empty database" and the first-run path would never run again — and anything short of a verdict (no database, a file it can't read, a query that fails) proceeds to normal startup, where the real guards get the last word. It vets exactly the two databases startup will open, `app.db` and the profile [`resolve_target_profile`](../../src-tauri/crates/app/src/state.rs) picks, sharing that lookup with `bootstrap` rather than re-deriving it: one profile left behind by a newer build is no reason to ground a launch that won't touch it.

### Single writer to SQLite

WAL mode allows concurrent reads but only one writer.

- Big import paths (`scan_folder_inner`, `edit.rs::update_track_tags`) wrap work in `pool.begin()` + commit every 200 rows.
- Upsert helpers (`upsert_artwork` / `upsert_artist` / `upsert_album` / `upsert_genre`) take `&mut sqlx::SqliteConnection` so they participate in the open transaction — never a pool clone mid-tx.
- The background analyzer ([`commands/analysis.rs::run_analyze_library`](../../src-tauri/crates/app/src/commands/analysis.rs)) is a second writer that MUST stay out of the scanner's way: it parks while [`scan::scan_in_flight()`](../../src-tauri/crates/app/src/commands/scan.rs) is set, batches `track_analysis` writes 16-at-a-time in one transaction, and retries the batch on `SQLITE_BUSY` / `SQLITE_LOCKED` (low-byte `5` / `6`) with backoff so a transient lock never silently drops a decoded result.

Any other long-running background writer follows the same **park-behind-scan + batch + busy-retry** shape.

### Multi-artist queries

The scanner splits a multi-artist tag value on `"; "` **only** (MusicBrainz Picard / foobar2000 / Beets / Mp3Tag convention) into individual `artist` rows linked via `track_artist`.

`", "` is deliberately NOT a separator because a comma can be part of an artist name (`"Tyler, The Creator"`, `"Earth, Wind & Fire"`, `"Crosby, Stills, Nash & Young"`). Libraries that comma-joined their multi-artist fields will see those tracks under the combined-name phantom artist until re-tagged with `"; "` — or dissolved in place via the user-driven **Split this artist** action ([`commands/artist_split.rs::split_artist`](../../src-tauri/crates/app/src/commands/artist_split.rs), issue #396), which re-links the phantom's tracks to the individual artists (reusing existing rows by canonical name) and is kept durable across normal rescans by a scanner skip-branch guard (`splits.len() == 1 && current_count > 1` ⇒ leave the credits alone).

Queries rebuild the display string via `GROUP_CONCAT` over `track_artist` ordered by `position`. `ArtistLink` accepts parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable. **New track queries must follow the same join pattern.**

### Album grouping = `(canonical_title, album_artist_id)`

[`scan.rs::upsert_album`](../../src-tauri/crates/app/src/commands/scan.rs) keys on the album artist (Album Artist tag → `is_compilation` → primary artist fallback). `album.is_compilation` is sticky, and `merge_implicit_compilations` collapses ≥ 3 distinct-artist same-title rows into "Various Artists" after every scan. `edit.rs` re-runs `upsert_album` with the OLD album's Album Artist / compilation flags so renames don't re-split — except for the Album Artist when the edit names a new one, which moves the track to the album that name keys, without the old compilation flag.

The tag decides, and a track never overrides it. When a rescan re-splits a track's credits, [`repoint_fallback_album_artist`](../../src-tauri/crates/core/src/scanner/upserts.rs) moves the album to the new lead artist only if the album took its artist from _that_ track by fallback: no Album Artist, no compilation flag, and `artist_id` still the track's old lead. When another album already holds the target `(canonical_title, artist_id)` — a `UNIQUE` pair, so re-keying would abort the whole scan transaction — the track joins that album instead, where a fresh scan would have put it. The unguarded version used to re-key a Various Artists record or an explicit Album Artist on the first track to be rescanned.

Deep dive: [library § album grouping](../features/library.md#album-grouping).

### File-write safety on Windows

Any command that rewrites an audio file (`edit::update_track_tags`, `save_lyrics`, `set_track_rating`) MUST pause playback first when the engine reports the edited track as `current_track_id` — lofty's `save_to_path` needs an exclusive handle on Windows. Re-hash with blake3 and update `track.file_hash` after the write so the scanner's `(mtime, size)` fast path stays addressable.

### Tag writes go through the concrete tag

**Never write a file by mutating `lofty::read_from_path(...)` and calling `save_to_path` on the `TaggedFile`.** Go through [`edit::patch_file`](../../src-tauri/crates/app/src/commands/edit.rs), which reads the concrete file type (`FlacFile`, `MpegFile`, `Mp4File`, …), splits its tag, applies the edit to the generic half and merges the remainder back.

The reason is that `TaggedFile` holds _generic_ `Tag`s, built by splitting each concrete tag into what has an `ItemKey` mapping plus a remainder — and what happens to that remainder is not uniform. `Id3v2Tag` stashes it in the `Tag`'s companion slot and restores it on write; `VorbisComments` does not (`From<VorbisComments> for Tag` is `split_tag().1`), so **every non-standard comment on a FLAC / Ogg / Opus / Speex file is dropped by the round trip** — our own `SYNCEDLYRICS`, `REPLAYGAIN_*` values another tagger left, anything a different player stores. Nothing warns, and no rescan brings it back.

Two rules ride along in the same function:

- **The year goes on the date item**, not `ItemKey::Year`. `Year` has no ID3v2 mapping at all, so writing it there dropped the value silently while the database happily recorded the new year.
- **A cover replaces the front cover, not the artwork.** Clearing `tag.pictures()` to make room for one image also threw away the booklet, the back cover and the artist shot. Only `CoverFront` and the untyped `Other` are removed.

Containers the scanner indexes but nothing can tag are refused up front with a message that says so, instead of failing later as an unrelated "unknown format". That is `.dff` alone now: `.dsf` has a writer of its own (#592), because its tag is a plain ID3v2 block at an offset the header declares rather than something lofty has to understand.

**Every payload, not just the properties dialog.** Star ratings and lyrics saved into the tag each reached the file by their own route for as long as they existed, and each paid for it the same way: no DSD, no remainder, no rewrite safety — the rule above, broken in two places that predated it. They are `TagPatch` variants now (#644), which is the shape any future payload should take rather than a third private route.

Deep dive: [library § tag editing](../features/library.md#tag-editing).

---

## Audio

### The audio callback is hot

The cpal callback (and the WASAPI exclusive thread) MUST NOT allocate, lock, or log. Only `rtrb::Consumer` reads and `Atomic*` loads. All heavy work (EQ, ReplayGain, resampling, FFT, BLAKE3) runs on the decoder thread before samples reach the SPSC ring.

The decoder's last stage is a `[-1.0, 1.0]` safety clamp ([`decoder::clamp_to_unity`](../../src-tauri/crates/app/src/audio/decoder/track.rs)) applied to every buffer right before `push_samples` — identity for an untouched stream (so bit-perfect output is preserved), it only bites when a gain stage overshoots unity and would otherwise hard-clip the DAC.

### Every load command carries its intent

A command that hands the decoder a track — `LoadAndPlay`,
`LoadRemoteFileAndPlay`, `LoadUrlAndPlay` — MUST carry a `LoadIntent`
claimed from `AudioEngine::next_load_intent` **at the moment the intent
starts**: before the profile snapshot, the queue read and the ReplayGain
lookup, not just before the send. The decoder drops any load older than one
it has already been handed.

Taking the token late is the failure mode, not a style question. Every load
path prepares asynchronously first, so a token claimed just before the send
records the order preparations _finished_ — which is exactly the order that
plays the wrong track (#622). The type makes the token compulsory (its field
is private, so no load can be built without one) but it cannot tell whether
you claimed it early enough; that part is on the author of each new load
site.

**And a producer publishes nothing for a load that will be dropped.**
`AudioEngine::claim_dispatch` is a compare-and-set on the same mark the
decoder reads: call it immediately before the first side effect — the
queue cursor, the label, a state change — and abandon the whole dispatch
when it returns `false`. Hold `AudioEngine::lock_publish` across the
claim and everything it authorizes, because the cursor write is an await
and the runtime is multi-threaded: without it, an older producer can be
overtaken mid-write and still land its cursor last. Never hold it across
the preparation that precedes the claim (#632).

**An output rebuild re-dispatches the decoder's own load, under that
load's own intent.** It is the one place a load is sent without a fresh
token, and deliberately so: the rebuild is putting back what its `Stop`
took away, so it has to be accepted when nothing newer has arrived and to
give way when something has. Read what to put back from
`SharedPlayback::last_load` — written by the decoder at receipt, so it
already names the track a user picked mid-rebuild — never from a snapshot
taken before the stop, which is what left nothing playing at all (#634).

Detail, and why `SetNextTrack` is excluded: [ordering the
loads](../features/playback.md#ordering-the-loads). What a rebuild puts
back, and the two sessions it parks instead: [what a rebuild puts
back](../features/playback.md#what-a-rebuild-puts-back).

### One codec registry for the whole app

Anything that opens an audio stream MUST go through
[`waveflow_core::audio_format::opus::codecs()`](../../src-tauri/crates/core/src/audio_format/opus.rs),
never `symphonia::default::get_codecs()`.

There are three such places and they answer three different questions
about the same file: **playback** (`ActiveStream::open`), the
**analysis** pass, and the **scanner**'s probe. They have to agree on
what this build can decode, because the disagreement is not abstract —
a scanner that believes in a codec playback does not have is exactly
how an unplayable track gets into a library, which is the single thing
[`AUDIO_EXTENSIONS`](../features/library.md#scanning) exists to
prevent.

Each of the three used to build its own decoder from symphonia's
default registry, which was harmless only for as long as the app
shipped no decoder of its own. Opus (#581) is one, so it is not
harmless any more.

Two consequences worth stating:

- **Adding a decoder means one registration**, not three, and the
  scanner's extension list plus
  [`undecodable_stream`](../../src-tauri/crates/core/src/scanner/extract.rs)
  have to move with it — those two are the scanner's half of the same
  agreement.
- **The registry lives in `core`**, not in the app, because the
  analysis pass and the scanner are core's and the future
  `waveflow-server` transcoder will want the same answer.

Wider topology: [audio architecture](audio.md).

---

## Frontend

### Virtual scroll everywhere

`TrackTable` uses `@tanstack/react-virtual` for 6000+ track performance. Virtualized tables consume `usePageScroll()` for the scroll element instead of nesting their own `overflow-y-auto` — that drives a single Spotify-style page scrollbar.

**Where the line is.** "Everywhere" means every list whose length has no natural bound: the library's tracks, albums, artists and playlist grids, `PlaylistView` — a playlist can hold six thousand tracks because someone put them there — the liked tracks, the play history, and the track tables of a genre, a year and an artist. Only a list its entity bounds by nature renders directly: `AlbumDetailView`, because an album is a dozen or two tracks. The line used to be drawn at "a detail view of one entity", which left `GenreDetailView`, `YearDetailView` and `ArtistDetailView` mapping every row, though a genre can be a third of the library and a prolific artist runs to hundreds of tracks. The machinery a page-driven list needs — the scroll element from `usePageScroll()`, a `scrollMargin` re-measured against the page scroller whenever the list, the scroller or the content above it changes size — is one hook, [`usePageVirtualizer`](../../src/hooks/usePageVirtualizer.ts), so a new list takes it rather than copying it (the library tables and `PlaylistView` predate it and still carry their own copy). A grouped list keeps its groups whole: the history virtualizes **days**, each a measured `<section>` whose sticky header still docks under the TopBar and is pushed off by the next day's, and a day is bounded by how much one person listens in one.

The **sidebar** is the deliberate exception on the other side: it is a fixed column with its own scroller, so it neither uses `usePageScroll()` nor virtualizes. Pointing a virtualizer at its nested `overflow-y-auto` would introduce, inside the one component that legitimately has one, exactly the pattern this invariant exists to keep out of the page.

### Modal accessibility

Every modal calls [`useModalA11y(isOpen, onClose)`](../../src/hooks/useModalA11y.ts) — Escape-close, Tab focus trap, focus restoration. The container gets `role="dialog"` + `aria-modal="true"` + `aria-labelledby` (stable heading id) or `aria-label` (conditional heading). Don't roll bespoke `useEffect` Escape handlers.

Modals nest — the immersive view is one, and the Canvas picker or the lyrics excerpt dialog opens over it. Every instance listens on `document`, where `stopPropagation` cannot keep the others from hearing the same key, so the hook keeps a stack of the open ones and only the top one answers Escape and traps Tab: Escape closes the dialog on top, not everything beneath it.

### Overlays must be portalled, not just given a big `z-index`

Issue #390. A `z-index` is only compared inside its stacking context, and `position: fixed` escapes layout flow but NOT that context. WaveFlow's chrome is glassy — the TopBar carries `backdrop-blur-md`, and the Pulse / Liquid skins add `backdrop-filter` to the PlayerBar and other containers — and **any** `backdrop-filter` / `transform` / `opacity < 1` / `filter` / `will-change` ancestor creates a context that silently caps everything inside it. So a context menu at `z-100` nested under such an ancestor still paints _under_ the PlayerBar.

Anything at z-100+ — context menus, dropdown popovers, **and any dialog that paints its own `fixed inset-0` scrim instead of going through `AnimatedModalShell`** — renders through `createPortal(…, document.body)` — see [`ContextMenu`](../../src/components/common/ContextMenu.tsx) and [`AnimatedModalShell`](../../src/components/common/AnimatedModalShell.tsx). Portalling is safe for skins because every skin rule is rooted at `:root[data-skin="…"] :where(…)` and `body` stays a descendant of `:root`.

The documented layer scale (in-panel sticky `z-10` → content sticky headers `z-20` → TopBar `z-30` → PlayerBar `z-50` → overlays `z-100`/`z-101`) lives at the top of [`src/app.css`](../../src/app.css) — extend it there, don't invent a bigger number locally.

### A blurred cover is blurred once, not once per frame

A CSS `filter: blur()` is recomputed every time the region it sits in repaints, and anything animating nearby repaints it every frame: a progress line, a wave, a pulsing dot, the lyrics. With a GPU that goes unnoticed. Without one — WebKitGTK's software renderer, where the renderer fallback (#595) and virtual machines put the app — it costs the whole frame: measured with [`scripts/bench/webkitgtk`](../../scripts/bench/webkitgtk/README.md), the Fluid Home drew one frame every 620 ms, and 48 frames a second with its CSS filters switched off; Studio's Home held 12 until either its animation or its blurred glows went.

So a blurred cover goes through [`PrecomputedBlur`](../../src/components/common/PrecomputedBlur.tsx): the image is drawn once into a small canvas (at most 192 px wide) sized against its box, blurred there, and the canvas stretched over the box, redrawn only when the box changes size; the props are the values the CSS filter would have said (`blur` in CSS pixels, `saturate`, `brightness`, `scale`). The drawing ([`lib/precomputedBlur.ts`](../../src/lib/precomputedBlur.ts)) blurs the **pixels** itself first — darkening and saturation per pixel, then three box passes — because the 2D context's `filter` does not exist on WebKitGTK 2.54 or macOS WebKit, the engines the CSS blur hurt; the image is loaded with `crossOrigin="anonymous"` so its pixels can be read, as the cover accent mode reads them. Only when they cannot be (a remote cover served without CORS) does it fall back to `ctx.filter`, and without that to shrinking the image to a few pixels and stretching it back. It never renders a CSS blur. The canvas aura and the suggestion band of the fluid Home, the immersive view's backdrop, the classic suggestion card and Lounge's room use it. A soft glow that is not an image is a gradient, not a blurred shape — the classic Home's banners draw their glows as `.home-orb` radial gradients, and the empty states' and the profile picker's breathing glows as `.soft-glow` ones with the blurred disc's own profile. A CSS `filter: blur()` stays acceptable only on something small that never repaints.

### Right panels are flex siblings, not overlays

`NowPlayingPanel` / `QueuePanel` / `LyricsPanel` are mounted as flex children of the outer row in `AppLayout`. The center column has `min-w-0` so wide tables collapse instead of pushing the panel off-screen.

### Adding a new player-bar action

Default it into the overflow ("⋯") menu via [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) first; promote to primary only when usage warrants it; add a Settings pin toggle if both modes make sense. See [UI § player-bar layout](../features/ui.md#player-bar-layout).

---

## Network & sync

### Process-wide offline mode

Every outbound HTTP path (Deezer, Last.fm, similar, LRCLIB, the plugin registry) checks `offline::is_offline()` first and short-circuits to an empty payload or the cache. Persisted in `app_setting['network.offline_mode']`. **Treat new HTTP code paths the same way.**

### No secret reaches the log

The log file is what a user attaches to a bug report, so nothing in it may let a reader act as them. A URL is logged through [`redact_url`](../../src-tauri/crates/app/src/audio/http_source.rs) — which drops userinfo, query and fragment, and masks the bearer ticket a remote server's stream URL carries in its path (`/api/v2/stream/<ticket>`) — and a `reqwest::Error` through [`describe_http_error`](../../src-tauri/crates/core/src/error.rs) before it is formatted, because reqwest prints the full request URL in both `Display` and `Debug`. That holds for an error that is only _returned_, too: every caller logs what it gets, and some show it in the UI. The metadata providers keep the error itself, stripped with `without_url()` ([`metadata::redact`](../../src-tauri/crates/core/src/metadata/mod.rs)), and render it through the same function.

`describe_http_error` is not `without_url()` plus `Display`: that reads only "error sending request", and the reason the request failed — the refused connection, the DNS failure, the timeout, the TLS handshake — lives in the error's `source()` chain. It takes the URL off the first layer, walks the chain ([`describe_error`](../../src-tauri/crates/core/src/error.rs), depth-capped, a layer restating its parent skipped) and cuts any URL a deeper layer prints anyway — a redirect target in a policy's refusal — down to its origin. Rendering a `reqwest::Error` any other way loses either the secret's protection or the cause; a lyrics provider whose URLs hold a token keeps the stripped error as its variant's `source`, and the caller walks it with `describe_error`. A `reqwest::Error` of another major version, wrapped by a dependency (the updater plugin's), cannot be named here: it goes through `describe_error_without_urls`, the same walk with every URL in it cut to its origin.

### A server identifier is one path segment

Every identifier the server supplies — track, album, artist, library, playlist, share, upload session — reaches a REST path through [`remote::client::segment`](../../src-tauri/crates/app/src/remote/client.rs), never formatted in raw. The request carries the user's bearer token, and an ID holding `/`, `?` or `#` would move it to another route of the same host, or into its query; a `..` would climb out of the resource. `segment` percent-encodes every byte outside RFC 3986's unreserved set, so the server's router decodes back exactly the ID it sent, and refuses the empty ID and the dot segments, which the URL parser resolves away whatever their encoding (`%2e` is a dot to it too). A refusal is permanent for the drain; a lookup that has a fallback (lyrics, Canvas, the metadata backfill) takes it as it would a 404, and the rest return an error. The artwork hash is the exception that needs none: it is checked to be hex before it is used at all.

### Remote user data never lands in the local tables

[RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md). Synchronized state describes the **server's** playlists, favourites, ratings, history, queue and shares, and those reference the **server's** tracks — which have no local counterpart. Writing them into `playlist` / `liked_track` / `track.rating` would leave two options, both wrong: fabricate local track rows for content that only exists on the server, or silently drop every entry. The first corrupts the library, the second makes sync look broken while reporting success.

The projection therefore lives in its own `remote_*` tables and is **reconstructible**: dropping it and re-fetching `GET /api/v2/sync/snapshot` is always a valid recovery, and is what the apply path does when it meets a known event it cannot apply. `remote_mutation` is the one exception — it holds writes the server has not seen yet, so it must survive a projection reset.

Matching a local file to a server track is deliberately out of scope and needs its own RFC.

**Two RFCs are numbered 003.** The desktop's [RFC-003](../rfcs/RFC-003-sync-architecture.md) (hybrid logical clocks, superseded) has nothing to do with the server's RFC-003 (sync v2, accepted). Any instruction naming "RFC-003" must name the repository too, or it will be read as the wrong document. On the desktop side the accepted design is **RFC-005**.

### The v1 sync protocol was removed

`crate::sync` is now a permanent no-op stub. The peer-to-peer v1 protocol — ops journal, logical clocks (`lamport` / `hlc`), digest/backfill reconciliation, per-op `snapshots` maps, and the scanner's track emit — was deleted in the RFC-005 cutover once the v2 snapshot bootstrap was proven end-to-end. See [RFC-005 §Decision 8](../rfcs/RFC-005-remote-source-and-sync-v2.md#decision-8--v2-landed-beside-v1-then-replaced-it).

The ~70 CRUD emit call sites still call `crate::sync::*`, but those now resolve to the stub's no-ops and nothing is enqueued. **New CRUD commands do not need to emit anything** — the remote source ([`crate::remote`](../../src-tauri/crates/app/src/remote/mod.rs), RFC-005) never touches local-entity CRUD. The HLC migration columns stay (immutable once merged) but are unused.

---

## Long-running work

### A long operation announces itself to the task registry

Anything that can run for more than a moment — a scan, an analysis sweep, a lyrics prefetch, a catalogue mirror, a reconciliation, an upload, a backup, a thumbnail pass — takes a [`TaskHandle`](../../src-tauri/crates/app/src/tasks.rs) so the status bar above the player can say what the machine is busy with. Before this there were three separate progress surfaces and several tasks that reported nowhere at all, which is why "why is this busy" had no answer.

Four rules, each of which is a visible defect when broken:

- **The handle is held for the whole operation, and dropped by scope.** `Drop` retires the row on every exit path, `?` and panic included. A task that leaves a ghost row behind is worse than one that reports nothing: the user gets a spinner that never resolves and a stop button that does nothing.
- **The registry does not implement cancellation — it routes to it.** Five of these operations already had a `cancel_*` command, each with its own safe stopping point: a reconciliation may only stop between batches, a mirror walk between albums, an upload after the current track. A task registers a **callback**; the safe stopping point stays where it is defined. Every long operation in the app turned out to have one once it was looked at — down to the thumbnail pass, which stops between two files — so `Cancellation` currently has no "cannot be stopped" variant. If a task ever genuinely has none, that variant belongs there and the status bar already renders a row without a button.
- **Cancelling must leave consistent state, and "between two items" is not always enough.** The library scan is the worked example: its missing-file sweep marks everything the walk did not reach as `is_available = 0`, so a scan stopped halfway would mark a working library unavailable _because the user pressed stop_. Its safe point is not between two files — it is **before that pass**, which is why `ScanSummary::cancelled` gates it.
- **Progress is throttled in the registry, not at the call site.** A scan ticks every 25 files; `tasks:changed` goes out at most every 250 ms, except for start, finish and a counter reaching its total, which are immediate so a finished task never leaves a half-drawn bar on screen.

The scan's own toast now shows only the _outcome_ — "412 added, 3 errors" — and leaves live progress to the bar: two surfaces counting the same files is the inconsistency this was written to remove, but a task row vanishes when its task ends, so the summary needs somewhere else to live.

The frontend subscribes to `tasks:changed` **before** calling `list_tasks` — see [Events](#events); Tauri replays nothing to a listener that registered a moment too late.

---

## Plugins

Full surface — worlds, store, options, security model — in [features/plugins.md](../features/plugins.md). The invariants that bite:

- **Plugins load at runtime; distribution is a separate repo.** The wasmtime host ([`waveflow_core::plugin::runtime`](../../src-tauri/crates/core/src/plugin/runtime.rs)) loads WASM components from `<app-data>/waveflow/plugins/` (writable sideload) and `<resource>/plugins/` (installer-bundled, re-seeded at boot). The catalogue and each plugin live in **separate repos** ([`InstaZDLL/waveflow-plugins`](https://github.com/InstaZDLL/waveflow-plugins) + per-plugin repos) — never in this one — so grey-area plugins carry no liability for the signed core and a takedown is one registry commit.
- **The registry is the trusted pin, not the release.** [`commands/plugin_store.rs`](../../src-tauri/crates/app/src/commands/plugin_store.rs) verifies `plugin.wasm`'s blake3 against the registry entry before installing, so a compromised GitHub release fails the hash.
- **Every registry fetch honours [`offline::is_offline()`](../../src-tauri/crates/app/src/offline.rs).**
- **A published manifest string must ship its translations in the sibling `*_i18n` field**, never as an inline `{ lang -> text }` map — an older host type-errors on the table and drops the plugin, and for `registry.json` it fails the whole catalogue decode on that build, so the store goes dark. `min_app_version` can't rescue it because it's read _after_ the decode. See [plugins § localized manifest strings](../features/plugins.md#localized-manifest-strings).
- **The localized fallback chain is implemented twice** — `LocalizedString::resolve` (Rust) and [`resolveLocalizedText`](../../src/lib/localizedText.ts) (TS). Change them together.
- **A `ui`-world plugin never gets raw library access.** `waveflow:host/library.list-artists` is a redacted read (names + aggregate track count + opaque id only — no file paths, no per-track rows), gated by the manifest permission `library.read_artists`, clamped by [`MAX_LIBRARY_ARTISTS`](../../src-tauri/crates/core/src/plugin/host_impl.rs), and snapshot-injected so the guest never touches SQLite. The redaction is host-enforced: a plugin without the permission gets `Err("permission denied: library.read_artists")` even with the snapshot present.
- **`ui` plugins return a JSON view descriptor, never HTML/JS/React** — the host draws it with native components, so there is no code-injection surface. A trapping `manifest()` is skipped and logged, never blanks the nav.
- **`canvas`-world fan-out is fail-soft.** [`fetch_track_canvas`](../../src-tauri/crates/app/src/commands/canvas.rs) applies a per-plugin lock + 20 s timeout and runs every URL through the shared `motion_cache::is_safe_motion_url` SSRF guard; any plugin error / panic / timeout is logged and skipped so playback never breaks — the frontend falls down the backdrop precedence chain.
- **A plugin that cannot load says so, instead of going quiet.** The host runs WASM **components**; a core module is valid wasm in the wrong format, and before the gate existed such a plugin installed cleanly and then did nothing, with only a `warn!` to show for it. It reaches a release more easily than it sounds: the two that did were built with the right command and packaged with the wrong file. [`plugin::binfmt`](../../src-tauri/crates/core/src/plugin/binfmt.rs) tells the two apart from the preamble; the store refuses a non-component at install, `load_plugin` records why a load failed, and the plugin list renders it as `PluginInfo.failure`. Log and surface these errors through `detail()`, never `%err` — wasmtime's outermost message says "failed to parse WebAssembly module" and the layer that names the cause is below it. See [plugins § when a plugin cannot run](../features/plugins.md#when-a-plugin-cannot-run).
- **Plugin option values live in `<state_dir>/.plugin-config.json`** ([`plugin_config`](../../src-tauri/crates/core/src/plugin/plugin_config.rs)), the single source of truth — no `app_setting` row, excluded from the scratch quota. They reach the guest read-only through `waveflow:host/config.get-option`, pinned at instantiate time. The import is additive: a plugin built without it still instantiates.
