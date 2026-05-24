# Library

The library is a per-profile SQLite database (`<root>/profiles/<id>/data.db`) keyed by canonical file path. It survives moves and renames as much as possible — see the import matcher in [playlists](playlists.md).

## Scanning

- **Tag extraction** — [`lofty 0.24`](https://crates.io/crates/lofty) reads ID3v2 / Vorbis Comments / MP4 atoms and surfaces title, artist(s), album, album artist, year, track / disc number, genre, embedded artwork, POPM ratings, and the tagged musical key (`TKEY` / `INITIALKEY`).
- **Folder cover fallback** — when a track carries no embedded picture, the scanner inspects its parent directory for a sidecar image with one of the canonical stems (`cover`, `folder`, `front`, `albumart`, `album`, `artwork`) and an extension the thumbnail pipeline can decode (`jpg`/`jpeg`, `png`, `webp`, `bmp`, `gif`, `tiff`). The first match — by stem priority, not alphabetical — is hash-addressed into the shared `artwork/` dir like an embedded picture. The provenance is tagged `source = 'folder'` in the `artwork` table so a future cleanup job can distinguish it from `'embedded'`, `'deezer'`, or `'user'` entries. Covers common CD-rip / lossless layouts where the artwork sits beside the audio files.
- **Audio quality** — sample rate, bitrate, channel count, bit depth and codec are captured at scan time. Hi-Res badges (≥ 24-bit, ≥ 44.1 kHz) light up automatically on covers and rows.
- **Watch folders** — [`notify 8`](https://crates.io/crates/notify) drives a per-folder filesystem watcher with debounced rescans so files dropped into a watched directory appear without a manual refresh. Deletions flag rows `is_available = 0` rather than purging them, so play history, ratings and playlist memberships survive a reorganisation.

## Audio analysis

[`analysis.rs`](../../src-tauri/src/analysis.rs) computes peak, integrated loudness (dB), ReplayGain (–18 LUFS reference) and BPM (autocorrelation). Runs on demand (per track) or as a background sweep (whole library), gated by a Settings toggle. Results land in `track_analysis` and feed:

- per-stream gain in the audio engine (`replaygain_enabled`),
- the BPM bucketing in [smart playlists](smart-playlists.md),
- the per-track audio specs strip under the player.

## Multi-artist

The scanner splits `"Artist A, Artist B"` (and `;` / `feat.` / `&` variants) on insert. Each contributor lands in its own `artist` row, linked to the track via the `track_artist` many-to-many table with a `position` column for stable ordering. Queries rebuild the display string with `GROUP_CONCAT(...) ORDER BY position`. The `ArtistLink` React component receives parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable, matching Spotify's behaviour.

## Browsing

- **Library tabs** — Morceaux, Albums, Artistes, Genres, Dossiers; each tab keeps its own scroll position and sort memory (per profile). All five tab queries (`list_tracks` / `list_albums` / `list_artists` / `list_genres` / `list_folders`) fire in parallel on first mount so subsequent tab switches hit cached React state instantly instead of paying a fresh SQL round-trip; the first paint shows a layout-shaped `LibraryTabSkeleton` (`role="status"` / `aria-busy="true"`) until the data lands, never the EmptyState. Browse queries lean on partial indexes `idx_track_album_available` / `idx_track_primary_artist_available` (`WHERE is_available = 1`) so the GROUP BY aggregates stay index-only on healthy libraries. Clicking a genre tile opens a Spotify-style genre detail page (`get_genre_detail` in [`browse.rs`](../../src-tauri/src/commands/browse.rs)) with every track tagged with that genre, sorted Artist → Album → Disc → Track.
- **Bulk list endpoints wire format** — `list_tracks` / `list_playlist_tracks` / `list_liked_tracks` (track-shaped) and `list_albums` / `list_artists` (browse-shaped) all return `{ artwork_base, items: <Slim>[] }` instead of the full row shape (artists additionally include `metadata_artwork_base` for the Deezer cache). Each slim row carries `artwork_hash` + `artwork_format` + `artwork_has_1x` + `artwork_has_2x` (artists also `picture_hash` + `picture_has_*`) instead of three full path strings; the ~70-char per-profile prefix only appears once in the response. Frontend wrappers ([`expandTrackResponse`](../../src/lib/tauri/track.ts), `expandAlbumRow` / `expandArtistRow` in [`browse.ts`](../../src/lib/tauri/browse.ts)) stitch the absolute paths back together so every UI consumer keeps the full `Track` / `AlbumRow` / `ArtistRow` shape unchanged. Cuts ~30 % off each payload (e.g. ≈ 1.0 MB → ≈ 700 kB on a 1k-track `list_tracks`, ≈ 650 kB → ≈ 250 kB on a 900-artist `list_artists`), proportionally shrinking JSON parse + IPC transfer time. Any future bulk endpoint shipping artworks for hundreds of rows should adopt the same `{ artwork_base, items }` shape.
- **A-Z navigator** — letter rail on the artists tab, NFD-normalised so accents (É → E, Ñ → N) bucket correctly.
- **Multi-select** — ctrl/shift across rows with a floating action bar (Play / Add to queue / Add to playlist / Remove) anchored to the bottom of the viewport.
- **Track Properties dialog** — foobar2000-style modal with the full tag set, audio specs, analysis results, file path and a Show in Explorer button.
- **POPM ratings** — 5-star with half-steps, round-tripped to the file's tag. Edit surfaces: inline `StarRating` in the library track list, integer-star submenu in the right-click `TrackContextMenu` (any view), full half-star widget in the `TrackPropertiesModal`. The backend command `set_track_rating` writes the POPM frame back to the file (binary `<email>\0<rating><counter>` for ID3v2, text `RATING=0-100` for Vorbis / MP4 / APE), updates `track.rating` in the DB, then emits `track:updated` so every open view re-fetches without polling. Containers lofty can't open (DSD) keep a DB-only rating; the next folder scan still preserves it because the fast-path skip on `(mtime, size)` never re-extracts unchanged files. Smart playlists expose this as the `rating_min` rule — see [smart-playlists.md](smart-playlists.md#custom-smart-playlists-rule-based).
- **Lightbox** — double-click any cover or artist photo to view full-size with keyboard navigation.

## Search

FTS5 contentless index over `title`, `artist`, `album` with prefix matching. Auto-sync triggers (`AFTER INSERT/UPDATE/DELETE` on `track`) keep the index current using the `'delete'` command on the contentless table. Queries are issued from the React top bar with a 150 ms debounce.

## Folder management

[`commands/library.rs`](../../src-tauri/src/commands/library.rs) exposes the watch-folder lifecycle: `add_folder_to_library`, `set_folder_watched` (toggle the in-memory `notify` watcher), and `remove_folder_from_library`. The remove path detaches the watcher, deletes every track that lived under the folder, then drops the `library_folder` row in a single transaction. The schema's `track.folder_id ON DELETE SET NULL` would otherwise leave orphan tracks with `library_id` still set — making the user "remove" a folder while its tracks stayed in the library, which never matches what they expect.

UI: per-folder trash button in the Library → Folders tab, two-step confirm-on-second-click that auto-clears after 3 s.

## Drag-and-drop import

[`hooks/useDragDropImport.ts`](../../src/hooks/useDragDropImport.ts) wires Tauri 2's window-level `onDragDropEvent` into the existing import flow via a single backend command: [`commands/library.rs::import_paths`](../../src-tauri/src/commands/library.rs). The command accepts a mix of folders and audio files — files contribute their parent directory — dedupes the resolved folder set, then for each one tries an `INSERT OR IGNORE INTO library_folder` (the `(library_id, path)` UNIQUE constraint absorbs duplicates) and runs `scan_folder_inner`. Aggregated `ScanSummary` is returned to the frontend so the user sees one toast with the total counts.

Auto-creates a default library on the very first drop when the profile has none, mirroring the existing pickFolder import path.

UI: emerald drop overlay in [`AppLayout`](../../src/components/layout/AppLayout.tsx) renders a fade-in border + drop hint while the user is dragging, then a spinner while the backend scan runs. `pointer-events: none` on the overlay so the drop still hits Tauri's native handler.

## Duplicate detection

[`commands/duplicates.rs`](../../src-tauri/src/commands/duplicates.rs) groups every available track by `file_hash` (BLAKE3 of the audio bytes, computed at scan time) so byte-identical copies in different folders fall into the same bucket regardless of metadata. Re-encodes of the same source — e.g. CBR vs VBR rips — **won't** match because the bytes differ; that's a fingerprinting problem and out of scope for the MVP.

`find_duplicates` returns one entry per group, ordered by `added_at ASC` so the oldest copy renders first (usually the one to keep). `delete_tracks(track_ids)` cascades through the schema's `ON DELETE` constraints to clean up `track_artist`, `track_genre`, `playlist_track`, `play_event`, etc. — but **the audio files on disk are not touched**: the user removes them via the OS so we don't accidentally wipe a backup.

UI: [`DuplicatesModal`](../../src/components/common/DuplicatesModal.tsx) launched from Settings → Stockage → "Rechercher". Each group exposes a radio selector (defaults to oldest) and the footer's "Supprimer N doublons" wipes every other entry from the database.

## Cover picker

[`commands/deezer.rs::set_album_artwork_from_deezer`](../../src-tauri/src/commands/deezer.rs) and `set_album_artwork_from_file`. The file picker validates magic bytes (JPEG / PNG / WebP) before accepting upload, and `batch_fetch_missing_album_covers` walks all albums without an `artwork_id`, querying Deezer in parallel with a small concurrency cap.

## Local artist images

Scanner sidecar lookup, mirror of the folder-cover fallback but resolved against the track's ancestors instead of the immediate parent.

[`commands/scan.rs::extract_artist_image`](../../src-tauri/src/commands/scan.rs) walks up to **3 parent directories** from each track and accepts the first match where either:

- the filename stem is one of `ARTIST_IMAGE_STEMS = ["artist", "performer", "band"]`, **or**
- the stem's `canonical_name(...)` equals the artist's canonical name (covers `Daft Punk.jpg` at the root of a `Daft Punk/` folder).

Both common layouts from issue #31 work out of the box:

- `Music/<Artist>/<Album>/track.flac` → matches `artist.jpg` two levels up.
- `Music/<Album>/track.flac` → matches `<Artist>.jpg` sitting beside the album folder (strict name-match so an unrelated `cover.jpg` is never mistaken for an artist photo).

Hash-addressed via BLAKE3 into the shared `artwork/<hash>.{jpg,png,webp,…}` cache and linked through the existing `artist.artwork_id → artwork` foreign key (no schema change). The `UPDATE … WHERE artwork_id IS NULL` guard means scanner runs never overwrite a manually uploaded image or a previously cached Deezer picture.

Resolution priority in [`commands/browse.rs::get_artist_detail`](../../src-tauri/src/commands/browse.rs) is now: **local sidecar → Deezer cache → live Deezer fetch** (last skipped when offline). [`ArtistDetailView`](../../src/components/views/ArtistDetailView.tsx) prefers `artwork_path` over `picture_path` and refuses to clobber a local image with a late-arriving Deezer response.

The `"Various Artists"` sentinel is explicitly excluded so a compilation folder never inherits a stray album cover as an artist photo.

For libraries scanned before the feature shipped, [`commands/scan.rs::rescan_local_artist_images`](../../src-tauri/src/commands/scan.rs) (exposed as **Settings → Library → Local artist images**) walks every `artist WHERE artwork_id IS NULL` and probes up to 16 tracks per artist with `extract_artist_image`, stopping at the first hit. Already-linked rows are filtered out at the SQL level, so the rescan is cheap to re-run.

### Manual override

The pencil overlay on the artist photo in [`ArtistDetailView`](../../src/components/views/ArtistDetailView.tsx) opens [`ArtistImagePickerModal`](../../src/components/common/ArtistImagePickerModal.tsx), which exposes three actions backed by [`commands/deezer.rs`](../../src-tauri/src/commands/deezer.rs):

- **Search Deezer** → `search_artists_deezer` + `set_artist_artwork_from_deezer` (downloads the chosen picture into the profile artwork cache, marks source `"deezer"`).
- **Pick a local file** → `set_artist_artwork_from_file` (same magic-byte validation as the album cover picker: jpg / png / webp).
- **Remove image** → `clear_artist_artwork` sets `artist.artwork_id = NULL` so the next render falls back through the resolution chain (Deezer cache → live fetch).

Both `set_artist_artwork_from_*` overwrite `artwork_id` unconditionally — an explicit user pick beats any automatic resolution.
