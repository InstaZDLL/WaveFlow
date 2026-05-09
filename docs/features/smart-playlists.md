# Smart playlists

Auto-generated playlists materialised from the user's listening history. Today: a 3-slot **Daily Mix** family bucketed by tempo. Tomorrow: "On Repeat", "Repeat Rewind", per-mood mixes — the engine in [`smart_playlists/`](../../src-tauri/src/smart_playlists) is built around a discriminated `SmartPlaylistRules` enum so new families plug in without touching the regen flow.

## Storage

Smart playlists share the regular `playlist` table with user playlists. Three columns matter:

| Column | Role |
|--------|------|
| `is_smart` (`INTEGER`, default `0`) | Filter flag for the UI. |
| `smart_rules` (`TEXT`) | JSON payload — `SmartPlaylistRules` enum. The regenerator looks up an existing slot via `LIKE '%"slot":N%'` so an upsert rewrites the same row instead of stacking duplicates. |
| `cover_hash` (`TEXT`, added in [migration `20260509000000`](../../src-tauri/migrations/profile/20260509000000_playlist_cover_hash.sql)) | Blake3 hash of the composite cover, looked up in the shared `<root>/metadata_artwork/<hash>.jpg` cache. `NULL` → frontend falls back to the `icon_id` + `color_id` gradient tile. |

The Tauri layer additionally returns a derived `cover_path: Option<String>` resolved by [`metadata_artwork::existing_path`](../../src-tauri/src/metadata_artwork.rs) so a stale `cover_hash` (cache wiped, file gone) doesn't render a broken image.

## Daily Mix algorithm

Implemented in [`generator.rs`](../../src-tauri/src/smart_playlists/generator.rs). Inputs are read from the active profile's database; outputs are three playlists named `Daily Mix 1` / `2` / `3`.

### 1. Top artists window

```sql
SELECT a.id,
       SUM(pe.listened_ms),
       AVG(ta.bpm)
  FROM play_event pe
  JOIN track t           ON t.id  = pe.track_id
  JOIN track_artist ta2  ON ta2.track_id = t.id AND ta2.position = 0
  JOIN artist a          ON a.id  = ta2.artist_id
  LEFT JOIN track_analysis ta ON ta.track_id = t.id
 WHERE pe.played_at >= now() - 90 days
   AND t.is_available = 1
 GROUP BY a.id
 ORDER BY SUM(pe.listened_ms) DESC
 LIMIT 60
```

Lookback is **90 days** so a one-off binge doesn't dominate forever. Below `MIN_ARTISTS = 6` distinct artists the regenerator skips entirely (better empty than degenerate three-shuffles-of-the-same-thing).

### 2. Tempo bucketing

Each artist is routed to **one** bucket based on the average BPM of their tracks:

| Slot | Label | BPM range | Description |
|------|-------|-----------|-------------|
| 1 | Daily Mix 1 (Calm) | `< 95` | Lower-tempo / ambient |
| 2 | Daily Mix 2 (Groove) | `[95, 130)` | Mid-tempo, where most pop / rock / hip-hop sits |
| 3 | Daily Mix 3 (Energy) | `≥ 130` | High-energy, dance, drum & bass |

Artists with **no** analysed BPM fall back to slot 2. Same for ties — a missing analysis doesn't black-hole an artist.

### 3. Track picking

For each bucket: take the top 12 artists, fetch up to 200 of their tracks ordered by `play_count DESC, t.id ASC`, deterministic-shuffle (xorshift seeded with `SHUFFLE_SEED ^ slot`), truncate to 50.

Determinism matters: the same input set always produces the same listening order, so the user doesn't see a "different mix" mid-session if the playlist re-renders. A second regen against the same listening data rewrites the rows in place.

### 4. Cover composition

[`cover.rs`](../../src-tauri/src/smart_playlists/cover.rs):

- **Image source priority** — try the top 3 artists' Deezer pictures first (shared `metadata_artwork/<hash>.jpg` cache, looks best because portraits crop cleanly). If none of the cluster's artists are Deezer-enriched (common with niche / soundtrack libraries), fall back to **album artwork of the first 3 shuffled tracks** from the per-profile cache (`<root>/profiles/<id>/artwork/<hash>.<format>`). The fallback is what guarantees a real cover even for libraries dominated by obscure artists.
- **Layout auto-pick** — the `build_composite_cover` entry point dispatches by input count: 1 → fill the canvas, 2 → vertical halves, 3 → 3 strips (the Daily Mix look), 4+ → **2×2 grid** (Spotify-style auto-playlist cover, used by the user-playlist auto-cover pipeline; smart playlists never reach this branch since they cap at 3 artist pictures).
- 640×640 RGB canvas → centre-crop each source via `cover_fit` (matches CSS `object-fit: cover`) → SIMD resize via `fast_image_resize 6` → paint.
- Apply a `t²` ease-out gradient over the bottom 40 % so the React-rendered "Daily Mix N" label stays legible without baking text into the JPEG.
- Encode JPEG q=85 → blake3 → write to `metadata_artwork/<hash>.jpg`.

Why the label isn't rasterised in Rust: avoids a font dep (`ab_glyph` / `fontdue` + a bundled TTF) and lets the frontend re-style / re-translate without regenerating images.

## Regen flow

```
User clicks "Régénérer" on HomeView
  → invoke('regenerate_daily_mixes')
    → smart_playlists::generator::regenerate_daily_mixes(pool, paths)
      ├── top_artists_with_bpm()        ← profile DB + cross-DB lookup to app.db for picture_hash
      ├── for each bucket:
      │     ├── pick_tracks_for_artists()
      │     ├── shuffle_with_seed()
      │     ├── cover::build_daily_mix_cover()  ← only if any cached picture exists
      │     └── upsert_smart_playlist()         ← LIKE-match on smart_rules to refresh in place
      └── returns Vec<i64> of refreshed playlist ids
  → frontend usePlaylist().refresh() → sidebar + home carousel update
```

The cross-DB lookup in `top_artists_with_bpm` opens a short-lived secondary connection to `app.db` rather than `ATTACH`ing it — easier than routing through the same pool the Deezer commands use, and the lookup runs once per regen so the per-call cost is negligible.

## UI

- **Sidebar** ([`Sidebar.tsx`](../../src/components/layout/Sidebar.tsx)): when `cover_path` is set, the playlist row swaps its icon-tile for an 8 × 8 image. Smart playlists sort to the top of the playlist list because their `position` matches their `slot` (1-3) while user playlists default to 0.
- **HomeView** ([`HomeView.tsx`](../../src/components/views/HomeView.tsx)): a "Pour vous" section above "Récemment joués" renders 16:7 cards with the cover as background, a black gradient, the `DAILY MIX` label, the playlist name and the track count. Empty state when `smartPlaylists.length === 0`.
- **Regen button**: violet "Régénérer" with a spinning `RefreshCw` while in flight. Calls `regenerateDailyMixes()` then `refresh()` on the playlist context.

## Manual edits

A user adding or removing a track from a Daily Mix **will lose the change on the next regen**: `upsert_smart_playlist` deletes every `playlist_track` row for the playlist before re-inserting the freshly shuffled set. If durable curation is needed, the right move is to "Save as new playlist", which copies the tracks into a fresh `is_smart = 0` row.
