# Library

The library is a per-profile SQLite database (`<root>/profiles/<id>/data.db`) keyed by canonical file path. It survives moves and renames as much as possible — see the import matcher in [playlists](playlists.md).

## Scanning

- **Tag extraction** — [`lofty 0.25`](https://crates.io/crates/lofty) reads ID3v2 / Vorbis Comments / MP4 atoms and surfaces title, artist(s), album, album artist, year, track / disc number, genre, embedded artwork, POPM ratings, and the tagged musical key (`TKEY` / `INITIALKEY`).
- **Folder cover fallback** — when a track carries no embedded picture, the scanner inspects its parent directory for a sidecar image with one of the canonical stems (`cover`, `folder`, `front`, `albumart`, `album`, `artwork`) and an extension the thumbnail pipeline can decode (`jpg`/`jpeg`, `png`, `webp`, `bmp`, `gif`, `tiff`). The first match — by stem priority, not alphabetical — is hash-addressed into the shared `artwork/` dir like an embedded picture. The provenance is tagged `source = 'folder'` in the `artwork` table so a future cleanup job can distinguish it from `'embedded'`, `'deezer'`, or `'user'` entries. Covers common CD-rip / lossless layouts where the artwork sits beside the audio files.
- **Audio quality** — sample rate, bitrate, channel count, bit depth and codec are captured at scan time. Hi-Res badges (≥ 24-bit, ≥ 44.1 kHz) light up automatically on covers and rows.
- **Watch folders** — [`notify 8`](https://crates.io/crates/notify) drives a per-folder filesystem watcher with debounced rescans so files dropped into a watched directory appear without a manual refresh. Deletions flag rows `is_available = 0` rather than purging them, so play history, ratings and playlist memberships survive a reorganisation.
- **Live progress** — `scan_folder_inner` emits a `scan:progress` event (throttled to every 25 files, plus one initial and one final tick) carrying `current` / `total`, the running `added` / `updated` / `skipped` / `errors` tallies, and `current_dir` — the parent directory of the file just processed. [`ScanProgressToast`](../../src/components/common/ScanProgressToast.tsx) (mounted once in `AppLayout`) renders a bottom-right card with a percentage bar and a live "…/Parent/Album" folder line so the user can watch the scan walk through directories instead of staring at a bare spinner (issue #430). `current_dir` is `None` on the initial + final ticks, which carry no specific file.

### Folder-cover reconciliation

The scan fast path keys on each **audio** file's `(mtime, size)`. Replacing a sidecar `cover.jpg` next to those files changes nothing it looks at: the tracks are skipped, `extract_folder_cover` never runs, and the album keeps its old picture indefinitely. The file that changed simply isn't one the scanner watches (issue #366).

[`refresh_folder_covers`](../../src-tauri/crates/core/src/scanner/upserts.rs) closes that gap as a post-scan pass. It works per **directory** rather than per file — one `read_dir` + one hash for a whole album instead of one per track — and updates any album whose sidecar no longer matches. No extra bookkeeping is required: `artwork.hash` is already the blake3 of the picture bytes, so the stored row is its own baseline for comparison, and no migration was needed.

Three deliberate restrictions:

- **Only sidecar-sourced artwork is replaced.** The guard is an _allowlist_: `artwork.source = 'folder'`, or no artwork at all. Everything else was put there by something that outranks a sidecar — `embedded` (extraction treats the sidecar as a fallback for tracks whose tag carries no picture), `user` (a manual upload), `deezer` (an enrichment fetch) — and a source added later is preserved by default rather than silently clobbered.
- **A deleted sidecar does not blank the album.** A vanished cover is far more likely to be a transient state (files being reorganised) than a request for a blank album.
- **A multi-directory album resolves against its first directory** in sorted order, evaluated over _all_ of the album's tracks rather than only those under the scanned folder. Scoping it to the scanned folder would make the winning directory depend on which folder triggered the scan — disc 1 winning one pass and disc 2 the next — which is exactly the non-determinism this rule exists to prevent.

Directory resolution (`read_dir` + read + blake3 per directory, potentially hundreds of megabytes) runs in a single `spawn_blocking` batch rather than on the async runtime, and the writes land in one transaction — the scanner is the single writer, so per-album autocommits would serialise N round-trips through WAL for no benefit.

Because that walk can take seconds, the candidate list is a snapshot that may be stale by the time it is written. The update is therefore a compare-and-swap that also re-asserts the source allowlist (`link_folder_cover_if_eligible`): an album whose cover changed mid-walk — the user uploaded one, or a concurrent scan resolved a fresher sidecar — is left alone, and the count of refreshed covers comes from `rows_affected` so a skipped album never inflates the scan summary.

A tag edit that rewrites the audio file is normally detected, since it moves that file's mtime — this pass is specifically about the sidecar case. **But that is not guaranteed**: taggers commonly offer to preserve the modification date (Mp3tag ships that behaviour), and an ID3 rewrite often fits in the existing padding, so `size` doesn't move either. The file then looks untouched to the fast path and its new tags are never read — reported as issue #457 for a batch of genre edits.

### Deep rescan

A **deep rescan** bypasses `(mtime, size)` entirely and re-hashes + re-reads every file. It is the escape hatch for exactly the case above, and it is opt-in because it costs a full re-read of the library.

Two entry points, in different places:

- **per folder** — the magnifier button on a folder row, under **My music → Folders** (`scan_folder` with `deep: true`). Appears on row hover;
- **whole library** — the second button next to Rescan in the **My music header**, so it is reachable from any tab, not just Folders (`rescan_library` with `deep: true`). Added in issue #457: until then the bypass existed per folder only, so the library-wide button users actually reach for could never see mtime-preserving edits.

Both are mutually exclusive with each other and with a plain rescan — `scan_folder_inner` writes, and SQLite takes one writer at a time.

Note the interaction with **Split this artist** below: a deep rescan re-reads tags authoritatively and will undo an in-place split, since it no longer sees the split as deliberate.

## Audio analysis

[`analysis.rs`](../../src-tauri/crates/core/src/analysis.rs) computes peak, integrated loudness and BPM (autocorrelation). Loudness is **ITU-R BS.1770-4** — K-weighted and gated ([`analysis/loudness.rs`](../../src-tauri/crates/core/src/analysis/loudness.rs)) — so the number is real LUFS and lands on the same scale as the ReplayGain other taggers write; the earlier unweighted RMS over a mono sum was a fine relative yardstick inside one library but could not be compared with anything outside it. ReplayGain is `-18 LUFS - loudness`, and is `NULL` rather than a huge boost for a track with nothing above the absolute gate. Peak is taken across **every channel**, not over a mono downmix — an out-of-phase mix sums to near silence while its samples sit at full scale, and clipping prevention downstream depends on that number being true. Runs on demand (per track) or as a background sweep (whole library), gated by a Settings toggle. Results land in `track_analysis` and feed:

- per-stream gain in the audio engine (`replaygain_enabled`) — as the **fallback** source, behind the file's own tags (see [playback](playback.md#replaygain)),
- the BPM bucketing in [smart playlists](smart-playlists.md),
- the per-track audio specs strip under the player.

Rows analysed before the BS.1770 switch are **left in place**: they hold the old unweighted RMS figure, which is a few dB off the new scale on some material, so a track analysed back then and a track carrying a `REPLAYGAIN_*` tag can differ slightly in level. Nothing is invalidated automatically — deleting a user's analysis results to force a re-run is a worse trade than the residual mismatch, which clipping prevention bounds anyway. Re-analysing a track (or a sweep over the library) replaces the value with the real LUFS one.

The background sweep ([`run_analyze_library`](../../src-tauri/crates/app/src/commands/analysis.rs)) is deliberately yielded to the foreground scanner: a scan saturates every CPU core and the single SQLite writer, so the analyzer parks itself while [`scan_in_flight()`](../../src-tauri/crates/app/src/commands/scan.rs) reports an active walk (any of `scan_folder` / `rescan_library` / `import_paths` / the fs-watcher / the startup rescan). It resumes once the scan drains. Decoded results are buffered and flushed to `track_analysis` in batches of 16 inside one transaction, and the flush retries the whole batch on `SQLITE_BUSY` / `SQLITE_LOCKED` with exponential backoff — before this, a per-row `INSERT` racing a concurrent scan would hit `database is locked` after the 5 s busy-timeout and silently drop the freshly-computed BPM / loudness.

## Multi-artist

The scanner splits multi-artist tag values on `"; "` only — the convention used by MusicBrainz Picard, foobar2000, Beets and Mp3Tag for multi-value artist fields. `"Artist A; Artist B"` becomes two `artist` rows linked to the track via the `track_artist` many-to-many table with a `position` column for stable ordering. `", "` is deliberately NOT a separator because a comma can be part of an artist name (`"Tyler, The Creator"`, `"Earth, Wind & Fire"`, `"Crosby, Stills, Nash & Young"`); the earlier comma-split silently fragmented those into multiple artists. Libraries that comma-joined their multi-artist fields will see those tracks under the combined-name **phantom** artist until re-tagged with `"; "`. Queries rebuild the display string with `GROUP_CONCAT(...) ORDER BY position`. The `ArtistLink` React component receives parallel `artist_name` + `artist_ids` strings so every contributor is individually clickable, matching Spotify's behaviour.

**Split this artist** (issue #396) is the escape hatch for a comma-joined library that can't be re-tagged. The phantom's Artist Detail → _Edit info_ modal shows a "Split" section (only when the name contains a comma) previewing the comma fragments; confirming calls [`split_artist`](../../src-tauri/crates/app/src/commands/artist_split.rs), which re-links every track that credits the phantom to the individual artists — **reusing existing rows by canonical name**, so the track immediately points at the already-enriched artist row rather than a fresh one — repoints `track.primary_artist`, and deletes the phantom once nothing references it (guarded on `track_artist`, `album.artist_id`, `track.primary_artist` and `artist_similar_custom` so it never SET-NULLs an album artist or CASCADEs a curated similar list). No file is re-tagged. Durability: the scanner's skip-branch re-normalisation treats a comma-joined tag (one `"; "`-split name) whose track already credits several artists as a deliberate split and leaves it alone, so a normal unchanged-file rescan won't collapse it back into the phantom. A **deep rescan** re-reads tags authoritatively and will re-create the phantom — re-tag with `"; "` for a fix that survives that too.

## Album grouping

Albums are keyed on **`(canonical_title, album_artist_id)`**, not on the title alone — otherwise every "Greatest Hits" in the library would collapse into one row. [`scan.rs::upsert_album`](../../src-tauri/crates/app/src/commands/scan.rs) resolves the album artist in that order:

1. the **Album Artist** tag, when present;
2. the `is_compilation` flag ⇒ the `"Various Artists"` sentinel;
3. the track's primary artist, as a fallback.

`album.is_compilation` is **sticky**: once an album is marked as a compilation it stays one, so a later file whose tags disagree can't split the album in half. On top of that, `merge_implicit_compilations` runs after every scan and collapses same-title rows credited to ≥ 3 distinct artists into a single `"Various Artists"` album — the common shape of a soundtrack or a sampler ripped without an Album Artist tag.

Tag edits go through the same funnel: [`edit.rs`](../../src-tauri/crates/app/src/commands/edit.rs) re-runs `upsert_album` with the **old** album's Album Artist + compilation flags, so renaming an album (or fixing one track's title) re-groups the tracks instead of spawning a second row.

## Browsing

- **Library tabs** — Morceaux, Albums, Artistes, Genres, Playlists, Dossiers; each tab keeps its own scroll position and sort memory (per profile). Five of the six fetch their own data; **Playlists reads `PlaylistContext`** instead ([`PlaylistGrid`](../../src/components/views/library/PlaylistGrid.tsx), issue #461), so it has no query, no loading state, and no skeleton — the sidebar has already loaded the same rows. It shows **user playlists only** (`is_smart === 0`); the generated ones stay in Home's "Made for you" carousel, matching how `HomeView` filters them. Tiles are row-virtualized against the page scroller like the albums grid, sorted by the sidebar's manual order (`playlist.position`) by default, and fall back to the playlist's icon + colour tile when it has no cover — the same visual the sidebar row uses, so one playlist looks the same in both places. The remaining five tab queries (`list_tracks` / `list_albums` / `list_artists` / `list_genres` / `list_folders`) fire in parallel on first mount so subsequent tab switches hit cached React state instantly instead of paying a fresh SQL round-trip; the first paint shows a layout-shaped `LibraryTabSkeleton` (`role="status"` / `aria-busy="true"`) until the data lands, never the EmptyState. Browse queries lean on partial indexes `idx_track_album_available` / `idx_track_primary_artist_available` (`WHERE is_available = 1`) so the GROUP BY aggregates stay index-only on healthy libraries. Clicking a genre tile opens a Spotify-style genre detail page (`get_genre_detail` in [`browse.rs`](../../src-tauri/crates/app/src/commands/browse.rs)) with every track tagged with that genre, sorted Artist → Album → Disc → Track. **Manual genre picture** (issue #424): genres have no automatic/embedded artwork source of their own, so `genre.artwork_id` only gets a value when the user sets one — a `Pencil` overlay on the grid tile and the detail-page header opens [`GenreImagePickerModal`](../../src/components/common/GenreImagePickerModal.tsx) (`set_genre_artwork_from_file` / `clear_genre_artwork`), which validates the file's magic bytes (jpg/png/webp, shared `detect_image_format` helper) and stores it hash-addressed in the same per-profile `artwork/` dir as album/artist covers, tagged `source = 'manual'`.
- **Bulk list endpoints wire format** — `list_tracks` / `list_playlist_tracks` / `list_liked_tracks` (track-shaped) and `list_albums` / `list_artists` (browse-shaped) all return `{ artwork_base, items: <Slim>[] }` instead of the full row shape (artists additionally include `metadata_artwork_base` for the Deezer cache). Each slim row carries `artwork_hash` + `artwork_format` + `artwork_has_1x` + `artwork_has_2x` (artists also `picture_hash` + `picture_has_*`) instead of three full path strings; the ~70-char per-profile prefix only appears once in the response. Frontend wrappers ([`expandTrackResponse`](../../src/lib/tauri/track.ts), `expandAlbumRow` / `expandArtistRow` in [`browse.ts`](../../src/lib/tauri/browse.ts)) stitch the absolute paths back together so every UI consumer keeps the full `Track` / `AlbumRow` / `ArtistRow` shape unchanged. Cuts ~30 % off each payload (e.g. ≈ 1.0 MB → ≈ 700 kB on a 1k-track `list_tracks`, ≈ 650 kB → ≈ 250 kB on a 900-artist `list_artists`), proportionally shrinking JSON parse + IPC transfer time. Any future bulk endpoint shipping artworks for hundreds of rows should adopt the same `{ artwork_base, items }` shape.
- **A-Z navigator** — letter rail on the artists tab, NFD-normalised so accents (É → E, Ñ → N) bucket correctly.
- **Multi-select** — ctrl/shift across rows with a floating action bar (Play / Add to queue / Add to playlist / Remove) anchored to the bottom of the viewport.
- **Track Properties dialog** — foobar2000-style modal with the full tag set, audio specs, analysis results, file path and a Show in Explorer button.
- **POPM ratings** — 5-star with half-steps, round-tripped to the file's tag. Edit surfaces: inline `StarRating` in the library track list, integer-star submenu in the right-click `TrackContextMenu` (any view), full half-star widget in the `TrackPropertiesModal`. The backend command `set_track_rating` writes the POPM frame back to the file (binary `<email>\0<rating><counter>` for ID3v2, text `RATING=0-100` for Vorbis / MP4 / APE), updates `track.rating` in the DB, then emits `track:updated` so every open view re-fetches without polling. Containers lofty can't open (DSD) keep a DB-only rating; the next folder scan still preserves it because the fast-path skip on `(mtime, size)` never re-extracts unchanged files. Smart playlists expose this as the `rating_min` rule — see [smart-playlists.md](smart-playlists.md#custom-smart-playlists-recursive-boolean-rule-tree).
- **Lightbox** — double-click any cover or artist photo to view full-size with keyboard navigation.

## Tag editing

[`commands/edit.rs`](../../src-tauri/crates/app/src/commands/edit.rs) writes the file first, then mirrors the change into the database (`track` / `album` / `artist` / `track_artist` / `track_genre`) inside one transaction, re-hashes the file and emits `track:updated` + `library:rescanned` + `player:queue-changed`.

**Every write goes through `patch_file`**, which reads the *concrete* lofty file type rather than the generic `TaggedFile`. See the [invariant](../architecture/invariants.md#tag-writes-go-through-the-concrete-tag) for why: the generic round trip drops non-standard Vorbis comments, so editing a FLAC's title used to erase the `SYNCEDLYRICS` our own lyrics editor had written into it. The same function serves the cover write, which replaces the front cover and leaves a release's booklet, back cover and artist shots alone.

`.dsf` / `.dff` are refused before anything is touched — lofty has no DSD reader, so there is no write path to fall back on. The dialog shows the reason instead of failing silently.

**The genre is fetched separately.** It lives in `track_genre`, not on the `Track` row, so [`TrackPropertiesModal`](../../src/components/common/TrackPropertiesModal.tsx) loads it through `get_track_genres` and pre-fills the input. It has to: the dialog sends every field on save, and a genre input that opened empty read as "the user cleared it" — erasing the value from the file *and* from the database, with no rescan able to recover it. Until that fetch lands the field is omitted from the payload entirely rather than sent empty. The batch editor ([`BatchTagEditModal`](../../src/components/common/BatchTagEditModal.tsx)) never had the problem: it sends only the fields the user explicitly enabled.

A failed save is rendered in the dialog rather than logged to the console.

## Search

FTS5 contentless index over `title`, `artist`, `album` with prefix matching. Auto-sync triggers (`AFTER INSERT/UPDATE/DELETE` on `track`) keep the index current using the `'delete'` command on the contentless table. Queries are issued from the React top bar with a 250 ms debounce.

The top-bar dropdown shows **sectioned results — Artists / Albums / Titles** (Spotify-style). FTS is track-scoped, so the album/artist sections come from dedicated `search_albums` / `search_artists` commands ([`browse.rs`](../../src-tauri/crates/app/src/commands/browse.rs)) that substring-match the query's `canonical_name` form against `album.canonical_title` / `artist.canonical_name` (prefix matches rank first) and return the same slim `{ artwork_base, items }` shape as `list_albums` / `list_artists`. The three entities fan out in one `Promise.all`; clicking an artist or album row navigates to `ArtistDetailView` / `AlbumDetailView` (via the `onNavigateToArtist` / `onNavigateToAlbum` callbacks), while a title row plays the track. The advanced filter panel is track-only — when **any** advanced filter is active the album/artist sections are suppressed and only `search_tracks_advanced` runs.

## Folder management

[`commands/library.rs`](../../src-tauri/crates/app/src/commands/library.rs) exposes the watch-folder lifecycle: `add_folder_to_library`, `set_folder_watched` (toggle the in-memory `notify` watcher), and `remove_folder_from_library`. The remove path detaches the watcher, deletes every track that lived under the folder, then drops the `library_folder` row in a single transaction. The schema's `track.folder_id ON DELETE SET NULL` would otherwise leave orphan tracks with `library_id` still set — making the user "remove" a folder while its tracks stayed in the library, which never matches what they expect.

UI: per-folder trash button in the Library → Folders tab, two-step confirm-on-second-click that auto-clears after 3 s.

## Drag-and-drop import

[`hooks/useDragDropImport.ts`](../../src/hooks/useDragDropImport.ts) wires Tauri 2's window-level `onDragDropEvent` into the existing import flow via a single backend command: [`commands/library.rs::import_paths`](../../src-tauri/crates/app/src/commands/library.rs). The command accepts a mix of folders and audio files — files contribute their parent directory — dedupes the resolved folder set, then for each one tries an `INSERT OR IGNORE INTO library_folder` (the `(library_id, path)` UNIQUE constraint absorbs duplicates) and runs `scan_folder_inner`. Aggregated `ScanSummary` is returned to the frontend so the user sees one toast with the total counts.

Auto-creates a default library on the very first drop when the profile has none, mirroring the existing pickFolder import path.

UI: emerald drop overlay in [`AppLayout`](../../src/components/layout/AppLayout.tsx) renders a fade-in border + drop hint while the user is dragging, then a spinner while the backend scan runs. `pointer-events: none` on the overlay so the drop still hits Tauri's native handler.

## Duplicate detection

[`commands/duplicates.rs::find_duplicates`](../../src-tauri/crates/app/src/commands/duplicates.rs) surfaces byte-identical copies in different folders regardless of metadata: it prefilters candidates by `file_size`, then groups them with a full-content BLAKE3 hash ([`scanner::hash_file_full`](../../src-tauri/crates/core/src/scanner/extract.rs)) — _not_ by the scan-time `file_hash` directly. The scan-time hash is **partial** — file size + first 1 MiB + last 1 MiB ([`scanner::hash_file`](../../src-tauri/crates/core/src/scanner/extract.rs)) rather than every byte — because full-file hashing was the dominant scan cost (reading ~9 GB to scan 900 tracks). Byte-identical copies still collide (same bytes → same digest), and a tag rewrite still changes the digest (ID3v2 head / ID3v1·APE tail sit inside the window). Because a partial digest _could_ in theory collide on the unread middle bytes — and because a legacy full hash and a newer partial hash for the _same_ file never match (both are 64-char blake3 hex, indistinguishable) — `find_duplicates` prefilters candidates by **byte size** (a format-stable field every real duplicate shares) and then re-verifies each candidate with a **full-content** hash ([`scanner::hash_file_full`](../../src-tauri/crates/core/src/scanner/extract.rs)) — computed off-thread, only on the handful of same-size files — before returning a group. The destructive delete therefore only ever sees byte-identical files, regardless of when each row was scanned. Re-encodes of the same source — e.g. CBR vs VBR rips — **won't** match because the bytes differ; that's a fingerprinting problem and out of scope for the MVP.

`find_duplicates` returns one entry per group, ordered by `added_at ASC` so the oldest copy renders first (usually the one to keep). `delete_tracks(track_ids)` cascades through the schema's `ON DELETE` constraints to clean up `track_artist`, `track_genre`, `playlist_track`, `play_event`, etc. — but **the audio files on disk are not touched**: the user removes them via the OS so we don't accidentally wipe a backup.

UI: [`DuplicatesModal`](../../src/components/common/DuplicatesModal.tsx) launched from Settings → Stockage → "Rechercher". Each group exposes a radio selector (defaults to oldest) and the footer's "Supprimer N doublons" wipes every other entry from the database.

## Cover picker

[`commands/deezer.rs::set_album_artwork_from_deezer`](../../src-tauri/crates/app/src/commands/deezer.rs) and `set_album_artwork_from_file`. The file picker validates magic bytes (JPEG / PNG / WebP) before accepting upload, and `batch_fetch_missing_album_covers` walks all albums without an `artwork_id`, querying Deezer in parallel with a small concurrency cap.

## Local artist images

Scanner sidecar lookup, mirror of the folder-cover fallback but resolved against the track's ancestors instead of the immediate parent.

[`commands/scan.rs::extract_artist_image`](../../src-tauri/crates/app/src/commands/scan.rs) walks up to **3 parent directories** from each track and accepts the first match where either:

- the filename stem is one of `ARTIST_IMAGE_STEMS = ["artist", "performer", "band"]`, **or**
- the stem's `canonical_name(...)` equals the artist's canonical name (covers `Daft Punk.jpg` at the root of a `Daft Punk/` folder).

Both common layouts from issue #31 work out of the box:

- `Music/<Artist>/<Album>/track.flac` → matches `artist.jpg` two levels up.
- `Music/<Album>/track.flac` → matches `<Artist>.jpg` sitting beside the album folder (strict name-match so an unrelated `cover.jpg` is never mistaken for an artist photo).

Hash-addressed via BLAKE3 into the shared `artwork/<hash>.{jpg,png,webp,…}` cache and linked through the existing `artist.artwork_id → artwork` foreign key (no schema change). The `UPDATE … WHERE artwork_id IS NULL` guard means scanner runs never overwrite a manually uploaded image or a previously cached Deezer picture.

Resolution priority in [`commands/browse.rs::get_artist_detail`](../../src-tauri/crates/app/src/commands/browse.rs) is now: **local sidecar → Deezer cache → live Deezer fetch** (last skipped when offline). [`ArtistDetailView`](../../src/components/views/ArtistDetailView.tsx) prefers `artwork_path` over `picture_path` and refuses to clobber a local image with a late-arriving Deezer response.

The `"Various Artists"` sentinel is skipped by the per-track pass because it's an _album_ artist — it's written to `album.artist_id` (never to `track_artist`), so the per-track join can't reach it. It's handled separately by [`scanner::link_va_artist_image`](../../src-tauri/crates/core/src/scanner/upserts.rs), which resolves a curated `Various Artists/artist.jpg` (or `Various Artists.jpg`) via the album relationship (issue #292). Because `extract_artist_image` only matches an explicit artist-named sidecar — never a generic `cover.jpg` / `folder.jpg` — VA still never inherits a stray album cover. The helper runs at the end of every scan (after `merge_implicit_compilations`) and inside the rescan below.

For libraries scanned before the feature shipped, [`commands/scan.rs::rescan_local_artist_images`](../../src-tauri/crates/app/src/commands/scan.rs) (exposed as **Settings → Library → Local artist images**) walks every `artist WHERE artwork_id IS NULL` and probes up to 16 tracks per artist with `extract_artist_image`, stopping at the first hit (plus a dedicated VA pass via the album relationship). Already-linked rows are filtered out at the SQL level, so the rescan is cheap to re-run.

### Manual override

The pencil overlay on the artist photo in [`ArtistDetailView`](../../src/components/views/ArtistDetailView.tsx) opens [`ArtistImagePickerModal`](../../src/components/common/ArtistImagePickerModal.tsx), which exposes three actions backed by [`commands/deezer.rs`](../../src-tauri/crates/app/src/commands/deezer.rs):

- **Search Deezer** → `search_artists_deezer` + `set_artist_artwork_from_deezer` (downloads the chosen picture into the profile artwork cache, marks source `"deezer"`).
- **Pick a local file** → `set_artist_artwork_from_file` (same magic-byte validation as the album cover picker: jpg / png / webp).
- **Remove image** → `clear_artist_artwork` sets `artist.artwork_id = NULL` so the next render falls back through the resolution chain (Deezer cache → live fetch).

Both `set_artist_artwork_from_*` overwrite `artwork_id` unconditionally — an explicit user pick beats any automatic resolution.

### Wide artist fanart (hero)

Everything above carries the **square** artist photo. The [artist hero](ui.md#artist-hero) needs a **wide** one, and the only source in the stack that has one is TheAudioDB (issue #482).

[`metadata::theaudiodb`](../../src-tauri/crates/core/src/metadata/theaudiodb.rs) already queried `search.php` for multi-language bios; the same response carries `strArtistFanart` (+ `2/3/4`), `strArtistWideThumb` and `strArtistBanner`. `TheAudioDbClient::artist_info` now returns bio **and** `fanart_url` from one lookup, picking the first non-blank image widest-and-cleanest first (fanart → alternates → wide thumb → logo banner last, since baked-in text can clash with the header copy). It returns `Some` for any name match even with neither bio nor fanart, so the caller can cache the "looked, nothing there" outcome.

[`enrich_artist_deezer`](../../src-tauri/crates/app/src/commands/deezer.rs) calls it **independently of the `metadata.bio_source` setting**: Last.fm has no equivalent image, so gating the fanart on the bio source would leave every Last.fm user with no hero at all. One request serves both consumers (the bio half is used only when TheAudioDB _is_ the selected source) — TheAudioDB's shared free key is rate-limited, so it's one call, cached hard. The URL is downloaded through the usual `metadata_artwork::download_and_cache` (BLAKE3-addressed, shared across profiles) and kept at **full resolution** — no `_1x` / `_2x` tier, downscaling a full-bleed banner would only soften it. Offline mode short-circuits before any of this, and the blurred-photo tier still works.

Cached in `app.metadata_artist` next to the picture pair: `background_url` + `background_hash`, plus **`background_fetched_at`** — the "we already looked" marker (migration `20260802120000_metadata_artist_background.sql`). Without it a NULL hash can't be told apart from "never queried", so every artist without fanart would re-hit a rate-limited API on each page visit. It is stamped whenever the API was _reached_ (match or not) and left NULL on a transport error, so a network blip retries instead of caching as "this artist has no fanart" for the row's whole 30-day TTL. Rows written before the migration have NULL there and are treated as a background cache miss on their next refresh, which backfills them once.

Both `get_artist_detail` (first paint, straight from the cache) and `enrich_artist_deezer` (refresh) return `background_url` / `background_path`.
