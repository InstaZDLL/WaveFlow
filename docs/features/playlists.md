# Playlists

User-curated playlists live in the per-profile `playlist` table alongside the auto-generated [smart playlists](smart-playlists.md). The `is_smart` flag is the only thing distinguishing them in the schema — the UI filters on it to render the two groups in their own sections.

**Where they surface.** The sidebar list is the primary entry point, plus a **grid tab** under My music → Playlists ([`PlaylistGrid`](../../src/components/views/library/PlaylistGrid.tsx), issue #461) for browsing them all at once the way the Albums tab does — a user who curates their own playlists reads them as albums, not as a one-at-a-time list. The grid reads `PlaylistContext` rather than issuing its own query, so it costs no round-trip; it renders `is_smart === 0` only.

## CRUD

[`commands/playlist.rs`](../../src-tauri/crates/app/src/commands/playlist.rs) exposes:

- `list_playlists` / `get_playlist` — both compute `track_count` and `total_duration_ms` in the SELECT (denormalisation lives in the query, not the schema) and resolve `cover_hash` → `cover_path` from the shared `metadata_artwork` cache.
- `create_playlist`, `update_playlist`, `delete_playlist` — all bump `updated_at`.
- `add_track_to_playlist` / `add_tracks_to_playlist` / `remove_track_from_playlist` / `reorder_playlist_track`.
- `add_source_to_playlist` — bulk-add by `(source_type, source_id)`: every track of an album / artist / library / liked / recent in one round-trip.

## The right-click menu

Every playlist action used to live only as an icon button in [`PlaylistView`](../../src/components/views/PlaylistView.tsx)'s header, so reaching any of them meant opening the playlist first, and a right-click on a sidebar row or a library card did nothing — inert in a release build, and in a dev build the webview's own menu came through instead (#737).

[`usePlaylistContextMenu`](../../src/hooks/usePlaylistContextMenu.tsx) is the one menu both surfaces open, modelled on [`useRemoteTrackContextMenu`](../../src/hooks/useRemoteTrackContextMenu.tsx) and returning the same `{ open, openFromKeyboard, close, render }`. It offers Play / Shuffle / Play next / Add to queue, then Edit, Export as M3U and Delete. Shuffle only ever turns shuffle **on**: `player_toggle_shuffle` really toggles, so firing it unconditionally — which the header's button did — switched it off for anyone who already had it on, and played the playlist in order. It owns the edit modal itself rather than leaving each caller to mount one, so the sidebar and the grid cannot drift apart on what Edit opens.

The sidebar's two pinned rows — Liked tracks and Recently played — open the same menu with the playable half only: Play, Shuffle, Play next, Add to queue. They are queries, not stored rows, so there is nothing to rename, export or delete. Recently played queues the last 300 tracks — the limit this caller passes; `list_recent_plays` takes whatever it is given — and that command is already one row per track — it groups `play_event` by track and orders by `MAX(played_at)`. The raw log with its repeats is `list_play_history`, which is what the view itself renders.

A **server** playlist gets one item, Open. Every other action takes a local rowid it does not have, and its playback runs through the view's own remote path rather than `player_play_tracks` — so the alternative was a menu with every item greyed out, which is worse than the inert right-click this replaced.

The grid is virtualized, so the menu state lives in [`PlaylistGrid`](../../src/components/views/library/PlaylistGrid.tsx) rather than in a card — a card can unmount under an open menu. `ContextMenu` portals to `body` and closes when the page scrolls, so that unmount is harmless. A scroll inside the menu itself — the playlist list of "Add to a playlist" — leaves it open.

## Reordering

Drag-and-drop is implemented with `@dnd-kit` over a virtualised list (`@tanstack/react-virtual`). The `reorder_playlist_track` command renumbers the affected `position` slice in a single transaction; rows outside the moved range stay untouched.

## M3U export

`export_playlist_m3u` writes a UTF-8 `.m3u8` with `#EXTINF:<duration>,<artist> - <title>` headers and absolute file paths, through a partial sibling of its own (created exclusively) renamed over the destination once complete ([`atomic_write`](../../src-tauri/crates/core/src/atomic_write.rs)), so overwriting an existing playlist file that fails halfway leaves the old one intact rather than truncated; a partial left by a write that never finished is removed by the next write to the same file once it is a day old. A [placeholder](#placeholders) has no line in it: an M3U entry is a path, and a placeholder has none.

## Importing a playlist

The sidebar's import button opens [`ImportPlaylistModal`](../../src/components/common/ImportPlaylistModal.tsx): a file, or a public Deezer playlist's link. Every source is read into the same list of entries ([`playlist_import`](../../src-tauri/crates/core/src/playlist_import/mod.rs)), in the playlist's order, and goes through the same matcher; [`commands/playlist_import.rs`](../../src-tauri/crates/app/src/commands/playlist_import.rs) creates the playlist and writes its entries in one transaction, so a failed import leaves nothing behind. The dialog stays open on the outcome — tracks added, entries waiting for their track, repeats skipped — and the sidebar opens the new playlist.

| Source                  | Read by                                                                                                                                                | What an entry carries                                                                                                                                                                                                                                                                                                                                                                                     |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `.m3u` / `.m3u8`        | [`m3u.rs`](../../src-tauri/crates/core/src/playlist_import/m3u.rs)                                                                                     | The path, resolved against the playlist's folder; `#EXTINF` gives title, artist (`Artist - Title`) and length. An entry without one is named after its file. `#PLAYLIST:` names the playlist.                                                                                                                                                                                                             |
| `.csv` (`.tsv`, `.txt`) | [`csv.rs`](../../src-tauri/crates/core/src/playlist_import/csv.rs)                                                                                     | A header is required; columns are recognised by name, in any order: `title` / `track` / `track name` / `name` / `song` (required), `artist` / `artists` / `artist name(s)`, `album` / `album name`, `duration_ms` / `duration (ms)` (milliseconds) or `duration` / `length` / `time` (seconds or `m:ss`), `isrc`. The separator is whichever of `,` `;` tab the header uses most; fields follow RFC 4180. |
| `.json`                 | [`json.rs`](../../src-tauri/crates/core/src/playlist_import/json.rs)                                                                                   | An array of tracks, or `{ "name", "tracks": [...] }`; each track has `title`, `artist` (a string, an array of strings or of `{ "name" }`), `album`, `duration_ms` or `duration`, `isrc`.                                                                                                                                                                                                                  |
| Deezer link             | [`deezer_link.rs`](../../src-tauri/crates/core/src/playlist_import/deezer_link.rs) + [`deezer.rs`](../../src-tauri/crates/core/src/metadata/deezer.rs) | `deezer.com/[lang/]playlist/<id>` or the bare id; a `link.deezer.com` share link is followed through Deezer's hosts only. The public API needs no account — a private playlist is refused like a missing one. Title, artist, album, length and ISRC per track.                                                                                                                                            |

Files are decoded like a lyrics sidecar ([`text_encoding`](../../src-tauri/crates/core/src/text_encoding.rs)): a legacy `.m3u`, or a CSV out of a spreadsheet, is in the code page of the machine that wrote it. An import keeps at most 10 000 entries, and a file over 16 MB is refused before it is read: no playlist is that large.

**Which track an entry is** ([`matching.rs`](../../src-tauri/crates/core/src/playlist_import/matching.rs)), strongest key first:

1. **The path an M3U named** — compared lexically (`.` and `..` resolved, the Windows `\\?\` prefix stripped, case folded on Windows), then its file name, which recovers a playlist written before the library was reorganised — alone only when the entry names no artist and a single file has that name, otherwise only when the file found is by that artist: `01.mp3` names a track in every album. Never resolved on disk: a playlist may come from anyone, its paths are kept with its placeholders and compared again after every scan, and resolving `\\host\share\…` would open a connection to that host — on Windows, handing it the user's credentials.
2. **ISRC** — the library's comes from the file's tags (`track_tag`, key `ISRC`). Several tracks with one ISRC: the closest length wins.
3. **Title + artist + length** — the title through `normalize_name`, the artist through the shared `select_by_name` matcher against every credited artist (an export's "A, B" or "A feat. B" is split, and kept whole too), the length within 3 s when both sides know it. A title that only agrees once its version suffix is stripped ("(Remastered 2011)", "- Radio Edit") must agree on the length, which is what tells a remaster from a live take. An entry with no artist is never matched by its title: "Intro" names a hundred tracks.

A playlist holds a track once (`playlist_track`'s key), so a repeat of a track already in it is dropped and counted as such.

## Placeholders

An entry the library does not hold is kept as a **placeholder** (`playlist_placeholder`, [storage.md](../architecture/storage.md#placeholders-keep-their-place-in-the-tracks-numbering)): its place in the order and what the source said about it. It is never presented as something the user has:

- **Shown greyed in its place**, with a dashed tile and "Not in your library", and only a remove button — nothing to play, open or select. Under a display sort (title, artist…) there is no place to keep, so the placeholders follow the tracks.
- **Not counted**: `track_count` and `total_duration_ms` are the tracks; the header adds "· N missing", the view's own count of the placeholders it lists (`placeholder_count` on the row carries the same figure for other surfaces).
- **Never queued**: playing the playlist plays its tracks, and a placeholder is simply not in the list the queue is built from — playback goes on to the next track.
- **Filled in by the scan.** After every scan, next to the play-history reattachment, [`fill_pending`](../../src-tauri/crates/core/src/repository/sqlite/playlist_placeholder.rs) runs the same matcher over every placeholder; a match takes the placeholder's place in `playlist_track` (a placeholder whose playlist already holds the track just goes) and the playlist is announced on `playlist:changed`, which `PlaylistContext` refreshes on. The watcher and a drop import both end in a scan, so a track that arrives by any route fills its place. A profile with no placeholder pays one `COUNT(*)`, and a scan that added, rewrote and brought back nothing (the folder's available tracks unchanged) does not match at all: matching reads the whole library.

They stay out of everything else: smart playlists, Daily Mix and On Repeat are generated and never hold any; the conversion of a local playlist to a server one ([RFC-005](../rfcs/RFC-005-remote-source-and-sync-v2.md)) sends its tracks, not its placeholders; the DLNA server does not expose playlists. A profile archive carries them with the rest of `data.db`.

Moving a track jumps it over the placeholders between its old and new place, which stay before the tracks they were before; a placeholder cannot itself be dragged, and a track dropped at the end lands before the placeholders already waiting at the tail.

## Likes

[`liked_track`](../../src-tauri/migrations/profile/20260411120000_initial.sql) is a one-column table keyed by `track_id` with a `liked_at` timestamp. The dedicated "Liked" view sorts by `liked_at DESC`; the heart icon in track rows is wired to `toggle_like_track`.

## Cover management

User playlists support custom covers, managed alongside the existing `cover_hash` column added for smart playlists. Two modes, one column flag:

| `cover_is_auto` | Behaviour                                                                                                                                                                                                              |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `1` (default)   | Auto-cover. After **every** mutation (`add_track`, `add_tracks`, `remove_track`, `reorder_track`, `add_source`, an import), the backend re-runs the compositor on the first 4 album artworks (Spotify-style 2×2 grid). |
| `0`             | Manual upload. Mutations leave the cover untouched.                                                                                                                                                                    |

[`commands/playlist_cover.rs`](../../src-tauri/crates/app/src/commands/playlist_cover.rs) exposes three Tauri commands:

- **`set_playlist_cover_from_file(playlist_id, file_path)`** — magic-byte validates jpg/png/webp (8 MB cap), normalises through the same compositor used for auto-covers (re-encodes to a 640×640 JPEG so every `cover_hash` resolves to one extension), flips `cover_is_auto = 0`.
- **`regenerate_playlist_auto_cover(playlist_id)`** — explicit "refresh now" escape hatch.
- **`clear_playlist_cover(playlist_id)`** — drops the manual cover, switches back to `cover_is_auto = 1`, and **immediately** re-runs the auto-pipeline so the visual feedback is instant rather than blank-until-next-mutation.

Smart playlists (`is_smart = 1`) are excluded from every code path here — they're owned by the smart-playlist regen flow. The post-mutation hook (`playlist_cover::maybe_regen_auto_cover`) checks both flags before running and logs a warning on failure (never blocks the mutation it was triggered by).

The frontend exposes the controls in the **edit modal** ([`CreatePlaylistModal.tsx`](../../src/components/common/CreatePlaylistModal.tsx)) Spotify-style: large preview tile on the left, hover overlay with a pencil icon ("Choose photo"), and a `...` menu top-right with "Change photo" / "Remove photo". The remove option is conditional on `cover_hash != null`. After every cover mutation the modal calls `onCoverChanged`, which the parent ([`PlaylistView`](../../src/components/views/PlaylistView.tsx)) implements by re-fetching the current playlist _and_ invoking [`PlaylistContext.refresh()`](../../src/contexts/PlaylistContext.tsx) — that second call is what makes the sidebar tile re-render (the sidebar reads from the context, not from `PlaylistView`'s local state).

See [The header colour](#the-header-colour) for where the header's gradient comes from — the cover feeds it when `color_mode` is `auto`.

## The header colour

The playlist header is a two-stop gradient rather than a flat surface, and where its colour comes from is a per-playlist choice stored in the `playlist.color_mode` column (migration `20260921120000_playlist_color_mode.sql`, `TEXT NOT NULL DEFAULT 'auto'` with a `CHECK (color_mode IN ('auto','manual'))`). The default is deliberate: an existing playlist with artwork adopts the image-derived colour, and the stored palette colour stays available for whenever no artwork can be loaded.

The control is the auto / manual segmented pair in [`CreatePlaylistModal`](../../src/components/common/CreatePlaylistModal.tsx) — the palette picker only appears in `manual`, because in `auto` it would be a control with no effect.

**Auto samples the cover, but not by averaging it.** [`vibrantColorFromPixels`](../../src/lib/dominantColor.ts) buckets pixels into 8 hues and weights by chroma, because the plain dominant-colour path turns a multicolour collage into grey — the one case where a cover has the most colour to offer. [`usePlaylistAccent`](../../src/hooks/usePlaylistAccent.ts) then derives the two stops from that hue.

**The two schemes are different designs, not inverses** (`HeaderScheme = "dark" | "light"`). The dark header is a saturated ground carrying white text; the light one is a pale tint carrying near-black ink, because a light header that fades toward transparent sits on the Liquid skin's aurora and stops being a header at all.

**A contrast guard runs on the generated colour, and it measures the faintest ink on the header, not the boldest.** The header's secondary labels render at 85 % opacity over the gradient (`LABEL_OPACITY = 0.85`), sampled at the point where the text sits (`TEXT_AT = 0.4` between the stops), and the guard darkens or lightens until that composite clears its floor. The floors differ by scheme and that is a decision, not an oversight: **3:1** on the dark header (large text, and holding 4.5 there would wash every accent toward grey) and **4.5:1** on the light one, where dark ink on a pale ground makes the stricter ratio affordable. The light branch measures at `top`, its darkest stop.

## Recently played

[`browse/history.rs::list_recent_plays`](../../src-tauri/crates/app/src/commands/browse/history.rs) projects the most-recently-played distinct tracks from `play_event`, deduplicated by track id (you only see a given track once even if you played it three times in a row). **The row count is the caller's** — the command takes a `limit`, the home carousel asks for 12 and the playlist context menu for 300. The "Récents" sidebar entry does **not** go through it: it opens `HistoryView`, which renders the raw log via `list_play_history` and therefore keeps the repeats. Playing a row there queues that play and every later row loaded so far, one entry per track, so playback carries on back through the history instead of stopping after one song.

**Save the queue as a playlist.** The queue panel's save button opens the create-playlist dialog with a dated name ("Queue of …") and copies every queue row, in queue order, played ones included, into the new playlist. Both this and "Keep as playlist" on a mix ([smart-playlists.md](smart-playlists.md)) go through `CreatePlaylistModal`'s `initialName` and add the tracks once the playlist exists; a failure there leaves the playlist created, so the dialog never offers a retry that would duplicate it.
