# Smart playlists

Auto-generated playlists materialised from the user's listening history. Today: a 3-slot **Daily Mix** family bucketed by energy, a single **On Repeat** playlist tracking the user's top played tracks over the last 30 days, a single **Release Radar** holding the recent releases of the user's artists that the library already has, and the artist, genre and decade mixes. Tomorrow: "Repeat Rewind", per-mood mixes — the engine in [`smart_playlists/`](../../src-tauri/crates/core/src/smart_playlists) is built around a discriminated `SmartPlaylistRules` enum so new families plug in without touching the regen flow.

## Storage

Smart playlists share the regular `playlist` table with user playlists. Three columns matter:

| Column                                                                                                                                  | Role                                                                                                                                                                              |
| --------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `is_smart` (`INTEGER`, default `0`)                                                                                                     | Filter flag for the UI.                                                                                                                                                           |
| `smart_rules` (`TEXT`)                                                                                                                  | JSON payload — `SmartPlaylistRules` enum. The regenerator looks up an existing slot via `LIKE '%"slot":N%'` so an upsert rewrites the same row instead of stacking duplicates.    |
| `cover_hash` (`TEXT`, added in [migration `20260509000000`](../../src-tauri/migrations/profile/20260509000000_playlist_cover_hash.sql)) | Blake3 hash of the composite cover, looked up in the shared `<root>/metadata_artwork/<hash>.jpg` cache. `NULL` → frontend falls back to the `icon_id` + `color_id` gradient tile. |

The Tauri layer additionally returns a derived `cover_path: Option<String>` resolved by [`metadata_artwork::existing_path`](../../src-tauri/crates/core/src/artwork/metadata.rs) so a stale `cover_hash` (cache wiped, file gone) doesn't render a broken image.

## Daily Mix algorithm

Implemented in [`generator.rs`](../../src-tauri/crates/core/src/smart_playlists/generator.rs). Inputs are read from the active profile's database; outputs are three playlists named `Daily Mix 1` / `2` / `3`.

### 1. Top artists window

```sql
WITH played AS (
    SELECT track_id, SUM(listened_ms) AS listened_ms
      FROM play_event
     WHERE played_at >= now() - 90 days
     GROUP BY track_id
)
SELECT a.id,
       SUM(p.listened_ms),
       AVG(ta.bpm),
       AVG(ta.energy)
  FROM played p
  JOIN track t           ON t.id  = p.track_id
  JOIN track_artist ta2  ON ta2.track_id = t.id AND ta2.position = 0
  JOIN artist a          ON a.id  = ta2.artist_id
  LEFT JOIN track_analysis ta ON ta.track_id = t.id
 WHERE t.is_available = 1
 GROUP BY a.id
 ORDER BY SUM(p.listened_ms) DESC
 LIMIT 60
```

The plays are summed **per track** before the join (`artist_listening`): joined straight to `play_event`, a track's measures repeated once per play, and the artist's mean tempo or energy became its most replayed song's — ten plays of a calm track and one of an intense one read as calm instead of the middle of the two.

Lookback is **90 days** so a one-off binge doesn't dominate forever. Below `MIN_ARTISTS = 6` distinct artists the regenerator skips entirely (better empty than degenerate three-shuffles-of-the-same-thing).

### 2. Energy bucketing

