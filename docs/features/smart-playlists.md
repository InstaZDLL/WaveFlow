# Smart playlists

Auto-generated playlists materialised from the user's listening history. Today: a 3-slot **Daily Mix** family bucketed by tempo, plus a single **On Repeat** playlist tracking the user's top played tracks over the last 30 days. Tomorrow: "Repeat Rewind", "Release Radar", per-mood mixes — the engine in [`smart_playlists/`](../../src-tauri/src/smart_playlists) is built around a discriminated `SmartPlaylistRules` enum so new families plug in without touching the regen flow.

## Storage

Smart playlists share the regular `playlist` table with user playlists. Three columns matter:

| Column                                                                                                                                  | Role                                                                                                                                                                              |
| --------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `is_smart` (`INTEGER`, default `0`)                                                                                                     | Filter flag for the UI.                                                                                                                                                           |
| `smart_rules` (`TEXT`)                                                                                                                  | JSON payload — `SmartPlaylistRules` enum. The regenerator looks up an existing slot via `LIKE '%"slot":N%'` so an upsert rewrites the same row instead of stacking duplicates.    |
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

| Slot | Label                | BPM range   | Description                                     |
| ---- | -------------------- | ----------- | ----------------------------------------------- |
| 1    | Daily Mix 1 (Calm)   | `< 95`      | Lower-tempo / ambient                           |
| 2    | Daily Mix 2 (Groove) | `[95, 130)` | Mid-tempo, where most pop / rock / hip-hop sits |
| 3    | Daily Mix 3 (Energy) | `≥ 130`     | High-energy, dance, drum & bass                 |

Artists with **no** analysed BPM fall back to slot 2. Same for ties — a missing analysis doesn't black-hole an artist.

### 3. Track picking

For each bucket: take the top 12 artists, fetch up to 200 of their tracks ordered by `play_count DESC, t.id ASC`, deterministic-shuffle (xorshift seeded with `SHUFFLE_SEED ^ slot`), truncate to 50.

Determinism matters: the same input set always produces the same listening order, so the user doesn't see a "different mix" mid-session if the playlist re-renders. A second regen against the same listening data rewrites the rows in place.

### 4. Cover composition

[`cover.rs`](../../src-tauri/src/smart_playlists/cover.rs):

- **Image source priority** — try the top 3 artists' Deezer pictures first (shared `metadata_artwork/<hash>.jpg` cache, looks best because portraits crop cleanly). If none of the cluster's artists are Deezer-enriched (common with niche / soundtrack libraries), fall back to **album artwork of the first 3 shuffled tracks** from the per-profile cache (`<root>/profiles/<id>/artwork/<hash>.<format>`). The fallback is what guarantees a real cover even for libraries dominated by obscure artists.
- **Layout auto-pick** — the `build_composite_cover` entry point dispatches by input count: 1 → fill the canvas, 2 → vertical halves, 3 → 3 strips (the Daily Mix look), 4+ → **2×2 grid** (Spotify-style auto-playlist cover, used by the user-playlist auto-cover pipeline; smart playlists never reach this branch since they cap at 3 artist pictures).
- **Identical-input dedup** — the compositor dedupes incoming paths before counting, so a Daily Mix whose top 3 artists share a Deezer picture (or whose fallback album arts all point at the same release) collapses to a single full-canvas tile instead of a contact-sheet of identical thumbnails. Mirrors the hash-level dedup that `playlist_cover::top_track_artwork_paths` applies for user playlists — both caches are hash-keyed (`metadata_artwork/<blake3>.jpg` + per-profile `artwork/<hash>.<ext>`), so path equality is content equality.
- 640×640 RGB canvas → centre-crop each source via `cover_fit` (matches CSS `object-fit: cover`) → SIMD resize via `fast_image_resize 6` → paint.
- Apply a `t²` ease-out gradient over the bottom 40 % so the React-rendered "Daily Mix N" label stays legible without baking text into the JPEG.
- Encode JPEG q=85 → blake3 → write to `metadata_artwork/<hash>.jpg`.

Why the label isn't rasterised in Rust: avoids a font dep (`ab_glyph` / `fontdue` + a bundled TTF) and lets the frontend re-style / re-translate without regenerating images.

## On Repeat algorithm

Implemented in [`on_repeat.rs`](../../src-tauri/src/smart_playlists/on_repeat.rs). Single playlist, no slot bucketing — the top tracks the user has rotated the most over the last 30 days, ordered by play count descending. The materialised playlist holds **up to `TRACKS_LIMIT = 30` tracks**; the SQL fetches up to 60 candidates first so the Rust caller has headroom to filter for future variants (e.g. dropping tracks already on another smart playlist) without re-issuing the query, then truncates client-side via `tracks.iter().take(TRACKS_LIMIT)` before the upsert.

