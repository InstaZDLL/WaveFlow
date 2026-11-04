# RFC-009 — Podcasts in the desktop app

- **Status**: Accepted
- **Date**: 2026-11-04
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-005](RFC-005-remote-source-and-sync-v2.md) (the remote source, for the server half)
- **Server-side context**: `waveflow-server` RFC-025 decisions 7 to 14, read at `v2.1.0`
- **Implementation**: targeted at 2.0, in the lots listed at the end

---

## The situation this RFC answers

WaveFlow plays local music, web radio and its own server's catalogue, and has no
way to follow a podcast. `waveflow-server` 2.1 has them, off by default, but a
desktop user with no server is most users, so the app needs them on its own. The
server is then one more place the same page reads from.

Most of what a podcast needs is already in the app, under another name:

| Need                                   | What exists                                                                                                                                                          |
| -------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Something that plays and is not a track | `DetachedSession`, used by [`external_playback`](../../src-tauri/crates/app/src/external_playback.rs) and `remote_playback`: negative id, no history, no statistics, no scrobble |
| An HTTP stream with seeking            | [`audio::http_source`](../../src-tauri/crates/app/src/audio/http_source.rs) and its Range reopen; to be measured on large files                                     |
| A rule for addresses out of a response | `web_radio_custom::may_open`                                                                                                                                         |
| Bytes on disk the scanner never sees   | The managed folder of [`remote/download.rs`](../../src-tauri/crates/app/src/remote/download.rs)                                                                      |
| A hostile XML reader                   | `roxmltree`, already a dependency of `waveflow-core`; the server's feed parser is built on it                                                                        |
| Timed text                             | [`waveflow_core::lyrics`](../../src-tauri/crates/core/src/lyrics.rs): one `Document` model with readers per format                                                   |

## Decision 1 — standalone first, in the app, off by default

Podcasts live in the application, not in a `source` plugin: subscriptions, resume
and downloads are more than that world was designed to carry
([RFC-007](RFC-007-provider-plugin-world.md) does not design one either).

A podcast is outbound traffic in an app that otherwise needs none, so it is
**opt-in**. Off, the sidebar has no row, no feed is fetched and no timer runs.
What was stored stays readable, and a downloaded episode stays playable. The
switch is one setting, `podcasts.enabled`, default `false`, through
`useProfileSetting` and `settingsRegistry.ts`.

**Turning it on goes through a warning the user accepts explicitly.** A dialog, with
`useModalA11y`, says in plain words what changes before anything does:

- feeds and episodes come straight from each podcast's host, so that host, and the
  measurement services episodes are often redirected through, see the user's IP
  address and what they play;
- feeds are checked every six hours while the app runs, and only then;
- downloads take disk space, up to the quota;
- offline mode stops all of it, and turning podcasts off stops it for good.

Keyboard focus starts on Cancel. **Only the dialog's confirmation writes
`podcasts.enabled`.** Nothing else turns it on: not the Settings row, which opens the
dialog, not opening an OPML file, not onboarding or What's new, which may point to it
but do not enable it. Resetting settings returns it to off. A profile imported or
restored comes back with it off, since the acceptance belongs to the person on this
machine, so the warning shows again. Turning it off asks for nothing, and says what
stays: subscriptions, positions and downloaded episodes.

**The backend enforces it.** Every `podcast_*` command that would make a request reads
the setting and refuses when it is off, and so does the refresh timer. A bug in the
interface cannot send traffic the user did not accept.

Every request checks `offline::is_offline()` first. No request is made during
startup. The six-hour interval counts from the last check, stored per profile, not
from launch, so a session shorter than six hours still gets its check: when one is
overdue it runs five minutes after launch, and only while the app runs. Subscribing fetches the feed before anything is
stored, so a typo leaves nothing behind.

## Decision 2 — an episode is not a track

An episode has no `track` row. It stays out of the library, the statistics,
Wrapped, Last.fm, smart playlists, duplicates and enrichment. It gets no crossfade
and no AutoMix, and ReplayGain does not touch it since it carries no measured
gain. It plays through a `DetachedSession` under a `LoadIntent` claimed the way
`external_playback` claims one, so the ordering rules of #622 hold.

Its queue is the channel's episodes, in memory. Next and Previous follow it, and
media keys and the tray keep going through `player_actions`.

## Decision 3 — every address is untrusted

A feed address is typed by a person, and every address after it is read out of a
response:

- **http or https only**, a host, no user information.
- **`may_open` for every hop.** A feed whose address is public can only lead to
  public addresses, so a feed on the internet cannot make the player reach the
  user's router. A feed whose typed address is on the local network may name
  local addresses, as a radio station may. As with the artwork downloader, no
  protection against DNS rebinding is attempted; that decision stands.
