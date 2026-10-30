# Tag workshop

The tag workshop edits the tags of many files at once: a selection of tracks, an album or a folder, in one table. It can match them with a [MusicBrainz](https://musicbrainz.org) release and write the release's tags and identifiers into the files. Nothing is written until a preview of every change, file by file and field by field, has been confirmed, and every batch it writes can be undone from a journal.

It is the file-writing counterpart of the [enrichment engine](enrichment.md), which fills gaps in the database only. The two share their matching: the workshop never decides on its own that a release is an album.

## Opening it

- a track's menu → **Edit in the tag workshop**: the selection when the track is part of one, else the track;
- a folder's menu in **Library → Folders**: every file under it, recursively;
- an album's menu (the album grid, the sidebar);
- **Settings → Library → Enrichment → Review** → **Open in the tag workshop** on an album: the album, matched at once against the release the engine proposed. That is where a doubtful match is checked track by track and, once right, written.

The view is reached through a window event (`OPEN_TAG_WORKSHOP_EVENT` in [`src/lib/tagWorkshop.ts`](../../src/lib/tagWorkshop.ts)) that `AppLayout` turns into a history entry carrying the source, so the menus need no navigation callback and **Back** returns to the same table. Remote tracks are not offered: there is no file to write.

The workshop belongs to the profile it was opened in: the layout stamps the active profile on the request, every command takes it and is refused once another profile is active (`require_profile_pool_for`), and the view shows nothing under another profile. A batch number or a track id means nothing in another profile's database, and an undo sent there would rewrite that profile's files.

At most 5,000 files open at once (`MAX_FILES`), and the view says so when a source held more; each one's tags are read from the file, through the same probe and the same tag (`primary_tag`, else the first) the scanner uses, so the table shows what a rescan would store. A file that cannot be read, or a `.dff` (no tag block this build writes), is shown greyed with the reason and is never written.

## The table

One row per file, virtualized against the page scroller ([invariant](../architecture/invariants.md#virtual-scroll-everywhere)); the header stays at the top of the page while it scrolls. Three groups of columns can be shown or hidden: **Tags** (title, artists, album, album artist, year, genres, composer), **Numbers** (track, track total, disc, disc total) and **MusicBrainz IDs**. Every cell is an input; an edited cell is highlighted until it is written or discarded.

Artists and genres are lists joined with `"; "`, the library's [one multi-artist separator](library.md#multi-artist); an Album Artist is kept whole, as the scanner keeps it.

The tools act on the ticked rows, or on every editable row when none is ticked:

- **Fill a column** — one value into one field; empty clears the field.
- **Find and replace** — in one text field or all of them (never the numbers or the identifiers, which a stray replacement would make invalid), as typed or as a regular expression (`$1` works in the replacement), case-sensitive or not.
- **Case** — Title Case (small English words such as "of", "the" stay lower case unless first or last; a word already in capitals, "AC/DC", is kept), Sentence case, UPPER, lower.
- **Number tracks** — in table order from a chosen number, restarting on each disc, with the track totals set per disc if asked.
- **MusicBrainz** — see below.

These run in the frontend: they change the table, never a file.

## Matching with MusicBrainz

The workshop asks [`resolve::resolve_album_release`](../../src-tauri/crates/core/src/enrichment/resolve.rs) — the engine's own resolution, extended to hand back the release it read — with the table's current album, artist and per-track title, number and duration, so a title corrected in the table is the title matched. The release is:

1. the one named by a link or an identifier the user pastes;
2. else the one the files' tags already name (the most common `mb_release_id` among the rows), trusted when every file pairs with one of its tracks, as the engine trusts it;
3. else the best a search finds, judged by [`enrichment::matching`](enrichment.md#matching).

[`tag_workshop::proposal::propose`](../../src-tauri/crates/core/src/tag_workshop/proposal.rs) then pairs the files with the release's tracks through the same `matching::match_album` and turns each pair into tags: the track's title and credited artists, the release title and artist credit, the year, the track and disc numbers and totals, the genres the engine would fill, and the six identifiers. The year is the release group's first, like the engine's: a remaster of a 1991 album is still a 1991 album. Files no track pairs with are listed and get nothing. The verdict and score are shown with the release; a doubtful one is a proposal like any other, to be checked before it is used.

**Use these tags in the table** copies the ticked fields into the table, nothing more. The requests go through the process-wide MusicBrainz pacer ([invariant](../architecture/invariants.md#musicbrainz-is-paced-process-wide)) and are refused in offline mode.

### The identifiers in the files

Written as MusicBrainz's own tagger writes them, the keys lofty maps to its `MusicBrainz*` items ([`edit/musicbrainz_ids.rs`](../../src-tauri/crates/app/src/commands/edit/musicbrainz_ids.rs)):

| identifier    | Vorbis / FLAC / Ogg          | ID3v2 (MP3, AAC, WAV, AIFF, DSF)         | MP4 (`----:com.apple.iTunes:…`) |
| ------------- | ---------------------------- | ---------------------------------------- | ------------------------------- |
| release       | `MUSICBRAINZ_ALBUMID`        | `TXXX:MusicBrainz Album Id`              | `MusicBrainz Album Id`          |
| release group | `MUSICBRAINZ_RELEASEGROUPID` | `TXXX:MusicBrainz Release Group Id`      | `MusicBrainz Release Group Id`  |
| recording     | `MUSICBRAINZ_TRACKID`        | `UFID` owned by `http://musicbrainz.org` | `MusicBrainz Track Id`          |
| release track | `MUSICBRAINZ_RELEASETRACKID` | `TXXX:MusicBrainz Release Track Id`      | `MusicBrainz Release Track Id`  |
| track artists | `MUSICBRAINZ_ARTISTID`       | `TXXX:MusicBrainz Artist Id`             | `MusicBrainz Artist Id`         |
| album artists | `MUSICBRAINZ_ALBUMARTISTID`  | `TXXX:MusicBrainz Album Artist Id`       | `MusicBrainz Album Artist Id`   |

"Track Id" naming the recording is the tagger's historical choice, and every reader follows it. A list of artist identifiers is one Vorbis comment or one MP4 value per identifier, and one `TXXX` frame with the values `\0`-separated in ID3v2 (the 2.4 multi-value form). DSF is written by the `id3` crate with the same frames. The scanner does not store the identifiers; the enrichment engine reads the release one back (`enrichment::tags::release_mbid`), so an album the workshop tagged needs no search on the next pass.

## Preview, then write

**Preview** sends every changed row to `tag_workshop_preview`, which reads each file **again** and returns, for each field that would change, what the file says now beside what it would say (`tag_workshop::diff`, values compared in one normalized form so `01` and `1` are no change). Files the change leaves as they are do not appear. A value that cannot be written — a year that is not a number, a malformed identifier — or a file that cannot be read blocks the write until the table is fixed.

**Write** sends exactly what the preview listed, with each changed field's old value as the preview showed it. `tag_workshop_apply` writes one file at a time:

1. it waits out a running scan — the workshop is a second writer ([invariant](../architecture/invariants.md#single-writer-to-sqlite));
2. it reads the file again and **refuses it if its tags are no longer the ones the preview showed** (`changed_since`): nothing is written that was not seen;
3. it journals the old and new values (status `pending`);
4. it pauses playback if this is the track playing ([invariant](../architecture/invariants.md#file-write-safety-on-windows)) and writes through `edit::patch_file` ([invariant](../architecture/invariants.md#tag-writes-go-through-the-concrete-tag)) — tags only: the audio bytes are copied through untouched, which the tests check for MP3, FLAC and M4A;
5. it re-hashes the whole file (BLAKE3) into `track.file_hash`, and keeps the same hash in the journal;
6. it brings the database in line through `edit::sync_db`, the relink the properties dialog runs: the track's fields, its artists split on `"; "`, its album regrouped on `(canonical_title, album_artist_id)` ([invariant](../architecture/invariants.md#album-grouping--canonical_title-album_artist_id)), its genres, its composer row, the totals. A title cleared from the file is the file's name in the library, as the scanner would store it.

Every database write is a short transaction retried on `SQLITE_BUSY`. The batch takes a `TaskHandle` (`tag_workshop` in the status bar) and stops **between two files** when cancelled; what was written stays written and journaled. One batch writes or undoes at a time. Each file holds one profile lease from its journal entry to its confirmation, so a profile switch waits for that file — up to `LEASE_DRAIN_TIMEOUT`. A file that takes longer can lose its database before its record: a write is then left `pending` and reported as `interrupted`, and an undo stops with the error, the file restored but still recorded as written (a later undo refuses it, its bytes having changed). The next file asks for a new lease and the batch stops if the profile changed; the report of what was done so far comes back as it is.

Once a file is written, nothing stops the batch any more: the hash is journaled first, and a failure of the database update after it is reported against that file (the next scan of the folder reads the file back) while the batch goes on. A file whose hash could not be recorded stays `pending` and is reported as such.

When the batch came from a MusicBrainz match:

- **Add the Cover Art Archive cover to albums that have none** fills a cover the way the engine does (`enrichment::fetch_cover`): the release's front, or the release group's, linked only to an album that has no cover — and only to the albums whose written files now name that release, never to another album the same batch touched. Never into the files, and never over a cover the album has.
- For each album a written file now names that release in (the release identifier written, or already there) — the album the file is in after the write, not one it left — an enrichment decision that says otherwise — a proposal under review, a rejection, a match with another release — is **forgotten**, so the engine's next pass reads the identifier from the files and settles it. A decision for the very release is kept, and so is every decision about an album whose files were given other tags only: a rejection stays a rejection.

## Undo

Each batch is listed under the table with its name, date and counts. **Undo this batch** (clicked twice) puts the old values back, through the same path, into every file it wrote **whose bytes still hash to what the batch left**. A file changed since — by another tagger, by a later batch — is refused and reported, never overwritten; so is a file journaled but never confirmed (`pending`, an interrupted batch), whose bytes nothing describes. A field the file did not carry before is removed. The batch is marked undone once none of its files is left written; until then the others can be retried, once the newer batch is undone, say.

The year is restored as a year: a file whose tag held a full date (`1991-09-24`) and was given another year gets `1991` back, not the day.

## Storage

Migration `20261030180000_tag_workshop_journal.sql` — see [storage](../architecture/storage.md#datadb-per-profile):

- `tag_workshop_batch` — one row per batch: when, its name, the release it was matched with, when it was undone;
- `tag_workshop_file` — one row per file a batch touched: its path, its track (`ON DELETE SET NULL`: the record of what was written into a file outlives the track), its status (`pending`, `written`, `failed`, `undone`), the hash the write left, the error of a refused write;
- `tag_workshop_field` — the old value (`NULL` when absent) and the new one of every field written.

A batch that wrote nothing is not kept.

## Where the code is

- [`waveflow_core::tag_workshop`](../../src-tauri/crates/core/src/tag_workshop.rs) — the fields, the normalized form, validation, the diff; [`proposal`](../../src-tauri/crates/core/src/tag_workshop/proposal.rs) (feature `musicbrainz`); [`journal`](../../src-tauri/crates/core/src/tag_workshop/journal.rs) (feature `sqlite`).
- [`commands/tag_workshop.rs`](../../src-tauri/crates/app/src/commands/tag_workshop.rs) — open, preview, apply, undo, the batches, the match; [`read.rs`](../../src-tauri/crates/app/src/commands/tag_workshop/read.rs) reads a file's fields (DSF through the `id3` crate) and builds the `TrackEdit` that writes them.
- [`TagWorkshopView`](../../src/components/views/TagWorkshopView.tsx) and [`WorkshopPreviewModal`](../../src/components/views/tagWorkshop/WorkshopPreviewModal.tsx).
