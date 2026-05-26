# Playlists

User-curated playlists live in the per-profile `playlist` table alongside the auto-generated [smart playlists](smart-playlists.md). The `is_smart` flag is the only thing distinguishing them in the schema — the UI filters on it to render the two groups in their own sections.

## CRUD

[`commands/playlist.rs`](../../src-tauri/crates/app/src/commands/playlist.rs) exposes:

- `list_playlists` / `get_playlist` — both compute `track_count` and `total_duration_ms` in the SELECT (denormalisation lives in the query, not the schema) and resolve `cover_hash` → `cover_path` from the shared `metadata_artwork` cache.
- `create_playlist`, `update_playlist`, `delete_playlist` — all bump `updated_at`.
- `add_track_to_playlist` / `add_tracks_to_playlist` / `remove_track_from_playlist` / `reorder_playlist_track`.
- `add_source_to_playlist` — bulk-add by `(source_type, source_id)`: every track of an album / artist / library / liked / recent in one round-trip.

## Reordering

Drag-and-drop is implemented with `@dnd-kit` over a virtualised list (`@tanstack/react-virtual`). The `reorder_playlist_track` command renumbers the affected `position` slice in a single transaction; rows outside the moved range stay untouched.

## M3U import / export

`export_playlist_m3u` writes a UTF-8 `.m3u8` with `#EXTINF:<duration>,<artist> - <title>` headers and absolute file paths. The importer accepts both `.m3u` and `.m3u8` (foobar2000, VLC, Rekordbox, hand-written sets) and matches against the active library:

1. **Canonical-path match** — strip Windows `\\?\` prefix, lowercase the drive letter, normalise separators.
2. **Basename fallback** — if path-match fails, try the basename. This recovers playlists exported before a library reorganisation.

Unmatched entries are surfaced (capped at 20 to keep the toast readable) so the user can investigate without losing the import.

## Likes

[`liked_track`](../../src-tauri/migrations/profile/20260411120000_initial.sql) is a one-column table keyed by `track_id` with a `liked_at` timestamp. The dedicated "Liked" view sorts by `liked_at DESC`; the heart icon in track rows is wired to `toggle_like_track`.

## Cover management

User playlists support custom covers, managed alongside the existing `cover_hash` column added for smart playlists. Two modes, one column flag:

| `cover_is_auto` | Behaviour                                                                                                                                                                                                                 |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `1` (default)   | Auto-cover. After **every** mutation (`add_track`, `add_tracks`, `remove_track`, `reorder_track`, `add_source`, `import_m3u`), the backend re-runs the compositor on the first 4 album artworks (Spotify-style 2×2 grid). |
| `0`             | Manual upload. Mutations leave the cover untouched.                                                                                                                                                                       |

[`commands/playlist_cover.rs`](../../src-tauri/crates/app/src/commands/playlist_cover.rs) exposes three Tauri commands:

- **`set_playlist_cover_from_file(playlist_id, file_path)`** — magic-byte validates jpg/png/webp (8 MB cap), normalises through the same compositor used for auto-covers (re-encodes to a 640×640 JPEG so every `cover_hash` resolves to one extension), flips `cover_is_auto = 0`.
- **`regenerate_playlist_auto_cover(playlist_id)`** — explicit "refresh now" escape hatch.
- **`clear_playlist_cover(playlist_id)`** — drops the manual cover, switches back to `cover_is_auto = 1`, and **immediately** re-runs the auto-pipeline so the visual feedback is instant rather than blank-until-next-mutation.

Smart playlists (`is_smart = 1`) are excluded from every code path here — they're owned by the smart-playlist regen flow. The post-mutation hook (`playlist_cover::maybe_regen_auto_cover`) checks both flags before running and logs a warning on failure (never blocks the mutation it was triggered by).

The frontend exposes the controls in the **edit modal** ([`CreatePlaylistModal.tsx`](../../src/components/common/CreatePlaylistModal.tsx)) Spotify-style: large preview tile on the left, hover overlay with a pencil icon ("Choose photo"), and a `...` menu top-right with "Change photo" / "Remove photo". The remove option is conditional on `cover_hash != null`. After every cover mutation the modal calls `onCoverChanged`, which the parent ([`PlaylistView`](../../src/components/views/PlaylistView.tsx)) implements by re-fetching the current playlist _and_ invoking [`PlaylistContext.refresh()`](../../src/contexts/PlaylistContext.tsx) — that second call is what makes the sidebar tile re-render (the sidebar reads from the context, not from `PlaylistView`'s local state).

## Recently played

[`browse.rs::list_recent_plays`](../../src-tauri/crates/app/src/commands/browse.rs) projects the last 50 distinct tracks from `play_event`, deduplicated by track id (you only see a given track once even if you played it three times in a row). Drives both the "Récents" sidebar entry and the home carousel.