- **Redirects are followed by hand, at most five**, each hop checked before it is
  contacted, and never from https to http. This covers enclosures too, which
  `http_source` does not do today: it lets the client follow them, which is fine
  for a station and not for an address out of a feed.
- **Bodies are bounded while they are read**: 16 MiB for a feed, each request under
  a deadline.

## Decision 4 — a feed address never reaches a log

Private feeds carry their token in the path as often as in the query, so
`redact_url`, which masks the userinfo and the query, is not enough. A feed or
enclosure address is **not logged at all, redacted or not**. A failure is logged
and stored as one word from a fixed list (`address_refused`, `too_many_redirects`,
`too_large`, `unreached`, `http_<status>`, `invalid_feed`, `not_audio`,
`quota_exceeded`), and never as text from a response. `http_source` is reviewed
for this when episodes go through it, since it logs its URL through `redact_url`.

For the same reason, automatic backups do not carry feed addresses, under the same
rule as the network password. OPML export is a deliberate act and carries them.
The export dialog says so.

As built, the rule is wider than backups, because the path is the one the network
password takes: **a manual profile export blanks them as well**, along with the site
and picture addresses. **An import blanks them again**, whatever the archive says,
since one may come from another build or from somebody else and an address in it is
not one this person typed. What survives is a hash of the address and of the guid, so
subscribing again finds the channel with its positions. After a restore, a channel
lists as having no address until then.

**An OPML list may name public addresses only.** It is a file someone may have been
handed, not an address typed by the person it runs for, and a line naming the local
network would make the app probe it, each line's result saying what answered. A feed on
a NAS has to be typed, even from one's own export.

## Decision 5 — feeds are parsed as hostile documents

`waveflow_core::podcast` takes the server's parser, so the two stop being two: DTD
refused, a node ceiling, text bounded before it is parsed, at most 500 episodes
per feed, titles and descriptions bounded. RSS 2.0, Atom and RSS 1.0.

A title may come back empty: the parser has no locale to choose a word in, so the
page shows its own, where the server's parser writes `Untitled`.

**A refresh is conditional.** `ETag` and `Last-Modified` are stored per channel and
sent back, so a feed that has not changed costs a `304`. **A feed that has moved
stays followed**: a permanent redirect, or `itunes:new-feed-url` from the same feed,
updates the stored address, once the new address has passed decision 3, and a
temporary redirect does not.

A refresh
matches on the item's guid, or its enclosure address when it has none, so a
re-published episode is updated rather than duplicated, and one the user deleted
stays deleted.

**A description is shown as plain text.** Tags are stripped in the backend, and
nothing from a feed reaches the DOM as HTML. Channel artwork goes through the
artwork cache and is served by the asset protocol, so opening the page does not
contact a feed's host. The CSP does not change: every fetch is in the backend.

## Decision 6 — storage

Two per-profile tables in a new dated migration, `podcast_channel` and
`podcast_episode`. No existing table is touched. Position, played state and the
downloaded file are columns of the episode. A write goes through a transaction,
and a refresh parks behind a scan and retries on `SQLITE_BUSY` like any background
writer. A refresh and a download each take a `TaskHandle`, with new `TaskKind`s.

## Decision 7 — resume

The position is written at pause, at seek, at the end of a session and every 15
seconds while playing, from the async side, never from the audio callback. An
episode counts as played past 95 %, or within its last 30 seconds. Opening a played
episode starts it over. Marking played or unplayed by hand is available on each
row and for a whole channel.

**A "To listen" list at the top of the page** spans channels: one episode per
channel, the one in progress first, else the newest unplayed. A channel's unplayed
count shows on its row. Nothing here needs a request: it reads the two tables.

## Decision 8 — downloads

To a managed folder under the app's cache root, never a library folder, so
neither the scanner nor the watcher sees it. The file name is derived from the
episode's id and a suffix chosen from the first bytes; nothing a feed says names a
path. A body that is not audio is refused. Written beside its final name and
renamed, one at a time, cancellable, under a quota (default 10 GiB) and a ceiling
per episode (1 GiB). A downloaded episode plays from disk. It shows under Disk
usage, and deleting it keeps the row. Automatic downloads are out of scope here.

## Decision 9 — chapters

Read from, in this order: the feed's `podcast:chapters` JSON file, fetched under
decision 3 and bounded at 1 MiB; inline `psc:chapters`; `CHAP` frames in an MP3,
from the file when downloaded, and from the start of the stream otherwise, under
the same bound. The first source that gives at least one valid chapter wins, and
sources are not merged. A chapter is a start time and a title, sorted, with
impossible times dropped, and one the JSON marks `toc: false` is not listed. A chapter's image and link are ignored here, since each
would be another address from a feed. The JSON file is fetched when the episode is
opened, not at refresh, and cached with it. Whether the tag library exposes `CHAP`
is to be measured in the lot; if it does not, that source waits.

