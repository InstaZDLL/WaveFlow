# Podcasts

Standalone podcasts in the desktop app, off by default. The design and its reasoning are in [RFC-009](../rfcs/RFC-009-podcasts.md); this page says what exists today.

| Lot | What                                                                               | State     |
| --- | ---------------------------------------------------------------------------------- | --------- |
| 1   | `waveflow_core::podcast`: feeds, OPML, chapters, transcripts, on hostile input     | This page |
| 2   | Tables, fetch rules, subscribe, refresh, OPML import and export, commands, setting | This page |
| 3   | Playback: session, stream with hand-followed redirects, resume, played state, speed | This page |
| 4   | Downloads: managed folder, quota, cancel, Disk usage                               | This page |
| 5+  | The page, chapters and transcript panel, the server's podcasts                     | Not yet   |

There is no page yet, so the only part of this reachable from the interface is the player while an episode plays: the commands and the setting exist for the lots that build on them.

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
- **Bodies are bounded while they are read**: 32 MiB for a feed, 4 MiB for a picture. A declared length past the ceiling is refused unread, and one that lies is stopped where it passes it.
- **Each request runs under a deadline**, redirects and body included: 60 s for a feed, 30 s for a small file.

`Fetcher::open` answers a `reqwest::Response` and leaves the deadline to its caller, so a stream and a download can use the same rules on budgets of their own.

### An address reaches no log

A private feed carries its token in the path as often as in the query, so `redact_url`, which masks userinfo and query, is not enough. **A feed or enclosure address is not logged at all, redacted or not**, and neither is a `reqwest::Error`, whose text quotes it. A failure is logged and stored as one word from `FetchFailure::code`:

`address_refused`, `too_many_redirects`, `too_large`, `unreached`, `http_<status>`, `invalid_feed`, `not_audio`, `quota_exceeded`

No row sent to the webview holds an address either: a channel says `has_feed_address`, an episode `has_enclosure`, and whatever opens one asks by id. A refused command rejects with `podcast: <word>`, the list above plus `disabled`, `offline`, `profile_changed`, `channel_gone` (unsubscribed from while a subscription to it was finishing), `episode_gone` (no such row, or one the user removed) and `no_source` (no file and no address: a restored profile, until the feed is subscribed to again). A failed download carries the same words in its event, plus `write_failed` for a disk or a database that refused.

### An address stays out of an archive

A backup or a profile export is made to travel, so `profile_io` blanks every podcast address in the copy it archives, on the same path as a share's password, along with what an archive has no business vouching for: the name of a downloaded file and the chapter and transcript caches. It then `VACUUM`s the copy if a secret was blanked or the profile has ever held a channel, which `sqlite_sequence` remembers. A row an ordinary write made grow, or one removed, leaves its old bytes in the file, addresses included, and even a profile with no channel left may hold every address it ever had. A profile that never held one pays nothing for this.

The import runs the same blanking, whatever the archive says: one may come from another build or from somebody else, and an address in it is not one this person typed, so the timer must not contact what it names. Before it, every trigger and view the migrations did not make is dropped, since a trigger is code the archive brought along and could undo the blanking on any later write, and one of ours that was changed or removed is put back. What survives the blanking anyway refuses the archive.

What survives is what a channel and an episode are known by: `feed_key` and `item_key`, hashes of the address and the guid. Subscribing again to the same address, by hand or through OPML, finds the row and gives it its addresses back, with every position and played state intact. Until then the channel lists as `has_feed_address: false`, and nothing refreshes it.

The guid is stored only as that hash, because it falls back to the enclosure's address and often is that address anyway.

An **OPML export** carries the addresses: it is a deliberate act, and the dialog that does it has to say so.

## Reading a feed

[`waveflow_core::podcast`](../../src-tauri/crates/core/src/podcast.rs) reads RSS 2.0, Atom and RSS 1.0 with `roxmltree`. A DTD is refused outright, so no entity can be declared, let alone expanded. There is a node ceiling, the bytes are bounded before they are decoded, and every text kept afterwards. The parser spends quadratic time on attributes and namespaces, which its ceiling does not count, and cannot be stopped once it runs, so [`xml_guard`](../../src-tauri/crates/core/src/xml_guard.rs), a linear scan, refuses first what real documents never do: more than 32 attributes on a tag or 2 million in a document, 32 namespaces in scope, 100 000 copies of them in a document. A document may decode to at most twice its byte bound, and at most 20 000 episodes are built from it before the newest are picked. TTML lyrics pass through the same guard. Bytes that are not in the encoding the document declares refuse it, since guessing would store mojibake as a title.

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

**OPML** import subscribes to each feed of a list in turn, and one that fails is reported by its title and word while the next is tried. A list is a file someone may have been handed, so **it may name public addresses only**: a feed on the local network has to be typed, or a list could make the app probe the network, each line's word saying what answered. It reads only a regular file named `.opml` or `.xml`, at most 32 MiB, and at most 2 000 feeds. Folders may nest to any depth.

A refresh writes into the row it read, by id, and never creates one, so unsubscribing while a feed is being read sticks.

