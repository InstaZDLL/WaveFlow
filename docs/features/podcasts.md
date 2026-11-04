# Podcasts

Standalone podcasts in the desktop app, off by default. The design and its reasoning are in [RFC-009](../rfcs/RFC-009-podcasts.md); this page says what exists today.

| Lot | What                                                                               | State     |
| --- | ---------------------------------------------------------------------------------- | --------- |
| 1   | `waveflow_core::podcast`: feeds, OPML, chapters, transcripts, on hostile input     | This page |
| 2   | Tables, fetch rules, subscribe, refresh, OPML import and export, commands, setting | This page |
| 4   | Downloads: managed folder, quota, cancel, Disk usage                               | This page |
| 3+  | Playback, the page, chapters and transcript panel, the server's podcasts           | Not yet   |

There is no page yet, so nothing here is reachable from the interface: the commands and the setting exist for the lots that build on them.

## The switch

`podcasts.enabled`, per profile, default `false`. Turning it on is an explicit choice: only the confirmation of the warning dialog writes it, through `acceptWarning` of [`usePodcastsEnabled`](../../src/hooks/usePodcastsEnabled.ts). The dialog is a later lot. Resetting settings returns it to off, and a profile imported or restored comes back off, since having accepted belongs to the person on this machine: the archive leaves the setting behind, and the import removes it whatever the archive says.

**The backend enforces it.** Off, every command that would contact anybody refuses with `podcast: disabled` before doing so, and so does the timer, which then does not run at all. A bug in the interface cannot send traffic the user did not accept. What was stored stays readable, and unsubscribing and exporting still work, since leaving must not depend on having opted in.

## The timer

[`podcast::schedule`](../../src-tauri/crates/app/src/podcast/schedule.rs) waits on a `Notify` while the switch is off. It wakes when the key is written (through the `profile-setting:changed` event every preference write emits) and when another profile becomes active.

On, it refreshes every six hours while the app runs. **The six hours count from the last check**, stored per profile in `podcasts.last_check_at`, not from launch, so a session shorter than that still gets its check. One that is overdue, or a profile never checked, runs five minutes after the loop looks: **nothing is requested during startup**, however overdue. A pass over every channel stamps the profile, including one the user stopped, or the timer would start it again at once. A pass over one channel does not, and neither does one that reached nobody.

Every request checks `offline::is_offline()` first, each hop of a redirect included. A refresh stopped by offline mode blames no channel: nothing is written to `last_error`.

## Fetching

[`podcast::fetch`](../../src-tauri/crates/app/src/podcast/fetch.rs) is the one way out for anything a feed names. A feed address is typed by a person, and every address after it is read out of a response.

- **http or https, a host, no user information**, and a hop is checked for that before it is contacted.
- **`may_open` for every hop**, against an `Origin` decided once from the address the person typed. A feed whose address is public can only lead to public addresses, so a feed on the internet cannot make the player reach the user's router. A feed typed with a local address may name local ones. As with the artwork downloader, DNS rebinding is not defended against.
- **Redirects are followed by hand, at most five**, and never from https to http. The client follows none by itself.
- **Bodies are bounded while they are read**: 16 MiB for a feed, 4 MiB for a picture. A declared length past the ceiling is refused unread, and one that lies is stopped where it passes it.
- **Each request runs under a deadline**, redirects and body included: 60 s for a feed, 30 s for a small file.

`Fetcher::open` answers a `reqwest::Response` and leaves the deadline to its caller, so a stream and a download can use the same rules on budgets of their own.

### An address reaches no log

A private feed carries its token in the path as often as in the query, so `redact_url`, which masks userinfo and query, is not enough. **A feed or enclosure address is not logged at all, redacted or not**, and neither is a `reqwest::Error`, whose text quotes it. A failure is logged and stored as one word from `FetchFailure::code`:

`address_refused`, `too_many_redirects`, `too_large`, `unreached`, `http_<status>`, `invalid_feed`, `not_audio`, `quota_exceeded`

No row sent to the webview holds an address either: a channel says `has_feed_address`, an episode `has_enclosure`, and whatever opens one asks by id. A refused command rejects with `podcast: <word>`, the list above plus `disabled`, `offline`, `profile_changed`, `channel_gone` (unsubscribed from while a subscription to it was finishing) and `episode_gone`. A failed download carries the same words in its event, plus `write_failed` for a disk or a database that refused.

### An address stays out of an archive

A backup or a profile export is made to travel, so `profile_io` blanks every podcast address in the copy it archives, on the same path as a share's password, and always `VACUUM`s it. A row an ordinary write made grow, or one removed, leaves its old bytes in the file, addresses included, and even a profile with no channel left may hold every address it ever had; the rebuild leaves nothing but what the rows now say.

The import runs the same blanking, whatever the archive says: one may come from another build or from somebody else, and an address in it is not one this person typed, so the timer must not contact what it names.