```sql
SELECT pe.track_id,
       COUNT(pe.id) AS play_count
  FROM play_event pe
  JOIN track t ON t.id = pe.track_id
 WHERE pe.played_at >= now() - 30 days
   AND t.is_available  = 1
 GROUP BY pe.track_id
HAVING play_count > 0
 ORDER BY play_count DESC, MAX(pe.played_at) DESC
 LIMIT 60          -- candidate pool; truncated to TRACKS_LIMIT = 30 in Rust
```

Differences vs Daily Mix:

- **Lookback is 30 days** (not 90) so the playlist reflects the _current_ rotation, not last quarter's binges. Matches Spotify's On Repeat cadence.
- **No shuffle** — the playlist is the top-N tracks in straight play-count order, so the user's #1 most-played song lands at the top. Deterministic by construction, no seed needed.
- **No tempo bucketing** — On Repeat is a single playlist, not a 3-way split.
- **Minimum 8 distinct tracks** in the window before anything is materialised. Below that the playlist would be "the same handful of songs you already listened to a lot" and adds nothing over the History view. A previously-materialised row is _deleted_ when this guard kicks in so a stale playlist doesn't linger after a quiet month.
- **Position is fixed at 0** so the row sorts ahead of every Daily Mix slot in the Home carousel and the sidebar.

### Brand cover

On Repeat doesn't use the album-art composite — its identity is a fixed visual rendered deterministically by [`cover::build_on_repeat_cover`](../../src-tauri/src/smart_playlists/cover.rs): a 640×640 JPEG composited from the embedded SVG at [`on_repeat.svg`](../../src-tauri/src/smart_playlists/on_repeat.svg) — deep indigo → near-black diagonal gradient, a faint vertical-bar equaliser motif at 4 % opacity, and a centred bezier infinity loop stroked with a `#ff3377 → #9933ff → #33ccff` gradient under a gaussian glow filter with a thin white inner-rim. Rasterised through `resvg + usvg + tiny-skia` (default features disabled — no font / raster-image bloat since the SVG is shape + gradient + filter primitives only), so the curves anti-alias at the canvas resolution instead of looking like the old hand-rolled pixel grid. Same bytes every regen → same blake3 hash → the cache dedupes against the existing file instead of piling up orphans. No `<text>` baked in (the playlist name and "On Repeat" eyebrow are rendered by React on top of the tile), so the canvas stays locale-agnostic.

Future per-track-art families (Release Radar, Recently Added) will resolve the _first track's artist image_ instead — that's why [`generator::first_track_artwork_paths`](../../src-tauri/src/smart_playlists/generator.rs) is `pub(super)` even though On Repeat doesn't use it.

## Regen flow

```bash
User clicks "Régénérer" on HomeView
  → invoke('regenerate_all_smart_playlists')
    → smart_playlists::generator::regenerate_daily_mixes()
    ├── top_artists_with_bpm()        ← profile DB + cross-DB lookup to app.db for picture_hash
    ├── for each bucket:
    │     ├── pick_tracks_for_artists()
    │     ├── shuffle_with_seed()
    │     ├── cover::build_daily_mix_cover()  ← only if any cached picture exists
    │     └── upsert_smart_playlist()         ← LIKE-match on smart_rules to refresh in place
    │
    → smart_playlists::on_repeat::regenerate_on_repeat()
    ├── top_played_tracks()           ← profile DB, 30-day window
    ├── cover::build_on_repeat_cover()← deterministic brand artwork
    └── upsert_smart_playlist()       ← LIKE-match on "kind":"on_repeat" needle
  → frontend usePlaylist().refresh() → sidebar + home carousel update
```

The cross-DB lookup in `top_artists_with_bpm` opens a short-lived secondary connection to `app.db` rather than `ATTACH`ing it — easier than routing through the same pool the Deezer commands use, and the lookup runs once per regen so the per-call cost is negligible.

## UI

- **Sidebar** ([`Sidebar.tsx`](../../src/components/layout/Sidebar.tsx)): when `cover_path` is set, the playlist row swaps its icon-tile for an 8 × 8 image. Smart playlists sort to the top of the playlist list because their `position` matches their `slot` (1-3) while user playlists default to 0.
- **HomeView** ([`HomeView.tsx`](../../src/components/views/HomeView.tsx)): a "Pour vous" section above "Récemment joués" renders 16:7 cards with the cover as background, a black gradient, the `DAILY MIX` label, the playlist name and the track count. Empty state when `smartPlaylists.length === 0`.
- **Regen button**: violet "Régénérer" with a spinning `RefreshCw` while in flight. Calls `regenerateDailyMixes()` then `refresh()` on the playlist context.

## Manual edits

A user adding or removing a track from a Daily Mix **will lose the change on the next regen**: `upsert_smart_playlist` deletes every `playlist_track` row for the playlist before re-inserting the freshly shuffled set. If durable curation is needed, the right move is to "Save as new playlist", which copies the tracks into a fresh `is_smart = 0` row.