A refresh and an import each take a `TaskHandle` of kind `podcast_refresh`, one pass at a time. A pass parks behind a running scan and retries a write on `SQLITE_BUSY`. No database lease is held across a network wait: the pool is leased again around each write, pinned to the profile the pass began for, and a profile switch stops the pass instead of letting it write into the wrong profile.

## Downloads

[`podcast::download`](../../src-tauri/crates/app/src/podcast/download.rs) keeps an episode on disk, one at a time, the others waiting in line.

**The folder** is `<cache_root>/profiles/<id>/podcast-downloads`, per profile and never a library folder, so neither the scanner nor the watcher sees it. It moves with the caches, since the worst a lost file costs is fetching the episode again. Nothing evicts it. An archive or a backup does not carry the files, and blanks `download_file`, `download_size` and `downloaded_at`, so a restored row does not promise a file that stayed behind.

**The name** is the episode's id and a suffix chosen from the first bytes (`sniff_audio`), such as `42.mp3`. Nothing a feed says names a path. A body that is not audio is refused as `not_audio` before anything is created, and so is one too short to judge. `stored_file` is how anything opens or removes a stored name, playback included: one that is not this episode's, in that shape, is refused rather than joined to a path, since a row may have come from somewhere else.

**The enclosure** goes through `Fetcher::open`, each hop judged against the address its channel was typed with. A channel with no address reaches nothing local.

**Two ceilings, counted while the body is read**: 1 GiB an episode (`too_large`) and the quota, 10 GiB by default (`quota_exceeded`), through the setting `podcasts.download_quota_gb` and [`usePodcastDownloadQuota`](../../src/hooks/usePodcastDownloadQuota.ts). What counts against the quota is what the rows record. A declared length past a ceiling is refused without reading, which only ever costs a host its own episode; a length that understates gains nothing, since only what arrives is stored, and one that overstates ends in `unreached` with nothing left.

**Time.** Opening runs under 60 s, every hop included. A body has six hours, and must bring 64 KiB in every minute: a deadline long enough for a slow line would let a host sending a byte now and then hold up the line behind it for hours.

**Bytes reach their name in one rename**, on the disk first, and the row comes last. A partial file is `<id>.part`, removed however the download ends. On Windows the rename is retried for a moment, since an indexer or an antivirus commonly opens a file the moment it is closed.

**The switch and offline mode** are looked at again every second while a body is read, and stop it: `disabled` or `offline`, with nothing left. Both also refuse a download that has not started. A file already on disk stays readable and removable with either on.

**The profile.** No lease is held across the network. The pool is leased again for each check, pinned to the profile the download began for, so a switch stops the download within a second and writes nothing, in either profile. The lease is held across the rename and the row. A job in line for a profile no longer active ends there, without an event.

**Cancelling** is `podcast_cancel_download` or the status bar: the job takes a `TaskHandle` of kind `podcast_download`, and the registry calls the stop the job already has, which interrupts a wait on a host too. Nothing stays.

**Deleting** removes the file, then what the row says of it, and the row stays with its position. Unsubscribing takes the channel's jobs out of the line, stops the one running, and removes its files once the rows have gone.

**What a crash leaves** is cleaned up by `sweep`, at launch and each time a profile becomes active, parked behind a scan: a partial file, a file no row names (killed between the rename and the row), a row whose file is gone. A name that is not ours is left alone. When the caches' drive is unplugged and the session fell back to the default location, rows are kept, or every download would be lost by the time it returns.

**The event** `podcast:download` carries `{ episodeId, state, receivedBytes, totalBytes, errorWord }`, `state` being `queued`, `running`, `done`, `failed` or `cancelled`, at most about four times a second, and only about the active profile: an episode's id is unique inside a profile only, so a job of the last one says nothing more once it has been left. A view opened meanwhile subscribes first, then calls `podcast_download_queue`. `done` is followed by `podcast:updated`.

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

## Playing an episode