What survives is what a channel and an episode are known by: `feed_key` and `item_key`, hashes of the address and the guid. Subscribing again to the same address, by hand or through OPML, finds the row and gives it its addresses back, with every position and played state intact. Until then the channel lists as `has_feed_address: false`, and nothing refreshes it.

The guid is stored only as that hash, because it falls back to the enclosure's address and often is that address anyway.

An **OPML export** carries the addresses: it is a deliberate act, and the dialog that does it has to say so.

## Reading a feed

[`waveflow_core::podcast`](../../src-tauri/crates/core/src/podcast.rs) reads RSS 2.0, Atom and RSS 1.0 with `roxmltree`. A DTD is refused outright, so no entity can be declared, let alone expanded. There is a node ceiling, the bytes are bounded before they are decoded, and every text kept afterwards. Bytes that are not in the encoding the document declares refuse it, since guessing would store mojibake as a title.

- **At most 500 episodes a feed**, newest first. Two items with one guid keep the newer.
- **An item without a usable enclosure is not an episode**: not http(s), holding credentials, or declared as video, an image, a page or a PDF. A missing or odd type proves nothing, since feeds get this wrong, and the bytes decide when a file is opened.
- **An impossible date is no date**, and so is one before 1990 or after 2100, which would otherwise hold the top of a list. Undated episodes sort last.
- **A title may be empty**: the parser has no locale to choose a word in, so the page shows its own.

A refresh matches on the guid's hash. A re-published episode is updated and keeps its position, and one the user removed (`deleted_at`) stays removed and is not even updated. A row no longer in the feed stays, since its position is the user's, up to 2 000 per channel: a feed handing out fresh guids at every refresh must not grow the table for ever. What is pruned is the oldest rows that hold nothing of the user's.

A refresh sends back the `ETag` and `Last-Modified` it was given, so an unchanged feed costs a 304.

**A feed that has moved stays followed.** A 301 or 308 updates the stored address, to the last hop of the unbroken run of permanent redirects a request began with; a 302 or 307 changes nothing, and neither does what comes after one. `itunes:new-feed-url` does the same once the address has passed the rules of a redirect, judged by where the announcing feed was, and has been read as a feed: a typo in someone's feed must not cost its subscribers the show. The row, its episodes and their positions stay. A channel does not move onto an address another channel follows.

### Text is plain

[`markup`](../../src-tauri/crates/core/src/markup.rs) strips tags, comments, scripts and styles in the backend, and decodes entities once. Nothing from a feed reaches the DOM as HTML. Its scan is linear, because the input is hostile and a stripper that looks ahead from every `<` is quadratic on a string of `<a `. `<` starts a tag only where one could start, so `I <3 you` keeps its text.

### The picture

A channel's picture is fetched at subscribe and whenever its address moves, under the same rules, then checked: JPEG, PNG, WebP or GIF, and no side above 8 192 pixels, read from the header alone so a small file declaring an enormous image is refused without being decoded. It lands in the shared artwork cache, so opening the page contacts nobody. A failure costs the channel its picture and nothing else, and the next refresh tries again, including when the cache was cleared.

## Subscribing

A subscription fetches the feed before anything is stored, so a typo leaves nothing behind. Subscribing to an address already followed answers its channel without contacting anybody.

**OPML** import subscribes to each feed of a list in turn, and one that fails is reported by its title and word while the next is tried. A list is a file someone may have been handed, so **it may name public addresses only**: a feed on the local network has to be typed, or a list could make the app probe the network, each line's word saying what answered. It reads only a regular file named `.opml` or `.xml`, at most 16 MiB, and at most 2 000 feeds. Folders may nest to any depth.

A refresh writes into the row it read, by id, and never creates one, so unsubscribing while a feed is being read sticks.

A refresh and an import each take a `TaskHandle` of kind `podcast_refresh`, one pass at a time. A pass parks behind a running scan and retries a write on `SQLITE_BUSY`. No database lease is held across a network wait: the pool is leased again around each write, pinned to the profile the pass began for, and a profile switch stops the pass instead of letting it write into the wrong profile.

## Downloads

[`podcast::download`](../../src-tauri/crates/app/src/podcast/download.rs) keeps an episode on disk, one at a time, the others waiting in line.

**The folder** is `<cache_root>/profiles/<id>/podcast-downloads`, per profile and never a library folder, so neither the scanner nor the watcher sees it. It moves with the caches, since the worst a lost file costs is fetching the episode again. Nothing evicts it. An archive or a backup does not carry the files, and blanks `download_file`, `download_size` and `downloaded_at`, so a restored row does not promise a file that stayed behind.

**The name** is the episode's id and a suffix chosen from the first bytes (`sniff_audio`), such as `42.mp3`. Nothing a feed says names a path. A body that is not audio is refused as `not_audio` before anything is created, and so is one too short to judge. `stored_file` is how anything opens a stored name: one that does not have our shape is refused rather than joined to a path, since a row may have come from somewhere else.