## Decision 10 — transcripts

From `podcast:transcript`, fetched only when the user opens the panel, bounded at 5
MiB, under decision 3, and cached with the episode. Preference: WebVTT, then SRT,
then Podcasting 2.0 JSON, then plain text or HTML with its tags stripped. The timed
ones become a `lyrics::Document`, through new `vtt`, `srt` and `transcript_json`
readers in core. The panel follows the position line by line as the lyrics panel
does, and a click seeks. It is a panel of its own, mutually exclusive with the
others through `PlayerContext`, and it reuses the line renderer, not the lyrics
providers or the editor: a transcript is not lyrics and is never looked up
elsewhere or written to a file.

## Decision 11 — the server

When a bound server answers `GET /api/v2/podcasts` with `enabled: true`, its
channels appear on the same page, marked as the server's. This needs no server
change:

- **Reads are live, not mirrored.** Episodes and channels are in neither the journal
  nor the snapshot (RFC-025), so nothing goes through the projection. The page
  reads when opened, and the section is absent offline or when the server is
  unreachable.
- **Playback** uses the stream routes, which take an episode id. Only a stored
  episode can play: the server downloads, never relays.
- **Resume** uses `PUT` and `DELETE /api/v2/bookmarks/{id}`, which accept an episode
  id, and the `position_ms` on an episode. It is the server's position, not a
  local one.
- **Managing** follows `can_manage`: subscribe, refresh, delete, and asking the
  server to download. Without it, those controls are absent. A refusal blurs to
  404 there, so the UI never infers a role from one.
- **Every id** goes through `remote::client::segment`. A feed `url` a manager sees is
  under decision 4.

A channel subscribed in both places shows twice, once per source, and each keeps
its own position. Merging them would need an identity the two do not share, and
that is left open.

## Decision 12 — speed

**A speed is remembered per channel and never leaks into music.** The global
`audio.playback_speed` is left as it is: an episode applies its channel's speed when
it starts, and anything else that plays afterwards gets the global speed back. The
speed is a column of `podcast_channel`.

Pitch is not preserved today: 1.5× lifts a voice by about seven semitones. Time-stretching lands as its own lot on the existing
board card, on the decoder thread and never in the callback. The library is chosen
by measurement. Podcasts do not wait for it.

## The surface

A **Podcasts** row in the sidebar, shown when enabled, and a view declared once and
styled per skin, branching on `skin.layout`, never on a skin id. Lists are virtual
through `usePageScroll()`. Modals call `useModalA11y`. Skip back 15 s and forward
30 s appear in the player bar's overflow menu first, as any new action does. Strings
go to all 17 locales. A page in the What's new panel introduces it.

Commands are prefixed `podcast_`, with `podcast:updated` and `podcast:download`
events.

## Lots

| Lot | Scope                                                                 | Depends on |
| --- | --------------------------------------------------------------------- | ---------- |
| 1   | `waveflow_core::podcast`: feed parser, OPML, chapter and transcript readers, tests on hostile input | —          |
| 2   | Migration, fetch rules, subscribe, refresh, OPML import and export, commands, setting | 1          |
| 3   | Playback: session, stream with hand-followed redirects, resume, played state | 2          |
| 4   | Downloads: managed folder, quota, cancel, Disk usage                  | 2          |
| 5   | The page, the activation warning, sidebar, settings, 17 locales, What's new, documentation | 2          |
| 6   | Chapters and the transcript panel                                     | 3, 5       |
| 7   | The server's podcasts                                                 | 3, 5       |
| 8   | Speed without changing pitch                                          | none       |

## Out of scope

Searching a directory, which brings in a third-party service and is the first thing
to add next. Automatic downloads by subscription. Video enclosures. Merging a
channel subscribed both locally and on the server. Publishing or editing a feed.

## Tests

- Parsers on hostile input: DTD, entity expansion, oversized text, unknown encodings,
  impossible dates, duplicate guids.
- The fetcher against a local server: a public-to-private redirect refused before it
  is contacted, https to http refused, a lying `Content-Length`, a body that never
  ends, HTML in place of audio.
- A test that a private feed's token, in path and query, reaches no log line.
- With the switch off, and in offline mode, no request is made, whichever command is called.
- Only the warning's confirmation turns the switch on, and an imported profile comes back off.
- Database tests on the really migrated schema with foreign keys on.
- A played episode leaves `play_event` and the scrobble queue empty.
- Against real public feeds in a sandbox, muted, and heard by a person before a PR.

macOS cannot be verified while no job builds it and the machine is off; each PR says so.
