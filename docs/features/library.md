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

- **Library tabs** — Morceaux, Albums, Artistes, Genres, Dossiers; each tab keeps its own scroll position and sort memory (per profile).
- **A-Z navigator** — letter rail on the artists tab, NFD-normalised so accents (É → E, Ñ → N) bucket correctly.
- **Multi-select** — ctrl/shift across rows with a floating action bar (Play / Add to queue / Add to playlist / Remove) anchored to the bottom of the viewport.
- **Track Properties dialog** — foobar2000-style modal with the full tag set, audio specs, analysis results, file path and a Show in Explorer button.
- **POPM ratings** — 5-star with half-steps, read from tags at scan time and editable inline.
- **Lightbox** — double-click any cover or artist photo to view full-size with keyboard navigation.

## Search

FTS5 contentless index over `title`, `artist`, `album` with prefix matching. Auto-sync triggers (`AFTER INSERT/UPDATE/DELETE` on `track`) keep the index current using the `'delete'` command on the contentless table. Queries are issued from the React top bar with a 150 ms debounce.

## Cover picker

[`commands/deezer.rs::set_album_artwork_from_deezer`](../../src-tauri/src/commands/deezer.rs) and `set_album_artwork_from_file`. The file picker validates magic bytes (JPEG / PNG / WebP) before accepting upload, and `batch_fetch_missing_album_covers` walks all albums without an `artwork_id`, querying Deezer in parallel with a small concurrency cap.