Each artist is routed to **one** bucket based on the average **energy** of their tracks (analysis version 2, 0 calm to 1 intense — [library.md](library.md#tempo-and-energy-analysis-version-2)):

| Slot | Label                | Energy         | Tempo, for older rows |
| ---- | -------------------- | -------------- | --------------------- |
| 1    | Daily Mix 1 (Calm)   | `< 0.4`        | `< 95`                |
| 2    | Daily Mix 2 (Groove) | `[0.4, 0.65)`  | `[95, 130)`           |
| 3    | Daily Mix 3 (Energy) | `≥ 0.65`       | `≥ 130`               |

The tempo was the only measure before, and a poor one here: a slow song can be intense and a fast one airy, and a tempo read an octave out moved an artist into the wrong mix outright. Measured on a real library (829 artists, `analysis_bench`), the tempo rule put 407 of 670 artists in Groove; the energy cuts are that library's per-artist tertiles (0.39 / 0.66, rounded), and give 266 / 278 / 285. An artist whose tracks were analysed before energy existed falls back to the tempo column until the sweep re-measures them, and one with **no** analysis at all goes to slot 2 — a missing analysis doesn't black-hole an artist.

### 3. Track picking

For each bucket: take the top 12 artists, fetch up to 50 tracks per artist (a mix's worth), its most played first (`POOL_PER_ARTIST`; one cut of 200 over the whole bucket could be all one prolific artist's), deterministic-shuffle (xorshift seeded with `SHUFFLE_SEED ^ slot`), then take 50 with **at most an even share per artist** (`cap_per_artist`: `ceil(50 / artists)`, never under 3). Without the cap the bucket's most played artist could fill the mix alone; the shuffle has already run, so the tracks it gives up are random ones.

**Album mode** (#618, `profile_setting['playback.generator_album_mode']`,
default off) replaces that with whole records: albums by the bucket's
artists, shuffled as units and played in disc / track order. One
setting covers the Daily Mix and Mood Radio both, because it is one
preference — someone who listens to albums wants albums from anything
that builds them a session.

A record belongs to a bucket when **most of what we have measured of
it** sits inside that bucket's window (60 %, over at least three
analysed tracks) — by each track's energy, or its tempo for a track
analysed before energy existed, the artists' rule. A fraction rather than a median: a median says
nothing about spread, so a record that is half ambient and half thrash
lands in the middle and would be offered for a mood neither of its
halves belongs to. Asking that most of it actually fits rejects that
record from every bucket, which is the right answer.

The 50-track cap is then spent in whole albums
([`album_playback::fit_albums_to_budget`](../../src-tauri/crates/core/src/album_playback.rs)) —
truncating the track list would end the mix halfway through a record,
which is the single thing album mode exists to prevent. An album longer
than the whole budget is still taken, because an empty mix is worse
than a long one, and that exception is spent once.

What the bucket actually produced is written into the playlist's own
`smart_rules` as `album_mode` (#647). The session outlives its
generation — a mix is a stored playlist, and playing it a week later
goes through the ordinary playlist path — so this is the only way the
gain decision can still tell a mix of records from a mix of tracks. It
records the **fallback** too: a bucket where no record qualifies is
filled track by track on purpose, and says so. See
[playback.md](playback.md#replaygain) for what reads it.

Determinism matters: the same input set always produces the same listening order, so the user doesn't see a "different mix" mid-session if the playlist re-renders. A second regen against the same listening data rewrites the rows in place.

### 4. Cover composition

[`cover.rs`](../../src-tauri/crates/core/src/smart_playlists/cover.rs):

- **Image source priority** — try the top 3 artists' Deezer pictures first (shared `metadata_artwork/<hash>.jpg` cache, looks best because portraits crop cleanly). If none of the cluster's artists are Deezer-enriched (common with niche / soundtrack libraries), fall back to **album artwork of the first 3 shuffled tracks** from the per-profile cache (`<root>/profiles/<id>/artwork/<hash>.<format>`). The fallback is what guarantees a real cover even for libraries dominated by obscure artists.
- **Layout auto-pick** — the `build_composite_cover` entry point dispatches by input count: 1 → fill the canvas, 2 → vertical halves, 3 → 3 strips (the Daily Mix look), 4+ → **2×2 grid** (Spotify-style auto-playlist cover, used by the user-playlist auto-cover pipeline; smart playlists never reach this branch since they cap at 3 artist pictures).
- **Identical-input dedup** — the compositor dedupes incoming paths before counting, so a Daily Mix whose top 3 artists share a Deezer picture (or whose fallback album arts all point at the same release) collapses to a single full-canvas tile instead of a contact-sheet of identical thumbnails. Mirrors the hash-level dedup that `playlist_cover::top_track_artwork_paths` applies for user playlists — both caches are hash-keyed (`metadata_artwork/<blake3>.jpg` + per-profile `artwork/<hash>.<ext>`), so path equality is content equality.
- 640×640 RGB canvas → centre-crop each source via `cover_fit` (matches CSS `object-fit: cover`) → SIMD resize via `fast_image_resize 6` → paint.
- Apply a `t²` ease-out gradient over the bottom 40 % so the React-rendered "Daily Mix N" label stays legible without baking text into the JPEG.
- Encode JPEG q=85 → blake3 → write to `metadata_artwork/<hash>.jpg`.

Why the label isn't rasterised in Rust: avoids a font dep (`ab_glyph` / `fontdue` + a bundled TTF) and lets the frontend re-style / re-translate without regenerating images.

## On Repeat algorithm

Implemented in [`on_repeat.rs`](../../src-tauri/crates/core/src/smart_playlists/on_repeat.rs). Single playlist, no slot bucketing — the top tracks the user has rotated the most over the last 30 days, ordered by play count descending. The materialised playlist holds **up to `TRACKS_LIMIT = 30` tracks**; the SQL fetches up to 60 candidates first so the Rust caller has headroom to filter for future variants (e.g. dropping tracks already on another smart playlist) without re-issuing the query, then truncates client-side via `tracks.iter().take(TRACKS_LIMIT)` before the upsert.

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

On Repeat doesn't use the album-art composite — its identity is a fixed visual rendered deterministically by [`cover::build_on_repeat_cover`](../../src-tauri/crates/core/src/smart_playlists/cover.rs): a 640×640 JPEG composited from the embedded SVG at [`on_repeat.svg`](../../src-tauri/crates/core/src/smart_playlists/on_repeat.svg) — deep indigo → near-black diagonal gradient, a faint vertical-bar equaliser motif at 4 % opacity, and a centred bezier infinity loop stroked with a `#ff3377 → #9933ff → #33ccff` gradient under a gaussian glow filter with a thin white inner-rim. Rasterised through `resvg + usvg + tiny-skia` (default features disabled — no font / raster-image bloat since the SVG is shape + gradient + filter primitives only), so the curves anti-alias at the canvas resolution instead of looking like the old hand-rolled pixel grid. Same bytes every regen → same blake3 hash → the cache dedupes against the existing file instead of piling up orphans. No `<text>` baked in (the playlist name and "On Repeat" eyebrow are rendered by React on top of the tile), so the canvas stays locale-agnostic.

Release Radar, the genre and decade mixes take the 2×2 composite of their first tracks' album art instead, through [`generator::first_track_artwork_paths`](../../src-tauri/crates/core/src/smart_playlists/generator.rs), which is `pub(super)` for them.

## Genre and decade mixes

[`smart_playlists/themed.rs`](../../src-tauri/crates/core/src/smart_playlists/themed.rs). The other families cut the library on energy (Daily Mix) or on recency (On Repeat); these cut it along axes of their own: one mix per genre, and one per decade, the user listens to most. Both families work the same way and differ only in how a mix's tracks are found (`track_genre` for a genre, `track.year` between the decade's first and last year for a decade).

- **Seeds.** The three genres (decades) with the most `listened_ms` over the last 90 days, the largest in the library breaking ties — so a library with no history yet still gets mixes, of what it holds most of. A seed needs **25 available tracks by 4 artists** (`MIN_TRACKS`, `MIN_ARTISTS`): below that a "mix" is one album on repeat. The 25 are counted again as **songs** once copies are merged (below), before the per-artist cap; a seed that falls short gives its place to the next, since nine are read for three mixes. A year outside 1–9999 belongs to no decade, as in the Years tab.
- **Tracks.** Every available track of the seed, scored `ln(1 + plays) + jitter`, the jitter in `[0, 1)` drawn from a seed of the family, the genre or decade, and **the day**. One play already outweighs most of the draw while a track never played can still beat one played once, so the mix leans on what the user plays and still brings back what they have not; and it changes from one day to the next without moving between two visits the same day. Play counts are all-time: the window ranks the seeds, but a track loved two years ago still belongs at the front of its genre's mix. **One copy per song** (`dedupe`): two files of the same track — same artist, same title under `canonical_name`, say an MP3 and its Hi-Res re-buy — count once, the best copy (bit depth, then sample rate) standing for it with the plays of both. **At most 3 tracks per artist** (`PER_ARTIST_CAP`), then 50 tracks, then shuffled from the same seed so the favourites are spread through the mix rather than played first.
- **Rows.** `GenreMix { genre_id }` / `DecadeMix { decade }` in `smart_rules`, upserted through the same `upsert_smart_playlist` as Daily Mix, with the rule JSON minus its opening brace as the `LIKE` needle — the closing brace stays, so genre 1 cannot match genre 12, and neither variant has a `slot` field for Daily Mix's `"slot":N` deletes to catch. Positions 10+ (artists, below), 20+ (genres) and 30+ (decades), after On Repeat (0) and Daily Mix (1–3). After a family is regenerated, its rows that were not rewritten are **removed**: a genre that left the top three, or no longer has 25 tracks, takes its mix with it. Cover: the 2×2 composite of the first tracks' album art, as Daily Mix's fallback.
- **Names in the user's language.** Daily Mix and On Repeat store a fixed name; a genre or decade mix is named after data the backend has and words it does not. `regenerate_all_smart_playlists` takes `MixLabels` — the name and description templates, translated by the frontend with `{name}` / `{decade}` left in for the backend to fill — so the rows read "Mix Rock" / "Années 1990" in French and "Rock Mix" / "The 1990s" in English, as of the last regeneration.
- **UI.** Each family has its eyebrow and fallback colour on the Home cards and on the playlist header (`smartFamilyStyle` in [`playlist.ts`](../../src/lib/tauri/playlist.ts)). The user's own rule-based playlists, also `is_smart`, are left out of "Made for you" — they are theirs, in the sidebar — and their header no longer reads "Daily Mix".

## Artist mixes

The same module, the same scoring, pick and sweep as the genre and decade mixes; what differs is who a mix is about and who joins it.

- **Seeds.** The three artists with the most `listened_ms` over the last 90 days that hold **4 songs** (`MIN_SEED_SONGS`), counted once copies are merged; one that falls short leaves its place to the next. There is no fallback without history: the family is "the artists you play".
- **Neighbours** (`neighbours` in `themed.rs`, up to 8): first the similar-artists cache (`app.lastfm_similar`), in its order, keeping the artists that have an available track; then the artists the user plays in the same sessions — a play within 30 minutes of one of the seed's over the last year, most often first (`co_listened_artists` in [`similar.rs`](../../src-tauri/crates/core/src/similar.rs)) — and only those played beside it **3 times and a quarter as often as the closest** (`MIN_CO_LISTENS`, `CO_LISTEN_SHARE`). Without that floor, eight places filled the list deep into chance meetings in long shuffled sessions: measured on a real library, 45 plays of one lo-fi artist's closest neighbour came within a session of it, and 5 of its eighth, a K-pop act. The count is of the neighbour's distinct plays, not of pairs, so one play near several of the artist's is counted once. **Nothing goes to the network**: the cache is read, never filled, so a regeneration offline gives the same mixes, and an artist nobody looked up still gets neighbours from the user's own listening. The cache read lives in core now, shared with the radio (`commands/radio.rs`), which still fills the cache before reading it.
- **Tracks.** The artist's own up to **15** (`SEED_ARTIST_CAP`), each neighbour's up to **4** (`NEIGHBOUR_CAP`), scored and shuffled like the other mixes. A mix needs **2 neighbours bringing tracks** (`MIN_NEIGHBOURS`) and **15 songs** (`MIN_ARTIST_MIX_TRACKS`, lower than a genre's 25: in a library built of singles both the artist and its neighbours are there by a few songs each — measured on one, the three most played artists had 4 to 9 songs and their eight neighbours one or two, which made mixes of 16 to 18): below that it is the artist's page shuffled, not a mix, and the artist gets none.
- **Rows.** `ArtistMix { artist_id }`, positions 10+, named from the frontend's `artist_name` / `artist_description` templates ("Mix {name}").

## Release Radar

The recent releases of the artists in the library, found on [MusicBrainz](https://musicbrainz.org) and matched against the files the user already has. It replaces the `ui`-world plugin of the same name ([plugins.md](plugins.md#release-radar-ui-world)): the logic that plugin proved — an incremental scan, one request a second, a clean stop on `503` / `429`, a cursor to resume from, an identifying User-Agent — is kept, and the release now meets the library. **Nothing is downloaded but a release's information and its cover**: the radar points at music, it never fetches any.

A release is in one of **two states**, and only two:

| State          | Means                                                     | Shown as                                              |
| -------------- | --------------------------------------------------------- | ----------------------------------------------------- |
| **discovered** | MusicBrainz knows of it, no local track was matched to it | its cover, a link to its MusicBrainz page             |
| **playable**   | at least one available local track was matched to it      | its cover, a play button, and a place in the playlist |

A release is never offered as playable without a matched track: "playable" is the presence of a match, computed on every read, not a stored flag.

### Pieces

- **Core** — [`waveflow_core::release_radar`](../../src-tauri/crates/core/src/release_radar.rs): the Lucene query, the parsing, the title rules and the matcher, all pure; [`musicbrainz`](../../src-tauri/crates/core/src/release_radar/musicbrainz.rs), the release-group search client; [`store`](../../src-tauri/crates/core/src/release_radar/store.rs), the two tables. The family is [`smart_playlists::release_radar`](../../src-tauri/crates/core/src/smart_playlists/release_radar.rs).
- **App** — [`commands/release_radar.rs`](../../src-tauri/crates/app/src/commands/release_radar.rs): the scan (`release_radar_scan`), the view's read (`release_radar_list`) and the local playlist rewrite (`release_radar_refresh_playlist`). The scan is the app's because it needs the offline switch, the task row, the pacing across awaits and the profile it runs for.
- **Tables** — `release_radar_artist` (what the radar learnt about an artist: its MusicBrainz id, when it was last asked about) and `release_radar_release` (one row per release group) — see [storage.md](../architecture/storage.md#release-radar). The radar never writes the library's own `artist` table.

### Who is followed

The library's **100** heaviest artists (`FOLLOWED_ARTISTS`, `store::followed_artists`). An artist weighs its available tracks, plus one for every two plays over the last 90 days, plus three for every liked track — what the user keeps, listens to and loves, the first of which is all a library that was never played has. Every credit counts, not only the first, so a featured artist the user collects is followed too. "Various Artists" is a label and is never followed.

### The scan

`release_radar_scan(rescan, playlistDescription)`, from the view's button — never on its own: opening the view sends nothing.

1. **Offline mode** answers `offline` before anything is sent, and is checked again **before every request**: the switch can be turned on while the scan runs.
2. The releases older than the window (**180 days**, `RECENT_DAYS`) are pruned; `rescan` clears every artist's `checked_at` and learnt id.
3. **The cursor is `checked_at`.** An artist answered less than 20 hours ago (`REFRESH_AFTER_MS`) is not asked again, so a scan stopped by the user, a rate limit, offline mode, the network or a profile switch resumes with the artists it did not reach. A failed lookup leaves the artist due.
4. **One request a second.** Every request waits on the process-wide `musicbrainz::shared_pacer()` (one a second for the whole app, shared with the [enrichment engine](enrichment.md)), and a rate limit pushes that pacer back for every caller ([invariant](../architecture/invariants.md#musicbrainz-is-paced-process-wide)). A `503` / `429` is waited out **once**, at the `Retry-After` MusicBrainz gives (5 s without one, 3 s at least — its `503` comes with `Retry-After: 0` — and 30 s at most), and stops the scan the second time — never a retry loop, which is what gets a client blocked. Three transport failures in a row stop it too (`failed`): the network is down.
5. **By id when known.** The query is `arid:<mbid>` once the radar knows the artist's MusicBrainz id, `artist:"<name>"` until then. A query by name is a phrase search, answered with homonyms and releases that merely mention the name, so a release is kept only when one of its credits is the artist, through the shared [`name_match::select_by_name`](../../src-tauri/crates/core/src/metadata/name_match.rs) (which also accepts "Artist & The Band"). The id is **learnt only when the library confirms it** ([`confirmed_mbid`](../../src-tauri/crates/core/src/release_radar.rs)): every release of the answer that matched a local track of the artist was credited, under the artist's exact name, to one and the same id. The answer alone is no proof — it is filtered to the recent window, so an artist with nothing new and a homonym with something would leave the homonym's id alone on the page — while a homonym does not put out a record the user already has under that name. A credit kept only as "Artist & The Band" teaches nothing either: that id is the band's. Until an id is confirmed, the radar keeps searching by name; **Check everything again** forgets the learnt ids along with `checked_at`, the one way back from an id learnt wrong.
6. Each artist's answer is **one short transaction** (`store::record_artist`): checked, its confirmed id kept (`COALESCE`, a later answer that confirms nothing does not erase it), its releases upserted. Every write of the scan, the playlist's included, is parked behind a library scan (`scan_in_flight`) and retried on `SQLITE_BUSY`.
7. **Covers** come from the [Cover Art Archive](https://coverartarchive.org) (`front-250`), cached into the shared `metadata_artwork/` by `download_and_cache`, each waiting on the same pacer, so the view draws local files — offline too — and never a remote image. A release the archive had no cover for is asked again after a week.
8. When the scan ends — however it ends, unless the profile changed — the playlist is rewritten and `playlist:changed` emitted. `release-radar:changed` goes out after every artist, so an open view fills as releases come in.

The scan takes a `TaskHandle` (`TaskKind::ReleaseRadar`, slug `release_radar`, `Cancellation::Flag`) before anything waits — a scan parked behind a library scan is shown and can be stopped — so the status bar shows it with a stop button, and it stops between two artists (or during a `Retry-After` wait, polled every 500 ms). It takes the profile pool again for every artist rather than holding a lease for the minute it runs, and stops (`profile_changed`) the moment the active profile is not the one it started for.

**Where the library's own MusicBrainz ids plug in.** `store::followed_artists` reads `COALESCE(musicbrainz_artist.artist_mbid, release_radar_artist.mbid)`: an artist id the user accepted from the [enrichment engine](enrichment.md) (status `applied`) goes first, so that artist is asked about by id from the first scan on, and the id the radar learnt is the fallback. A proposal still waiting on review is not used. "Recheck everything" forgets only the radar's own ids; an accepted one stays until the user undoes it on the enrichment side.

### Matching

[`match_releases`](../../src-tauri/crates/core/src/release_radar.rs) pairs each release with the library's available tracks, **within the artist the release was found for** — never across artists, since a title alone ("Intro", "Home") is shared by half a library. A track belongs to an artist by any of its credits or by its album's artist.

- **An album, EP or other release** matches the local albums whose title has the same key: [`title_key`](../../src-tauri/crates/core/src/release_radar.rs) drops a store's " - Single" / " - EP" suffix and a trailing bracketed **edition** ("(Deluxe Edition)", "[2024 Remaster]"), then folds through `name_match::normalize_name`. A remix or a live recording keeps its suffix: it is another record. Its tracks play as the record (`matched_by: album`, album-ordered for ReplayGain).
- **A single** that no album matches falls back to the artist's tracks of that title — the single often came out before the album the user has (`matched_by: track`).
- **An album never falls back to a track**: the title track of a record is not the record.

### The playlist

`ReleaseRadar` in `smart_rules` (needle `"kind":"release_radar"`), named **"Release Radar"** in every language, like Daily Mix and On Repeat; its description comes from the frontend (`MixLabels.release_radar_description`). Position **4**, after Daily Mix. Releases newest first, each bringing at most **5** of its matched tracks (`PER_RELEASE_CAP`, so a whole new album does not fill it alone), **50** in all, a track two releases share (a single, then its album) once. Cover: the 2×2 composite of the first tracks' album art. No playable release, no row — an existing one is deleted.

It is rewritten from local data only, so a regeneration offline gives the same playlist: at the end of a scan, by **Regenerate** on Home (`regenerate_all_smart_playlists`), and when the view opens (`release_radar_refresh_playlist`), so an album imported since the last scan joins it without asking MusicBrainz anything.

### View

[`ReleaseRadarView`](../../src/components/views/ReleaseRadarView.tsx), the `release-radar` view, from a native sidebar entry (always present — the radar sends nothing until asked). A header with the artists followed and the last check; **Find new releases** / **Check everything again**, disabled offline; **Play** (every playable release) and **Open the playlist**; filters **All / In your library / Discovered**; then the releases as a grid virtualized against the page scroller. A card says its date at the precision MusicBrainz gave it, "Upcoming" for a date after today, its type, and either its local track count with a play button or a link to MusicBrainz. Whether a scan runs is read from the task registry (`useTasks`), not kept in the view, so it survives a trip to another page; the stop button cancels the task. The card's hook for the skins is `data-release-card` with `data-release-state`.

**The retired plugin.** While the `ui` plugin this view replaced is installed — enabled or not — the view shows a notice, `data-release-radar-plugin-notice`, saying the radar is built in now and the plugin can be uninstalled, with a button to the Extensions page. It is shown **once per profile** ([`useReleaseRadarPluginNotice`](../../src/hooks/useReleaseRadarPluginNotice.ts), `profile_setting['release_radar.plugin_notice_seen']`, default unseen): the flag is written as soon as the notice is drawn, and the notice stays up for that visit until it is closed or the plugin uninstalled. Not a preference, so not in the settings registry and not counted by Settings' reset, like `artist.bio_collapsed`. The plugin's own sidebar entry is never shown ([plugins.md](plugins.md#release-radar-ui-world)).

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
    │
    → smart_playlists::release_radar::regenerate_release_radar()
    ├── store::recent_releases() + store::local_tracks()  ← the radar's tables; no network
    ├── match_releases() → pick_tracks()  ← newest first, 5 per release, 50
    │
    → smart_playlists::themed::regenerate_artist_mixes()
    ├── artist_seeds() → neighbours()   ← similar-artists cache, then shared sessions; no network
    │
    → smart_playlists::themed::regenerate_genre_mixes() / regenerate_decade_mixes()
    ├── genre_seeds() / decade_seeds()  ← 90-day listened_ms, then size; 25 tracks, 4 artists
    ├── for each seed: candidates → pick() (score, 3 per artist, 50, shuffle)
    ├── cover::build_composite_cover() + upsert_smart_playlist()
    └── sweep()                         ← the family's rows not rewritten this time
  → frontend usePlaylist().refresh() → sidebar + home carousel update
```

The cross-DB lookup in `top_artists_with_bpm` reads `metadata_artist` (renamed from `deezer_artist` in `20260427000000`; until the generator followed, the query failed, the error was swallowed, and no Daily Mix ever got its artist pictures) and opens a short-lived secondary connection to `app.db` rather than `ATTACH`ing it — easier than routing through the same pool the Deezer commands use, and the lookup runs once per regen so the per-call cost is negligible.

## UI

- **Sidebar** ([`Sidebar.tsx`](../../src/components/layout/Sidebar.tsx)): when `cover_path` is set, the playlist row swaps its icon-tile for an 8 × 8 image. Generated playlists carry their family's `position` (On Repeat 0, Daily Mix 1–3, Release Radar 4, artist mixes 10+, genre mixes 20+, decade mixes 30+), and the list sorts on `position`, then on the last update.
- **HomeView** ([`HomeView.tsx`](../../src/components/views/HomeView.tsx)): a "Pour vous" section above "Récemment joués" renders 16:7 cards with the cover as background, a black gradient, the `DAILY MIX` label, the playlist name and the track count. Empty state when `smartPlaylists.length === 0`.
- **Regen button**: violet "Régénérer" with a spinning `RefreshCw` while in flight. Calls `regenerateAllSmartPlaylists(labels)` — every family, with the genre and decade mixes' name templates — then `refresh()` on the playlist context.

## Manual edits

A user adding or removing a track from a Daily Mix **will lose the change on the next regen**: `upsert_smart_playlist` deletes every `playlist_track` row for the playlist before re-inserting the freshly shuffled set. **Adding is refused**: the "Add to playlist" menus list only regular playlists, and `add_track_to_playlist`, `add_tracks_to_playlist` and `add_source_to_playlist` reject a smart one, since the track would be accepted and then vanish. **Removing is still allowed** ("Remove from playlist" on the track and in the selection bar) and lasts until the next regeneration: it hides a track for now, which is still useful. **To keep a mix, copy it:** "Keep as playlist" in a smart playlist's header opens the create-playlist dialog with the mix's name and copies its tracks, in the order shown, into a new regular playlist, which no regeneration touches.

## Custom smart playlists (recursive boolean rule tree)

The user-driven counterpart to Daily Mix lives in [`smart_playlists/custom.rs`](../../src-tauri/crates/core/src/smart_playlists/custom.rs). `CustomRules` now wraps a **`RuleNode` tree** (`All` / `Any` / `Not` / `Leaf`) plus a sort + limit, so the editor can express arbitrary boolean expressions like `(artist contains "Daft Punk" OR artist contains "Justice") AND year ≥ 2000 AND NOT liked`. Predicates (leaves) carry the actual comparison; group nodes nest other nodes; `Not` wraps a single child.

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

| Predicate kind                                          | Notes                                                                                                                                                                                                                                                                                               |
| ------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `title_contains` / `artist_contains` / `album_contains` | Case-insensitive `LIKE '%…%'`.                                                                                                                                                                                                                                                                      |
| `genre_is`                                              | Single genre ID — multi-genre OR is expressed as `Any` of these.                                                                                                                                                                                                                                    |
| `year_min` / `year_max`                                 | Inclusive bounds, `NULL` years filtered out.                                                                                                                                                                                                                                                        |
| `bpm_min` / `bpm_max`                                   | Reads `track_analysis.bpm`.                                                                                                                                                                                                                                                                         |
| `duration_min_ms` / `duration_max_ms`                   | Inclusive on `track.duration_ms`.                                                                                                                                                                                                                                                                   |
| `format`                                                | Single file extension — multi-format OR is expressed as `Any` of these.                                                                                                                                                                                                                             |
| `hi_res` (unit)                                         | `sample_rate >= 88200 OR bit_depth >= 24`.                                                                                                                                                                                                                                                          |
| `fake_lossless` (unit)                                  | A lossless format whose highs stop at a lossy encoder's wall — `probable_fake_lossless_sql!`, see [library.md](library.md#fake-lossless-files).                                                                                                                                                     |
| `liked` (unit)                                          | `EXISTS (SELECT 1 FROM liked_track …)`.                                                                                                                                                                                                                                                             |
| `rating_min`                                            | POPM 0-255 threshold. Editor's star picker writes `Math.round(stars / 5 * 255)`.                                                                                                                                                                                                                    |
| `play_count_min` / `play_count_max`                     | `COUNT(*)` over `play_event` — one row is one play, the same number Statistics shows. `play_count_max: 0` is how a rule says _never played_.                                                                                                                                                        |
| `played_in_last_days` / `added_in_last_days`            | Relative windows, resolved to an absolute epoch-ms cut-off at build time. Not dates: a stored rule is re-evaluated for years, and a pinned date drifts into meaning something its author never wrote. "Not played since" is the first one under a `Not`, which also takes in what was never played. |
| `sample_rate_min` / `bit_depth_min`                     | The two halves of `hi_res` said separately — it is the OR of two fixed thresholds and cannot express "88.2 kHz but 16-bit".                                                                                                                                                                         |
| `disc_number_is`                                        | One disc of a multi-disc set.                                                                                                                                                                                                                                                                       |
| `path_contains`                                         | Both sides folded to `/`, so one rule reads the same whichever separator the scanning OS wrote. The "everything under this folder" rule.                                                                                                                                                            |
| `tag_present` / `tag_contains`                          | A custom tag out of `track_tag` (#588) — a rip source, a catalogue number, a mood. The **key is a bind**, not interpolated; `track_tag.key` is `COLLATE NOCASE`, so the comparison stays case-insensitive _and_ index-backed.                                                                       |

### SQL builder

`build_node_sql` walks the tree recursively, emitting a single `WHERE` clause. Every join-needing predicate (`artist_contains`, `album_contains`, `genre_is`, `bpm_*`, `liked`) goes through an **`EXISTS` subquery** instead of a top-level JOIN — that way the tree can nest arbitrarily without DISTINCT, no Cartesian explosion regardless of how many leaves touch `track_artist` or `track_genre`. Empty `All` → `"1=1"`, empty `Any` → `"0=1"`; the editor relies on these for the "blank slate" + degenerate edge cases.

Bounds on a nullable column (`sample_rate`, `bit_depth`, `disc_number`, `year`, `rating`) carry an explicit `IS NOT NULL` guard rather than being left to SQLite's three-valued logic: without it the fragment is NULL for a track missing the value, and `NOT (NULL)` is NULL too — the track would fall out of the rule **and** out of its negation, which reads as the library losing tracks.

The clock is a parameter, not a call inside the builder: `run_query_at` / `count_matches_at` take `now_ms`, so the relative windows are testable against a pinned instant instead of racing midnight. `run_query` / `count_matches` read the clock and delegate.

`custom::materialize` rewrites `playlist_track` in a transaction so a partial failure can't leave the playlist half-empty. `playlist.smart_rules` stores the full `Custom { rules }` JSON so a future regen pass can re-evaluate the same rule set without the editor reopening.

### v1 → v2 auto-migration

Pre-tree user data lives in flat predicates (`title_contains: "foo", year_min: 2020, genre_ids: [1,2,3], …`). A custom `Deserialize` impl detects the legacy shape (missing `tree` field) and folds it into an `All` root with each multi-value selector wrapped in `Any`. The migration is read-only — old JSON in `playlist.smart_rules` stays untouched until the next save — so a downgrade is safe.

### Commands

[`commands/smart_playlists.rs`](../../src-tauri/crates/app/src/commands/smart_playlists.rs):

- `create_custom_smart_playlist(input)` — insert the row + materialise tracks.
- `update_custom_smart_playlist(playlist_id, input)` — update + re-materialise. Errors out when the target isn't a custom smart playlist (e.g. a Daily Mix slot or a manual playlist) so the editor never overwrites a wrong row.
- `regenerate_custom_smart_playlist(playlist_id)` — re-runs the stored rules without changing the definition. Useful after a library import so newly added tracks get picked up.
- `get_custom_smart_playlist_rules(playlist_id)` — rehydrates the editor.
- `count_custom_smart_playlist(rules)` — what the rules match right now, as `{ total, kept }`: every available track the tree matches, and what survives the limit. `COUNT(*)` only — no sort, no truncation, no ids crossing the IPC boundary, because the editor calls it after every edit. It replaces the old `preview_custom_smart_playlist`, whose first-200-ids half no caller ever used and whose count the live counter now serves.

### UI

[`RuleTreeEditor`](../../src/components/common/RuleTreeEditor.tsx) is a recursive React component that mirrors the data shape: each level renders the right widget for `node.type` and threads an `onChange(next)` callback up to the parent. No path arithmetic — every level only knows about its direct children. Group cards (AND = emerald, OR = violet, NOT = red) tint-code their operator; clicking the operator badge toggles AND ↔ OR in place. The "+ Condition / + Group / + NOT" footer adds children below the current group. Predicates are grouped into `<optgroup>` sections (text / library / audio / plays / tags) — two dozen entries in one flat list is a list nobody reads to the end. The two tag predicates offer the keys the library actually holds, each with the number of tracks carrying it (`list_track_tag_keys`): the count separates the tag on every track from the one on three, and a picker proves the key exists, where a typed one that matches nothing is a rule returning no tracks with nothing on screen to say why.

Wired into [`SmartPlaylistEditorModal`](../../src/components/common/SmartPlaylistEditorModal.tsx) with sort + limit at the bottom and, at the footer, a **live match count**: debounced 350 ms, re-run on every rule edit, showing `kept / total` whenever a limit is cutting the result. The count claims a token when it starts and checks it when it lands, so a slow answer to a rule the user has already changed is dropped rather than overwriting the answer to the rule on screen. A failed count leaves the last number and says nothing — it is an aid, not a gate.

[`RULE_TEMPLATES`](../../src/lib/smartPlaylistTemplates.ts) fills the "start from a template" picker on the rules section. Applying one to a playlist that already has rules **replaces** them, which is the more useful half: starting again from something known to work is what repairs a tangled rule set. Nothing is written until Save, so closing the modal undoes it — no confirmation step needed.

### Rules in plain language

[`describeRules`](../../src/lib/smartRuleSummary.ts) renders a rule tree as a sentence, shown by [`SmartRuleSummary`](../../src/components/common/SmartRuleSummary.tsx) under a custom smart playlist's title. Built from translated fragments — one key per predicate with its value interpolated, groups joined by a separator, nested groups parenthesised — so a translator only ever sees short phrases and the recursion stays out of the locale files.

It reads the rules through `get_custom_smart_playlist_rules` rather than parsing `playlist.smart_rules`, which is already in the row: playlists created before the tree carry the v1 flat shape and **only the backend deserializer migrates it**, so reading the column here would render an empty sentence for exactly the oldest playlists.

## Mood Radio

Five presets, each a **tempo gate plus a shape**
([`commands/mood_radio.rs`](../../src-tauri/crates/app/src/commands/mood_radio.rs),
scoring in [`waveflow_core::mood`](../../src-tauri/crates/core/src/mood.rs)).
The gate decides what may be considered; everything inside it is ranked
by how well it fits, and the forty tracks that play are the best of the
pool rather than the first forty drawn out of it (#616).

| Mood    | Tempo gate | Centre | Energy                             | Loudness                                    |
| ------- | ---------- | ------ | ---------------------------------- | ------------------------------------------- |
| Focus   | 72–108     | 88     | prefers ≤ 0.55                     | prefers ≤ −14 LUFS                          |
| Chill   | 65–95      | 78     | prefers ≤ 0.5                      | prefers ≤ −10 LUFS                          |
| Workout | 128–180    | 150    | prefers ≥ 0.6, **excludes** < 0.4  | prefers ≥ −12 LUFS                          |
| Party   | 110–132    | 122    | prefers ≥ 0.6, **excludes** < 0.4  | prefers ≥ −12 LUFS                          |
| Sleep   | ≤ 68       | 52     | prefers ≤ 0.25, **excludes** > 0.4 | prefers ≤ −18 LUFS, **excludes** > −12 LUFS |

The score weighs tempo 0.5, energy 0.3, loudness 0.1 and genre 0.1. A tempo read with confidence `c` counts as `c` of its fit plus `1 − c` of the unknown score, so a reading the estimator doubts is weak evidence either way. Rows from analysis version 1 have neither a confidence nor an energy and are judged as before until the sweep re-measures them.

The pool of 400 is drawn **measured readings first**, shuffled within
each group, and the ranking then decides which forty of it play. The
shuffle is what keeps two runs of one mood from being the same queue;
the priority is what stops a guess from taking a slot from a track that
really is this tempo — see the octave note below.

### Why mostly tempo gates

A gate answers "is this the wrong kind of track", a score answers "how
right is it". Tempo is the signal where falling outside the range most
clearly means the wrong mood — a 160 BPM track is not Sleep at any
loudness. Loudness and genre rank instead, which is what lets a thin
library still return forty tracks, closest fits first, rather than an
error.

**Sleep also gates on energy and loudness.** The energy ceiling (0.4,
about the calmest third of a real library) is what keeps a dance record
out at any level; Workout and Party take the mirror floor, so a fast
but gentle track does not end up on a treadmill. The loudness ceiling
(`lufs_ceiling`, −12 LUFS: where its loudness ranking reaches zero)
dates from analysis version 1, whose tempo stuck to 60 BPM when it
could not decide, so fast tracks read as 60-something often enough that
Sleep's window held a fifth of a library; it stays for rows not yet
re-measured, and does little on a library normalised to −14. Unmeasured
energy and loudness both pass. A record in album mode is out whole if
one of its tracks is measured past either gate. The home-tile count
answers the same gates, so it says what the radio can draw from.

That also fixes the case the issue opened on: **an unmeasured loudness
used to satisfy a ceiling exactly as well as a measured quiet track**
(`loudness_lufs IS NULL` passed the filter). It now scores 0.5 — below
a track measured inside the mood, above one measured outside it, which
is the only honest ordering and the same rule the tag matcher uses for
missing data.

### What the command answers with

`start_mood_radio` returns `{ trackIds, albumOrdered }`, not a bare
list (#647). Album mode is shared with the Daily Mix, and a mood with
no qualifying record falls back to individual tracks on purpose — so
the caller cannot tell the two apart from the ids, and it needs to:
`albumOrdered` is what it hands `playerPlayTracks`, and what tells
automatic ReplayGain that a record is playing through.

### Octave correction

Tempo estimators land an octave out often enough that a 170 BPM track
is recorded as 85, and a library where that happened is a library where
Focus quietly fills with drum'n'bass. The candidate query accepts any
octave reading (`bpm`, `bpm × 2`, `bpm ÷ 2`) **toward a bounded side
only** — doubled when the window has a ceiling, halved when it has a
floor. Sleep has no floor, and halving into it admitted every track up
to 136 BPM, nine tenths of a library (a user's Sleep counted 312
tracks). The scorer discounts
the corrected one by a third: a guess about a measurement is worth less
than a measurement, so rescued tracks sit below the honest ones rather
than beside them. A tempo already inside the window is **never**
reinterpreted, however well its double would score — and since analysis
version 2, neither is a tempo read with confidence 0.4 or more
(`TRUSTED_TEMPO`): 91% of those are right, and reinterpreting them would
move far more correct readings than it rescued. The SQL gate and the
scorer share the threshold, and a test holds them equal.

### Narrowed, not de-overlapped

Chill's window used to sit entirely inside Focus's, and Party shared
fifteen beats with Workout — two different moods could return the same
kind of list. The windows above share four beats at most between Party
and Workout, and Focus and Chill still overlap because the moods
genuinely do: "calm enough to work to" and "calm enough to sit in" are
the same tempo, and what separates them is the centre, the loudness and
the genre words.

### Put in or taken out by hand

No gate tells every slow ballad from a rock track the estimator read at
60 BPM, and normalised files sit at −14 LUFS whatever they hold, so the
user has the last word. A track's menu has a **Moods** submenu ("Add to"
/ "Remove from", one row per mood, a tick where the track already
stands; picking a ticked row hands it back to the gates), and the
sliders button on the home section opens a dialog listing, per mood,
what was changed by hand, each change undoable
([`MoodCurationModal`](../../src/components/views/home/MoodCurationModal.tsx)).

The choices live in `mood_override` (`mood`, `track_id`, `kind` =
`include` / `exclude`). A track taken out is never drawn for that mood:
the candidate pool skips it, and album mode leaves it out of the record's
tempo fit and track budget, then drops it from the record it would
otherwise play through. A track put in plays **ahead of the
ranking**, in a fresh order each run and outside the per-artist cap, and
the ranking fills what is left of the forty. Album mode honours
removals only — it plays whole records, and one added track does not
make a record. The tile count follows both: gated tracks less those
taken out, plus those put in whatever their analysis says.

### What the home tile says

`mood_radio_counts` returns the per-mood counts **and** how much of the
library carries a tempo at all, because a thin radio has a reason the
counts cannot show: they look small without saying small _of what_. The
grid shows the coverage line only while the two numbers differ. The
subtitle now says what the radio does — it promised "tempo and energy"
while energy was a loudness ceiling on two of the five moods.