## Custom smart playlists (recursive boolean rule tree)

The user-driven counterpart to Daily Mix lives in [`smart_playlists/custom.rs`](../../src-tauri/src/smart_playlists/custom.rs). `CustomRules` now wraps a **`RuleNode` tree** (`All` / `Any` / `Not` / `Leaf`) plus a sort + limit, so the editor can express arbitrary boolean expressions like `(artist contains "Daft Punk" OR artist contains "Justice") AND year ≥ 2000 AND NOT liked`. Predicates (leaves) carry the actual comparison; group nodes nest other nodes; `Not` wraps a single child.

### Tree shape

```rust
enum RuleNode {
    All { children: Vec<RuleNode> },  // AND
    Any { children: Vec<RuleNode> },  // OR
    Not { child: Box<RuleNode> },
    Leaf { predicate: Predicate },
}
```

JSON shape uses an internal `type` tag (`{"type":"all","children":[…]}`). The `Predicate` enum carries the leaf's `kind` + `value`:

| Predicate kind                                          | Notes                                                                            |
| ------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `title_contains` / `artist_contains` / `album_contains` | Case-insensitive `LIKE '%…%'`.                                                   |
| `genre_is`                                              | Single genre ID — multi-genre OR is expressed as `Any` of these.                 |
| `year_min` / `year_max`                                 | Inclusive bounds, `NULL` years filtered out.                                     |
| `bpm_min` / `bpm_max`                                   | Reads `track_analysis.bpm`.                                                      |
| `duration_min_ms` / `duration_max_ms`                   | Inclusive on `track.duration_ms`.                                                |
| `format`                                                | Single file extension — multi-format OR is expressed as `Any` of these.          |
| `hi_res` (unit)                                         | `sample_rate >= 88200 OR bit_depth >= 24`.                                       |
| `liked` (unit)                                          | `EXISTS (SELECT 1 FROM liked_track …)`.                                          |
| `rating_min`                                            | POPM 0-255 threshold. Editor's star picker writes `Math.round(stars / 5 * 255)`. |

### SQL builder

`build_node_sql` walks the tree recursively, emitting a single `WHERE` clause. Every join-needing predicate (`artist_contains`, `album_contains`, `genre_is`, `bpm_*`, `liked`) goes through an **`EXISTS` subquery** instead of a top-level JOIN — that way the tree can nest arbitrarily without DISTINCT, no Cartesian explosion regardless of how many leaves touch `track_artist` or `track_genre`. Empty `All` → `"1=1"`, empty `Any` → `"0=1"`; the editor relies on these for the "blank slate" + degenerate edge cases.

`custom::materialize` rewrites `playlist_track` in a transaction so a partial failure can't leave the playlist half-empty. `playlist.smart_rules` stores the full `Custom { rules }` JSON so a future regen pass can re-evaluate the same rule set without the editor reopening.

### v1 → v2 auto-migration

Pre-tree user data lives in flat predicates (`title_contains: "foo", year_min: 2020, genre_ids: [1,2,3], …`). A custom `Deserialize` impl detects the legacy shape (missing `tree` field) and folds it into an `All` root with each multi-value selector wrapped in `Any`. The migration is read-only — old JSON in `playlist.smart_rules` stays untouched until the next save — so a downgrade is safe.

### Commands

[`commands/smart_playlists.rs`](../../src-tauri/src/commands/smart_playlists.rs):

- `create_custom_smart_playlist(input)` — insert the row + materialise tracks.
- `update_custom_smart_playlist(playlist_id, input)` — update + re-materialise. Errors out when the target isn't a custom smart playlist (e.g. a Daily Mix slot or a manual playlist) so the editor never overwrites a wrong row.
- `regenerate_custom_smart_playlist(playlist_id)` — re-runs the stored rules without changing the definition. Useful after a library import so newly added tracks get picked up.
- `get_custom_smart_playlist_rules(playlist_id)` — rehydrates the editor.
- `preview_custom_smart_playlist(rules)` — dry-run for the editor's count badge; returns the matched track count + the first 200 ids.

### UI

[`RuleTreeEditor`](../../src/components/common/RuleTreeEditor.tsx) is a recursive React component that mirrors the data shape: each level renders the right widget for `node.type` and threads an `onChange(next)` callback up to the parent. No path arithmetic — every level only knows about its direct children. Group cards (AND = emerald, OR = violet, NOT = red) tint-code their operator; clicking the operator badge toggles AND ↔ OR in place. The "+ Condition / + Group / + NOT" footer adds children below the current group. Wired into [`SmartPlaylistEditorModal`](../../src/components/common/SmartPlaylistEditorModal.tsx) with sort + limit kept at the bottom and a live preview button at the footer.
