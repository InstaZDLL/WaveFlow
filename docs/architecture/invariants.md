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

Per-profile settings live in `profile_setting` (typed key-value); app-wide settings live in `app_setting` with the same shape. Write pattern is `INSERT ... ON CONFLICT DO UPDATE`.

**A React preference hook does NOT hand-roll this** (issue #485). Build it on [`useProfileSetting`](../../src/hooks/useProfileSetting.ts) (or its `useProfileBooleanSetting` shorthand), which owns:

- the optimistic update and the serialized write chain,
- read/write cross-invalidation,
- rollback to the last backend-confirmed value,
- the `ready` gate,
- the profile-switch reset.

The hand-rolled copies had drifted apart, each carrying a different subset of those guarantees.

Converted so far: `useScrollLongTitles`, `useArtistBioCollapsed`, `useHiddenKpis`, `useCoverSlideshow`, `useVisualizerColor`, `useArtistHero`, `useWrappedBannerVisibility` (two keys ⇒ two instances, each on its **own** broadcast channel — sharing one would make a write to either key re-read the other and clobber its in-flight optimistic value).

Still standalone, by shape rather than by oversight: `useWebRadioFavorites` (persists through the plugin-favorites commands, not `profile_setting`), `useHiResBadgeVisibility` (module-level store + subscription so a non-React consumer can read it), `useSortMemory` (many dynamic keys), and the direct `setProfileSetting` calls in `ThemeContext` / `SkinContext` / `shortcuts.ts` / a few Settings cards.

The hook also passes the captured profile id to [`set_profile_setting` / `get_profile_setting`](../../src-tauri/crates/app/src/commands/profile.rs), which validate it through [`AppState::require_profile_pool_for`](../../src-tauri/crates/app/src/state.rs) **under the same lock `switch_profile` takes**. A JS-side "is this still my profile?" check runs before the IPC hop, so only the backend can stop a queued write from landing in the profile the user just switched to. Passing `None` opts out and targets whatever is active — correct for a fresh user action, wrong for anything that awaited first.

### Events

The backend emits Tauri events; the frontend listens via `listen()` from `@tauri-apps/api/event`:

`player:state` · `player:position` · `player:track-changed` · `player:queue-changed` · `player:options-changed` · `player:volume-changed` · `player:error` · `player:ab-loop` · `player:spectrum` · `track:updated` · `track:liked-changed` · `library:rescanned` · `scan:progress` · `lyrics:updated` · …

**Shared state needs an event, because there is more than one window.** The mini-player is a second webview with its own provider tree, so anything a user can change from both places has to be broadcast or the two copies drift — the engine stays right, the two UIs disagree, and the user "fixes" the one that looks wrong and breaks the one that was. `player:options-changed` (repeat + shuffle), `player:volume-changed` and `track:liked-changed` all exist for that reason, and are emitted by the in-app commands too, not only by external surfaces like MPD (#523).

The window that made the change has already updated optimistically, so it receives an echo of what it holds — a no-op. With one exception: volume is pushed through a 60 ms debounce, so a returning value can be one the user has already dragged past. That window therefore ignores `player:volume-changed` for 500 ms after its own last change. Discrete toggles need no such guard.

**Subscribe first, then snapshot.** When a listener keeps a set in sync and a command loads its initial value, the two must be ordered, not fired in parallel: `listen()` is a round-trip and Tauri replays nothing emitted before it resolves, so a change landing in that gap is missed by the event _and_ can be missed by a read that raced the write — the copy stays wrong until something else forces a refetch. [`useLikedChanged`](../../src/hooks/useLikedTracks.ts) returns a readiness flag its callers gate their fetch on; anything committed before the snapshot is in it, anything after arrives as an event. Changes that land between the two are replayed on top of the snapshot rather than overwritten by it.

### Non-frontend control surfaces go through `player_actions`

The tray menu, the OS media keys and the MPD server all need the same sequence the frontend gets from `commands::player`: advance the queue → `emit_track_changed` → `emit_queue_changed` → hand the track to the decoder.

That sequence was copy-pasted in `lib.rs` and `media_controls.rs` until #471 (the second copy was literally commented "Mirror of `lib.rs::spawn_next`") and the two had already drifted. Forgetting one emit leaves a surface showing the previous track — exactly the bug duplication kept producing. So a new surface calls [`player_actions::{next, previous, play_at_index}`](../../src-tauri/crates/app/src/player_actions.rs) rather than re-deriving it.

They're `async` and await rather than spawn: sync callers (souvlaki, tray) wrap in `tauri::async_runtime::spawn` themselves, async ones report the outcome back to their client.

---

## Database

### Never `DROP TABLE` a parent table in a migration

The profile pool opens with `foreign_keys = ON` and SQLite's `DROP TABLE` fires an implicit `DELETE`, so `ON DELETE SET NULL` / `CASCADE` on children run — dropping `artwork` blanks every `album.artwork_id`, dropping `artist` empties `track_artist`. `PRAGMA foreign_keys` is a no-op inside the transaction sqlx wraps migrations in, so it cannot be turned off there. Use `ALTER TABLE … ADD COLUMN` (issue #401 took this route for exactly this reason).

### Migrations are immutable once merged

sqlx records a SHA-384 checksum in `_sqlx_migrations.checksum` at apply time, so editing a merged migration crashes every existing install at boot with `"migration <id> was previously applied but has been modified"`. For any schema evolution, **create a new dated migration** `YYYYMMDDhhmmss_<slug>.sql`. Same rule for `migrations/app/`.

**Line-ending drift is a non-event.** [`db::migration_heal`](../../src-tauri/crates/app/src/db/migration_heal.rs) reconciles stored checksums against the compiled-in migrator before each `Migrator::run`: when the stored hash matches the LF or CRLF variant of the same SQL (the Windows `core.autocrlf=true` regression), it silently rewrites the row to the canonical hash and logs a warning. A real SQL change still panics, because neither LF nor CRLF normalization will rescue it.

**A database from a newer build is refused, out loud.** Because migrations only ever get appended, a database records the newest build that ever touched it, and an older binary finds a `_sqlx_migrations` row it has no migration for. Refusing is right — replaying a newer schema through older code corrupts it — but the refusal used to leave `AppState::init`, and an error out of the Tauri `setup` hook is a panic: the splash painted, the process died with it, nothing explained (#526). [`db::schema_guard`](../../src-tauri/crates/app/src/db/schema_guard.rs) now detects the case ahead of the migrator, and startup turns it into a native dialog and a clean exit.

Three rules come out of that, in order:

- **Guard before heal, heal before run.** The guard runs first because the heal pass _writes_ — no rewriting checksums into a database this build has already decided not to touch.
- **The guard must not outlive its reason.** `the_guard_fires_on_exactly_what_sqlx_refuses` pins it to sqlx's own `VersionMissing`; if sqlx ever stops refusing, the guard has started deciding policy on its own and that test says so.
- **Nothing you queue from `setup` will happen.** `setup` runs from inside the event loop (`RuntimeRunEvent::Ready`), so while it blocks, the loop dispatches nothing — and a fatal path never returns. Three consequences, all measured, all in [`report_fatal_and_exit`](../../src-tauri/crates/app/src/lib.rs): `tauri-plugin-dialog` is unusable because it dispatches through `run_on_main_thread`; the splash must be `hide()`n, not `close()`d, because a close is delivered as an _event_ while `hide` takes the runtime's same-thread fast path; and the dialog needs its own thread, because `TaskDialogIndirect` on the main thread creates its window and never shows it (`WS_VISIBLE` stays clear) and the process hangs there. The splash has to go regardless — it's `alwaysOnTop` and would sit over the dialog. Text is English like the tray's seed labels: the language preference lives in the database we just refused to open.

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

[`scan.rs::upsert_album`](../../src-tauri/crates/app/src/commands/scan.rs) keys on the album artist (Album Artist tag → `is_compilation` → primary artist fallback). `album.is_compilation` is sticky, and `merge_implicit_compilations` collapses ≥ 3 distinct-artist same-title rows into "Various Artists" after every scan. `edit.rs` re-runs `upsert_album` with the OLD album's Album Artist / compilation flags so renames don't re-split.

Deep dive: [library § album grouping](../features/library.md#album-grouping).

### File-write safety on Windows

Any command that rewrites an audio file (`edit::update_track_tags`, `save_lyrics`, `set_track_rating`) MUST pause playback first when the engine reports the edited track as `current_track_id` — lofty's `save_to_path` needs an exclusive handle on Windows. Re-hash with blake3 and update `track.file_hash` after the write so the scanner's `(mtime, size)` fast path stays addressable.

---

## Audio

### The audio callback is hot

The cpal callback (and the WASAPI exclusive thread) MUST NOT allocate, lock, or log. Only `rtrb::Consumer` reads and `Atomic*` loads. All heavy work (EQ, ReplayGain, resampling, FFT, BLAKE3) runs on the decoder thread before samples reach the SPSC ring.

The decoder's last stage is a `[-1.0, 1.0]` safety clamp ([`decoder::clamp_to_unity`](../../src-tauri/crates/app/src/audio/decoder.rs)) applied to every buffer right before `push_samples` — identity for an untouched stream (so bit-perfect output is preserved), it only bites when a gain stage overshoots unity and would otherwise hard-clip the DAC.

Wider topology: [audio architecture](audio.md).

---

## Frontend

### Virtual scroll everywhere

`TrackTable` uses `@tanstack/react-virtual` for 6000+ track performance. Virtualized tables consume `usePageScroll()` for the scroll element instead of nesting their own `overflow-y-auto` — that drives a single Spotify-style page scrollbar.

### Modal accessibility

Every modal calls [`useModalA11y(isOpen, onClose)`](../../src/hooks/useModalA11y.ts) — Escape-close, Tab focus trap, focus restoration. The container gets `role="dialog"` + `aria-modal="true"` + `aria-labelledby` (stable heading id) or `aria-label` (conditional heading). Don't roll bespoke `useEffect` Escape handlers.

### Overlays must be portalled, not just given a big `z-index`

Issue #390. A `z-index` is only compared inside its stacking context, and `position: fixed` escapes layout flow but NOT that context. WaveFlow's chrome is glassy — the TopBar carries `backdrop-blur-md`, and the Pulse / Liquid skins add `backdrop-filter` to the PlayerBar and other containers — and **any** `backdrop-filter` / `transform` / `opacity < 1` / `filter` / `will-change` ancestor creates a context that silently caps everything inside it. So a context menu at `z-100` nested under such an ancestor still paints _under_ the PlayerBar.

Anything at z-100+ (context menus, dropdown popovers) renders through `createPortal(…, document.body)` — see [`ContextMenu`](../../src/components/common/ContextMenu.tsx) and [`AnimatedModalShell`](../../src/components/common/AnimatedModalShell.tsx). Portalling is safe for skins because every skin rule is rooted at `:root[data-skin="…"] :where(…)` and `body` stays a descendant of `:root`.

The documented layer scale (in-panel sticky `z-10` → content sticky headers `z-20` → TopBar `z-30` → PlayerBar `z-50` → overlays `z-100`/`z-101`) lives at the top of [`src/app.css`](../../src/app.css) — extend it there, don't invent a bigger number locally.

### Right panels are flex siblings, not overlays

`NowPlayingPanel` / `QueuePanel` / `LyricsPanel` are mounted as flex children of the outer row in `AppLayout`. The center column has `min-w-0` so wide tables collapse instead of pushing the panel off-screen.

### Adding a new player-bar action

Default it into the overflow ("⋯") menu via [`MoreActionsMenu`](../../src/components/player/MoreActionsMenu.tsx) first; promote to primary only when usage warrants it; add a Settings pin toggle if both modes make sense. See [UI § player-bar layout](../features/ui.md#player-bar-layout).

---

## Network & sync

### Process-wide offline mode

Every outbound HTTP path (Deezer, Last.fm, similar, LRCLIB, the plugin registry) checks `offline::is_offline()` first and short-circuits to an empty payload or the cache. Persisted in `app_setting['network.offline_mode']`. **Treat new HTTP code paths the same way.**

### Remote user data never lands in the local tables

[RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md). Synchronized state describes the **server's** playlists, favourites, ratings, history, queue and shares, and those reference the **server's** tracks — which have no local counterpart. Writing them into `playlist` / `liked_track` / `track.rating` would leave two options, both wrong: fabricate local track rows for content that only exists on the server, or silently drop every entry. The first corrupts the library, the second makes sync look broken while reporting success.

The projection therefore lives in its own `remote_*` tables and is **reconstructible**: dropping it and re-fetching `GET /api/v2/sync/snapshot` is always a valid recovery, and is what the apply path does when it meets a known event it cannot apply. `remote_mutation` is the one exception — it holds writes the server has not seen yet, so it must survive a projection reset.

Matching a local file to a server track is deliberately out of scope and needs its own RFC.

**Two RFCs are numbered 003.** The desktop's [RFC-003](../rfcs/RFC-003-sync-architecture.md) (hybrid logical clocks, superseded) has nothing to do with the server's RFC-003 (sync v2, accepted). Any instruction naming "RFC-003" must name the repository too, or it will be read as the wrong document. On the desktop side the accepted design is **RFC-005**.

### The v1 sync protocol was removed

`crate::sync` is now a permanent no-op stub. The peer-to-peer v1 protocol — ops journal, logical clocks (`lamport` / `hlc`), digest/backfill reconciliation, per-op `snapshots` maps, and the scanner's track emit — was deleted in the RFC-005 cutover once the v2 snapshot bootstrap was proven end-to-end. See [RFC-005 §Decision 8](../rfcs/RFC-005-remote-source-and-sync-v2.md#decision-8--v2-landed-beside-v1-then-replaced-it).

The ~70 CRUD emit call sites still call `crate::sync::*`, but those now resolve to the stub's no-ops and nothing is enqueued. **New CRUD commands do not need to emit anything** — the remote source ([`crate::remote`](../../src-tauri/crates/app/src/remote/mod.rs), RFC-005) never touches local-entity CRUD. The HLC migration columns stay (immutable once merged) but are unused.

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
- **Plugin option values live in `<state_dir>/.plugin-config.json`** ([`plugin_config`](../../src-tauri/crates/core/src/plugin/plugin_config.rs)), the single source of truth — no `app_setting` row, excluded from the scratch quota. They reach the guest read-only through `waveflow:host/config.get-option`, pinned at instantiate time. The import is additive: a plugin built without it still instantiates.