[`podcast::playback`](../../src-tauri/crates/app/src/podcast/playback.rs) over [`podcast::stream`](../../src-tauri/crates/app/src/podcast/stream.rs). **An episode is not a track.** It has no `track` row and plays under a negative id through a `DetachedSession`, the queue in memory the remote queue and the [files played without importing them](playback.md#playing-files-without-importing-them) use, so the ordering rules of #622 hold for it. It writes no `play_event`, sends no scrobble and no "playing now", and counts in no statistic. It takes no fade, no gapless hand-off and no AutoMix: the prefetch is only answered for a library track. ReplayGain does not touch it, neither its fallback nor its pre-amp, since nothing measured it. Discord's card is cleared rather than updated; the OS media overlay and the tray say what plays.

`podcast_play_episode(episode_id)` claims its `LoadIntent` first, and a refusal changes nothing: what plays keeps playing.

### Where the bytes come from

- **A downloaded file plays from disk, with no request, whatever the switch and offline mode say.** `download_file` is a file name inside the profile's managed folder (`profile_podcast_download_dir`). It may have come out of an archive somebody made, so it is treated as hostile: only `<this episode's id>.<letters and digits>` opens anything, which leaves no separator, `..`, absolute path, drive prefix, locator or alternate data stream on any system. Anything else is no file of ours, and the episode streams.
- **Otherwise it is a request**, refused with `podcast: disabled` or `podcast: offline` before anything changes. The decoder asks both again before every request of a stream, a seek included, and the switch going off stops a stream on the spot, while a downloaded episode plays on. Offline mode turned on stops the transfer where it is.

### The stream

An enclosure goes through [`Fetcher::open`](../../src-tauri/crates/app/src/podcast/fetch.rs), never `audio::http_source`, which lets its client follow redirects and logs its address through `redact_url`: `may_open` at each hop against the feed's own origin, redirects by hand and at most five, never from https to http, and failures as one word. A `Content-Type` of `text/*` is `not_audio`. No command, no `Debug` and no event holds the address; the bar is told about the episode, not where it lives.

The decoder reads on the thread it takes its commands on, so a task pulls the body into a short queue (64 of the host's chunks) and a read waits on that. Short on purpose: pulling the file ahead would spend a listener's data on an hour they may never hear. A read that gets nothing for 30 s fails with `unreached`, and a Stop or a load queued for the decoder lets go of a read that has been waiting a second. A body that breaks off is taken up again where it stopped, three times without a byte in between: a long pause outlives a connection.

**A seek is a `Range` request**, starting from where the last one landed, so a measurement service in front of the file is not told about every jump, and falling back to the enclosure's own address when that has expired. A `206` to the very first request is the only proof a host can seek: plenty do without saying `Accept-Ranges`, and some say it and do not. A `200` or a `206` from elsewhere is refused, and what was playing goes on. A jump forward of at most 256 KiB is read through instead.

**An accurate seek into an MP3 cannot be used over a network**, because the format keeps no index: symphonia reads every frame header from the start of the file, or from where it is, to the place. Measured on a 57 MB file, resuming at 20 minutes read 19.2 MB that way, and 3 KB in one request with the coarse seek a stream sets (`ActiveStream::coarse_seek`), which lands 14 ms off on a constant bitrate and within a few seconds on a variable one. It falls back to the accurate one when it has no length or duration to estimate from. A file on disk keeps the accurate one.

### The queue

The channel's episodes, in memory, newest first as the page lists them, and only what is on disk when no request may be made. Next and Previous follow it, and media keys, the tray and MPD reach it through `player_actions`. A person goes where they asked. At the natural end of an episode the next one that **has not been heard** follows, or a channel's back catalogue would replay itself through what they already know. Play from a stopped player picks the episode under the cursor back up from its row.

### Resume

The position is written at a pause, at a seek, at the end of a session and every 15 s, from the async side and never from the audio path. The decoder reports where an episode was left (`AnalyticsMsg::EpisodeLeft`), which covers a stop, another track and the exit, whose wait on the analytics channel covers this too. A pause, a seek and the ticks are seen by a task that reads the engine's atomics once a second while a session is active and sleeps otherwise. `podcast:updated` follows a pause, a seek, an end and an episode turning played, and not the ticks, which would have the page read its list every 15 s.

An episode **counts as played past 95 %, or within its last 30 seconds**, or when it ran to its end whatever its declared length said. Its position then goes back to zero, and **opening it starts it over**. Heard again it stays played: only a person takes that off, by `podcast_mark_played` or `podcast_mark_channel_played`, which forget the position either way. A position is pinned to the profile the episode began in, and dropped when that profile is gone.

`podcast_to_listen` is one episode per channel, the one in progress (the most recently opened if several are) or else the newest unplayed, channels with one in progress first, and only what can play. It reads the two tables.

### Speed

**A speed chosen for a channel never reaches music.** `audio.playback_speed` is never written from here. The speed is a column of `podcast_channel`, `NULL` meaning the normal one and not the global: a speed chosen for music does not reach a voice either.

Whether the pitch follows the speed is the user's own `audio.keep_pitch`, on by default, so a voice at 1.5× stays the same voice: an episode goes through the same `TimeShift` as anything else. A transposition does not reach it: that is for playing along to music, and a voice has no use for one. `SharedPlayback::time_shift` reports none while an episode holds the speed, without writing anything, so `audio.pitch_semitones` stays and the next track finds it, and the menu shows none and says why, and the pipeline shows none. The speed travels with the load, and the decoder holds it for as long as that load decodes (`SharedPlayback::hold_episode_speed`, released by a guard on every way out, a panic included). So everything that plays afterwards, by whatever path, finds the user's speed in place without knowing an episode was there. A slider moved during an episode writes the channel's and is heard at once (`AudioCmd::SetEpisodeSpeed`), and the recorded load takes it too, so an output rebuild brings the episode back at it; `player:speed-changed` tells every window when the engine moved the speed by itself.

### The player

Back 15 s and forward 30 s are in the "⋯" menu, only while an episode plays or is paused. The bar and the mini-player show its title, its channel and the channel's picture from the artwork cache, through `player:radio-metadata` with `is_podcast`, so the webview makes no request for them. No lyrics are looked up for an episode: its title sent to a provider would be one more thing leaving about what someone hears.