**The enclosure** goes through `Fetcher::open`, each hop judged against the address its channel was typed with. A channel with no address reaches nothing local.

**Two ceilings, counted while the body is read**: 1 GiB an episode (`too_large`) and the quota, 10 GiB by default (`quota_exceeded`), through the setting `podcasts.download_quota_gb` and [`usePodcastDownloadQuota`](../../src/hooks/usePodcastDownloadQuota.ts). What counts against the quota is what the rows record. A declared length past a ceiling is refused without reading, which only ever costs a host its own episode; a length that understates gains nothing, since only what arrives is stored, and one that overstates ends in `unreached` with nothing left.

**Time.** Opening runs under 60 s, every hop included. A body has six hours, and must bring 64 KiB in every minute: a deadline long enough for a slow line would let a host sending a byte now and then hold up the line behind it for hours.

**Bytes reach their name in one rename**, on the disk first, and the row comes last. A partial file is `<id>.part`, removed however the download ends. On Windows the rename is retried for a moment, since an indexer or an antivirus commonly opens a file the moment it is closed.

**The switch and offline mode** are looked at again every second while a body is read, and stop it: `disabled` or `offline`, with nothing left. Both also refuse a download that has not started. A file already on disk stays readable and removable with either on.

**The profile.** No lease is held across the network. The pool is leased again for each check, pinned to the profile the download began for, so a switch stops the download within a second and writes nothing, in either profile. The lease is held across the rename and the row. A job in line for a profile no longer active ends as `cancelled`.

**Cancelling** is `podcast_cancel_download` or the status bar: the job takes a `TaskHandle` of kind `podcast_download`, and the registry calls the stop the job already has, which interrupts a wait on a host too. Nothing stays.

**Deleting** removes the file, then what the row says of it, and the row stays with its position. Unsubscribing takes the channel's jobs out of the line, stops the one running, and removes its files once the rows have gone.

**What a crash leaves** is cleaned up by `sweep`, at launch and each time a profile becomes active, parked behind a scan: a partial file, a file no row names (killed between the rename and the row), a row whose file is gone. A name that is not ours is left alone. When the caches' drive is unplugged and the session fell back to the default location, rows are kept, or every download would be lost by the time it returns.

**The event** `podcast:download` carries `{ episodeId, state, receivedBytes, totalBytes, errorWord }`, `state` being `queued`, `running`, `done`, `failed` or `cancelled`, at most about four times a second. A view opened meanwhile subscribes first, then calls `podcast_download_queue`. `done` is followed by `podcast:updated`.

## Chapters and transcripts

The readers exist; fetching and showing them is a later lot.

**Chapters** come from the first source with a valid chapter, in this order, and sources are not merged: the feed's `podcast:chapters` JSON file, inline `psc:chapters`, `CHAP` frames in an MP3 (`chapters::choose`). A chapter is a start time and a title, sorted, with impossible times dropped, and one the JSON marks `toc: false` is not listed. Its image and link are ignored, since each would be another address out of a feed.

**Transcripts** are chosen by format (`transcript::choose`): WebVTT, then SRT, then Podcasting 2.0 JSON, then plain text or HTML. The timed ones become a `lyrics::Document` through `lyrics::vtt`, `lyrics::srt` and `lyrics::transcript_json`, so the panel can reuse the line renderer. They are readers only, and not a `lyrics::Format`: a transcript is never looked up elsewhere or written to a file. A publisher writing one JSON segment per word gets its words gathered into lines.

## Commands

| Command                 | Does                                                                     |
| ----------------------- | ------------------------------------------------------------------------ |
| `podcast_subscribe`     | Fetches and follows a feed; answers its channel                          |
| `podcast_unsubscribe`   | Forgets a channel and its episodes, and removes their downloads          |
| `podcast_list_channels` | Every channel of the active profile                                      |
| `podcast_list_episodes` | A page of a channel's episodes, newest first                             |
| `podcast_refresh`       | Asks one channel, or all of them                                         |
| `podcast_import_opml`   | Subscribes to every feed of a file                                       |
| `podcast_export_opml`   | Writes every address to a file                                           |
| `podcast_download_episode` | Puts an episode in line to be kept on disk                            |
| `podcast_cancel_download` | Takes it out of the line, or stops it where it is                      |
| `podcast_delete_download` | Removes its file; the row stays                                        |
| `podcast_download_usage` | `{ usedBytes, quotaBytes }`                                             |
| `podcast_download_queue` | What is under way, for a view opened meanwhile                          |

Typed wrappers are in [`src/lib/tauri/podcast.ts`](../../src/lib/tauri/podcast.ts). The event `podcast:updated` carries `channel_id`, and `podcast:download` is described under [Downloads](#downloads).
