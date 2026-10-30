# Library enrichment

The enrichment engine completes the local library from [MusicBrainz](https://musicbrainz.org) and the [Cover Art Archive](https://coverartarchive.org): it pairs every album with a MusicBrainz release, every track with a recording, every artist with a MusicBrainz artist, and fills from them what the library is missing — genres, release dates, credits, covers, and the facts of an artist's page.

**Settings → Library → Enrichment**. Off by default: a pass sends the library's album titles and artist names to a third party, one request a second, for as long as the library takes.

## Rules

- **The database only, never a file.** Nothing the engine does opens an audio file for writing. Writing identifiers and genres into the files themselves belongs to the tag workshop, which reuses the engine's matching.
- **Fill the gaps, never overwrite.** A genre, a year, a release date, a cover the library already has — from the file's tags or typed by the user — stays. Every write is conditional on the field being empty (`year IS NULL`, a track with no genre row, an album with no cover).
- **What was filled is recorded.** Each decision keeps `filled_*` flags and the genres it gave (`musicbrainz_filled_genre`), so **Undo** takes back exactly that, and leaves alone a value the user changed since. A value the user writes in the tag editor, or a genre a rescanned file carries, is theirs even when it equals the filled one: the editor (`store::release_track_fields`, `release_album_years`) and the scanner's genre link drop the provenance for it. The editor also sends the year only when the user changed it, so a save for the title alone never writes a filled year into the file.
- **Applied on its own only when sure.** A confident album match is applied at once; a doubtful one waits in the review. **An artist is never applied on its own**: a name is too ambiguous — two bands are called Nirvana — so every artist match waits for the user, with the disambiguation MusicBrainz gives.
- **Which fields** — genres, release dates, missing covers, credits — are switches of their own. A field turned off is never written, automatically or on review; a running pass reads the switches again before every album.

## How a pass runs

A pass starts when the engine is turned on, after every finished scan (any scan, not only one that added files — see [below](#surviving-the-scanner)), when a profile opens, at startup, and on **Run now**. A profile switch stops the running pass at its next album and starts one for the profile opened. The pass holds no lease on the database while it waits on the network — it takes one per read or write, for its own profile only — so the switch does not wait for it (measured: 26–36 ms during a pass, against the 5 s lease drain timeout before). One pass at a time; a request made while one runs is honoured when it ends, unless the user stopped it. It shows in the task bar as _Library enrichment_ and stops between two albums.

1. **Refill** what a scan emptied since the last pass, from the stored decisions, with no network.
2. **Albums** with no decision, and `no_match` ones older than thirty days (the catalogue grows). For each:
   - the release identifier the files carry, if a MusicBrainz tagger wrote one (`MUSICBRAINZ_ALBUMID` and its ID3 / MP4 spellings, read from the first track) — a release named by the files needs no search;
   - otherwise a search on the album title and artist (a phrase query, then the same words as loose terms if the phrase finds nothing), the hits ranked on their summary, and up to three of them looked up with their tracklist, stopping at the first confident one;
   - the verdict, written in one transaction; a confident album is applied, its credited artists proposed for review, and its cover fetched when it has none.
3. **Covers** an earlier pass could not fetch.
4. **Artists** no matched album named, searched by name.

A failed lookup writes nothing — the album stays pending for the next pass — and five failures in a row end the pass: the service is down, and asking a thousand more times changes nothing. A refusal that will not change on the next try and belongs to this album — an answer too large to read, or a `400` / `413` / `414` / `422` — is recorded as no match, so it neither ends the pass nor sits first in the queue of every pass after. Any other status (`401`, `403`…) or an answer that no longer parses is the same for every album: it stays a failure and counts toward the five that end the pass, rather than marking the whole library unmatched for a month.

The engine is the second background writer next to the analysis sweep, with the same shape ([invariant](../architecture/invariants.md#single-writer-to-sqlite)): it parks while a scan runs, writes each album in one short transaction, retries on `SQLITE_BUSY`, and never holds a transaction across a network wait.

## The service's rules

MusicBrainz asks for at most one request a second per IP, and a User-Agent naming the application and a way to reach it. The clients in [`waveflow_core::metadata::musicbrainz`](../../src-tauri/crates/core/src/metadata/musicbrainz.rs) enforce both: every request waits for its slot on one process-wide `Pacer` (1.1 s apart), and the agent is `WaveFlow/<version> ( https://waveflow.app )`. A `503` or `429` (the service's rate limiter, and the one in front of it) and a `502` / `504` are retried after `Retry-After`, or a doubling backoff, up to four attempts, and push every later slot back too; past that the failure is a failure, never an empty answer the engine would remember as "no match". The Cover Art Archive client shares the same pacer, and follows its redirects to `archive.org` over https only.

Offline mode ([invariant](../architecture/invariants.md#process-wide-offline-mode)) stops a pass before its next album, and accepting a proposal refuses while it is on.

## Matching

[`enrichment::matching`](../../src-tauri/crates/core/src/enrichment/matching.rs) — pure arithmetic, no network, no database, so the tag workshop reuses it as is.

- **Tracks** go through [`album_match`](../../src-tauri/crates/core/src/metadata/album_match.rs), the matcher the Deezer tag fetch already uses: title, duration and track number weighted 0.60 / 0.25 / 0.15, a global greedy pairing so one generic title cannot swallow a record, and a pair is _confident_ at 0.85. A guest credit is cut from both titles before they are compared — "Waterfalls (feat. …)" against "Waterfalls" — but a "(Remix)" or a "(Live)" is not: it names another recording.
- **The album title** is compared as written and with an edition suffix set aside on both sides ("(Remastered)", "[Deluxe Edition]", " - Single", " - EP"), the better of the two kept.
- **The artist** goes through the shared name matcher, [`name_match::names_match`](../../src-tauri/crates/core/src/metadata/name_match.rs) — case, diacritics, punctuation, a "& The Band" extension. One local name agreeing with one credited name ("Zedd, Alessia Cara" against "Zedd & Alessia Cara") scores 0.95; anything else is an edit-distance similarity capped below the confident line, so a near-miss spelling is never applied on its own.

The album score weights title, artist and tracks 0.30 / 0.20 / 0.50: the tracks weigh most, because they are the one signal an edition, a live album or a namesake cannot fake.

### What "confident" means, and how it was set

Confident — applied without asking — needs **all** of:

- the album titles agree at ≥ 0.90,
- the artist agrees at ≥ 0.90 (only the shared matcher's yes reaches it),
- **every** local track is paired with a recording, and every pairing is itself confident.

Anything short of that with an album score ≥ 0.60 is a proposal; below, no match. A release the files' own tags name is confident as soon as every local track pairs, whatever the title or the credit say.

The line was measured on the two libraries of the development sandbox (copies of real profiles, 1,198 and 264 albums), 90 albums sampled evenly from each, every candidate looked up and checked by hand against its local album:

| Verdict             | Albums           | Checked by hand                                                                                                                                                                              |
| ------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Confident (applied) | 114 / 180 (63 %) | All 114 are the right album. No false positive — at that sample size, a rate above about 2.6 % would have shown at least one (rule of three, 95 %).                                          |
| Doubtful (review)   | 26 (14 %)        | About 23 are the right album, but not one that may be applied blind: one is wrong (a film soundtrack matched with a trilogy compilation), two are a single against its two-track local copy. |
| No match            | 25 (14 %)        | The best candidate was wrong in all but a handful, which the title alone could not separate ("Comin' Home Baby" against "Comin' Home Baby (from Palm Royale Season 2)").                     |
| Nothing found       | 15 (8 %)         | No search hit: self-released tracks MusicBrainz does not list.                                                                                                                               |

None of the sampled files carried a MusicBrainz identifier, so the tag path was not exercised by the measure. Two rules come from the first round of it, which applied 93 albums: guest credits in track titles ("Laugh Now Cry Later (feat. Lil Durk)") and album artists written with commas ("Zedd, Alessia Cara") sent right albums to review; with both handled, 21 more are applied and none of the 93 changed.

The libraries are mostly singles — one track per "album" — which is the hard case: the tracklist then proves the least. A library of full albums pairs a dozen tracks per decision, where an error needs a dozen coincidences.

## Review

**Settings → Library → Enrichment → Review** opens a window with five tabs: albums and artists waiting for a decision, applied albums, accepted artists, and what was rejected. An album is shown beside the release it was matched with (artist, track count, original date, country, genres), an artist beside the MusicBrainz artist and its disambiguation; both link to the MusicBrainz page.

- **Accept** looks the release or the artist up again and applies it, so nothing stale is written.
- **Reject** means it is never proposed again on its own; **Try again** in the Rejected tab forgets the decision so the next pass looks again.
- **Undo** on an applied album takes back what it filled and marks it rejected.
- **Accept the credited artists** accepts, in one go, every pending artist that a matched album credits under the same name — the strongest evidence an artist match can have.

The lists have no natural bound, so they are virtualized against the window's own scroller; one answer holds at most 5,000 decisions, the most recent first. **Accept the credited artists** looks each one up for its facts at one a second, so accepting hundreds takes minutes, with the window's button busy until it is done.

## Where the results show

- **Genres** — in `track_genre`, so everywhere genres already show: the library, the genre pages, the smart-playlist rules. Only for paired tracks with no genre at all, and at most three per album: the release group's most voted, and only those with at least a quarter of the top genre's votes (one editor's stray tag beside fifty votes for another is noise). Capitalised per word and per hyphen ("Lo-Fi", "Hip Hop").
- **Dates** — the release group's _first_ release date into `album.release_date`, its year into `album.year` and into the paired tracks' `year`, where empty. The original date, not the pressing's: a 2011 remaster of a 1991 album is still a 1991 album.
- **Covers** — the release's front cover from the Cover Art Archive at 500 pixels, or the release group's when the pressing has none, stored like any other cover and linked with `album.artwork_source = 'coverartarchive'`. The `artwork` row says `manual`: its CHECK constraint predates the archive and cannot be widened ([invariant](../architecture/invariants.md#never-drop-table-a-parent-table-in-a-migration)), and provenance lives on the link since #401. Like a Deezer cover, a fetched cover is not replaced by a sidecar found later; undo the album or set the cover by hand.
- **Credits** — composers, lyricists, writers, arrangers, performers with their instrument or vocal part, producers, mix and recording engineers, conductors, remixers, as **names** in `track_credit` (not `artist` rows: a session drummer is not an artist of the library). Shown in **Track properties → Credits**, grouped by role, at most forty per track.
- **Artist facts** — kind, area or country, active years, disambiguation and genres, on the artist's page once the user accepted the match.

## Surviving the scanner

The scanner rewrites a track's row, and empties and refills its genres, whenever the file changes — and a rating or a tag WaveFlow writes changes the file. A file with no genre tag therefore loses the engine's genre on the next scan of it. That is why a pass follows **every** scan and starts by refilling, from the stored decisions and with no network, what the scan emptied (`store::reapply_all`). The refill is idempotent: a field already filled is not empty any more.

A consequence worth knowing: a genre the user deletes from a track whose album was enriched comes back on the next pass, because to the engine it is a gap. Undoing the album is how to keep it away.

## Storage

Five side tables, cascade-deleted with the album, track or artist they are about, and never read by the scanner — see [storage](../architecture/storage.md#datadb-per-profile): `musicbrainz_album`, `musicbrainz_track`, `musicbrainz_filled_genre`, `track_credit`, `musicbrainz_artist`. Migration `20261030153000_musicbrainz_enrichment.sql`.

## Where the code is

| Piece                                  | Where                                                                                                                                                                              |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Clients, pacing, retries               | [`core/src/metadata/musicbrainz.rs`](../../src-tauri/crates/core/src/metadata/musicbrainz.rs), [`coverartarchive.rs`](../../src-tauri/crates/core/src/metadata/coverartarchive.rs) |
| Matching (pure)                        | [`core/src/enrichment/matching.rs`](../../src-tauri/crates/core/src/enrichment/matching.rs)                                                                                        |
| What a release contributes             | [`core/src/enrichment/extract.rs`](../../src-tauri/crates/core/src/enrichment/extract.rs)                                                                                          |
| Search, lookup, verdict                | [`core/src/enrichment/resolve.rs`](../../src-tauri/crates/core/src/enrichment/resolve.rs)                                                                                          |
| Identifiers the files carry            | [`core/src/enrichment/tags.rs`](../../src-tauri/crates/core/src/enrichment/tags.rs)                                                                                                |
| Decisions, gap filling, undo           | [`core/src/enrichment/store.rs`](../../src-tauri/crates/core/src/enrichment/store.rs)                                                                                              |
| The pass, the task, the covers on disk | [`app/src/commands/enrichment/engine.rs`](../../src-tauri/crates/app/src/commands/enrichment/engine.rs)                                                                            |
| Commands                               | [`app/src/commands/enrichment.rs`](../../src-tauri/crates/app/src/commands/enrichment.rs)                                                                                          |
| Settings card, review window           | [`EnrichmentCard.tsx`](../../src/components/views/settings/EnrichmentCard.tsx), [`EnrichmentReviewModal.tsx`](../../src/components/common/EnrichmentReviewModal.tsx)               |
