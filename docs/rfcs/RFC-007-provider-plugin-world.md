# RFC-007 — Third-party servers through provider plugins

- **Status**: Draft
- **Date**: 2026-10-29
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-002](RFC-002-plugin-sdk.md) (the WASM host, worlds, permissions, the registry pin), [RFC-005](RFC-005-remote-source-and-sync-v2.md) (the remote source, the parallel queue, the projection), [RFC-006](RFC-006-deduplicating-the-two-catalogues.md) (what a proven pair may change).
- **Amends**: RFC-005 [Decision 2](RFC-005-remote-source-and-sync-v2.md#decision-2--two-seams-so-sync-stays-a-capability) and [Decision 3](RFC-005-remote-source-and-sync-v2.md#decision-3--remote-identity-is-polymorphic). The `SubsonicSource` they placed in the core is withdrawn; a third-party server is reached through a plugin. Everything those decisions say about our own server stands.
- **Implementation**: none yet. The measurements below were taken against a live server on 2026-10-29.
- **Naming trap**: the server repository has its **own** RFC-007 (the library event stream) and RFC-008 (receiving a file), and code in this repository already cites them by number — `remote/upload.rs` says "RFC-008" and means the server's. Any reference to "RFC-007" must name the repository, exactly as [RFC-005 does for 003](RFC-005-remote-source-and-sync-v2.md#the-rfc-003-naming-trap).

---

## The situation this RFC answers

The settings card already recognises a third-party server. Type a Navidrome URL
and [`probe::detect`](../../src-tauri/crates/app/src/remote/probe.rs) answers,
without any credential, that this is a server speaking the Subsonic protocol —
and [`RemoteServerCard`](../../src/components/views/settings/RemoteServerCard.tsx)
then disables its only button (`probe.supports_sync === false`) and says
"no sign-in flow here yet". RFC-005 had promised the rest: a `MusicServer` seam
with a `SubsonicSource` beside `WaveflowSource`, in the core.

That promise is withdrawn here, by a decision taken on 2026-10-26: **third-party
servers — Navidrome and the other OpenSubsonic servers, Jellyfin, a DLNA NAS —
are reached through plugins that live in their own repositories**, and the core
exposes only a generic provider interface. The reasons are the ones RFC-002 gave
for plugins in general, sharpened by what a server integration is: each protocol
has its own quirks, versions and bugs to track, and every one of them in the
core is maintenance the core cannot shed. A plugin can be updated the day a
server changes, by the registry, without a release of the signed app; a protocol
nobody uses any more is one registry commit to retire.

This RFC decides what that generic interface is, where a provider's catalogue
lands, how its tracks play, what it is allowed to reach on the network, and in
which order it gets built. It ships with the 2.0 as a design; the construction
comes after.

## What this RFC does not do

- **It does not move our own server behind the plugin interface.** WaveFlow
  Desktop talks to WaveFlow Server over `/api/v2`, end to end, as RFC-005
  Decision 2 settled — journal, events feed, content hashes, tickets, upload.
  None of that is expressible through a lowest-common-denominator protocol, and
  routing our own traffic through one would forfeit it.
- **It does not deduplicate a provider's tracks against the local library.**
  [Decision 5](#decision-5--a-provider-lands-in-its-own-projection-beside-the-servers)
  explains why there is nothing to deduplicate _with_.
- **It does not let a provider's tracks into local playlists, smart playlists or
  the local queue.** They stay a separate source, as the server's do.
- **It does not design a podcast, radio or video provider.** The `source` world
  keeps serving radio; the rest is out of scope.

## Decision 1 — a new world, `waveflow:provider/v1`, not a second `source`

The `source` world ([`wit/source/plugin.wit`](../../src-tauri/crates/plugin-sdk/wit/source/plugin.wit))
already "supplies tracks the engine can play", and extending it was the obvious
first idea. It is rejected, for reasons that are about the component model as
much as about taste:

| Question                                 | Extend `source`                                                                                                                                                                                                                  | New `provider` world                                                                                                      |
| ---------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Published plugins                        | Adding a function to an **exported** interface breaks every component built against it: a component must provide every export of the world it is instantiated as. A `source@1.1.0` is not loadable by Web Radio's binary.        | Untouched. Web Radio, the only published `source` plugin, keeps `waveflow:source/v1`.                                     |
| Optional functions                       | WIT has none. Web Radio would have to stub the catalogue walk, the write-backs and the artwork — the problem [`provides`](../features/plugins.md#runtime--sandbox) had to be invented for in `metadata`, at a much larger scale. | Every export is meaningful for every provider; what a _server_ lacks is a capability.                                     |
| Shape                                    | Flat `track` list, no albums, artists, playlists, pages, delta, credential or write-back.                                                                                                                                        | Typed catalogue, pages, delta, connection, write-backs.                                                                   |
| Host lifecycle                           | Resolved on demand into a view, never stored.                                                                                                                                                                                    | Walked into SQL, refreshed by a background task, written back through a queue.                                            |
| Permissions                              | A fixed allowlist of hosts.                                                                                                                                                                                                      | The one server the user typed — [Decision 4](#decision-4--a-provider-reaches-the-server-the-user-typed-and-nothing-else). |
| What the store's world chip tells a user | "source" for both a radio directory and a server holding their password.                                                                                                                                                         | Two different things, named apart.                                                                                        |

A `source/v2` that broke v1 would be a new world in all but name, with a
misleading name. So: **a new label, `waveflow:provider/v1`, a WIT package
`waveflow:provider@1.0.0`, its own bindings**, reusing the shared
`waveflow:host/*` interfaces through bindgen `with:` exactly as `metadata`, `ui`
and `canvas` do. The SDK note that "a bump beyond `<N>.0.0` stays backwards
compatible" holds for additions to what the host **imports into** a plugin, not
for additions to what a plugin **exports**; the provider world grows by new
labels (`provider/v2`), never by new exports under the same one.

**Upward compatibility of published plugins.** Nothing that exists changes. The
new label is added to `worlds::ALL` and to `worlds.json`, from which the
registry generates its `world` enum. An older host decodes the registry's
`world` as a plain string ([`plugin_store.rs`](../../src-tauri/crates/app/src/commands/plugin_store.rs)),
so a provider entry does not break its catalogue decode; it would fail at install
on `UnknownWorld`, which is why **every provider entry carries a
`min_app_version`** at the first version whose host binds the world. The
registry stays the trusted pin: a provider's `plugin.wasm` is blake3-verified
against the registry entry before install, like any other plugin's, and it loads
from the same two roots as every plugin and from nowhere else. Holding a
password does not earn it a second distribution path.

## Decision 2 — the contract

### What a provider exports

Sketch, not final WIT — names will move, the shape will not:

```wit
package waveflow:provider@1.0.0;

interface provider {
  /// What the host hands back on every call. Owned by the host
  /// (Decision 3); the guest never stores it.
  record connection {
    base-url: string,          // as the user typed it, normalised
    username: option<string>,
    credential: option<list<u8>>, // opaque, from `sign-in`
    client-name: string,       // "WaveFlow"
  }

  /// Exchange what the user typed for what the host keeps. A server
  /// that can mint a token returns the token and the password is
  /// dropped; one that cannot returns the password itself.
  sign-in: func(base-url: string, username: string, secret: string)
    -> result<signed-in, provider-error>;

  /// Who the server is and what it can do *today*. Called after
  /// sign-in and before every walk: a server upgrade changes it.
  describe: func(c: connection) -> result<server-info, provider-error>;

  /// One page of one kind. `cursor` is opaque, returned by the previous
  /// page; `none` starts the walk. The host picks `limit`.
  catalogue-page: func(c: connection, kind: entity-kind,
                       cursor: option<string>, limit: u32)
    -> result<page, provider-error>;

  /// Has anything changed since `token`? See "Delta" below.
  changes: func(c: connection, token: option<string>)
    -> result<change-report, provider-error>;

  search: func(c: connection, query: string, limit: u32)
    -> result<search-result, provider-error>;

  /// How to fetch the audio. A description, never the bytes.
  stream: func(c: connection, track-id: string, prefs: stream-prefs)
    -> result<stream-descriptor, provider-error>;

  /// How to fetch one picture, at roughly `size` pixels.
  artwork: func(c: connection, art-ref: string, size: u32)
    -> result<fetch-descriptor, provider-error>;

  /// Write-backs — Decision 2, "What it receives".
  apply: func(c: connection, m: mutation) -> result<_, provider-error>;
}
```

The records it returns carry what the library needs and nothing it would have
to guess:

- **`track`**: `id`, `title`, `credits: list<credit>` (artist id + name, in
  order), `album-id`, `album-title`, `album-artist`, disc and track numbers,
  year, genres, `duration-ms`, `container` and `codec` hints, bitrate, sample
  rate, bit depth, channels, `size`, `art-ref`, MusicBrainz id, ISRCs, ReplayGain
  if the server knows it, and the user's server-side state: `starred-at`,
  `rating`, `play-count`.
- **`album`**, **`artist`**, **`playlist`** (with ordered track ids, owner,
  `writable`).
- **`page`**: the items, the next cursor (`none` when done), and the server's
  total when it gives one.

**Credits are a list, never a joined string.** The library's multi-artist split
is `"; "` and only that ([invariant](../architecture/invariants.md#multi-artist-queries));
a plugin that joined credits with `", "` would corrupt every artist page. The
host does the joining, so the rule lives in one place. Navidrome already answers
this way — measured, `"artists":[{"name":"Beta Wave"},{"name":"Gamma Pulse"}]`
next to a display string `"Beta Wave; Gamma Pulse"` the host ignores.

**URLs, not bytes.** `stream` and `artwork` return a `fetch-descriptor` — a URL,
the headers to attach, and what the host may assume about the answer — and the
host does the fetching. Piping audio through the guest is not an option:
`waveflow:host/http.send` buffers a whole response under a 10 MB cap, a guest
call is bounded by a 30 s epoch, and the decoder already does ranges, caching
and seeking on its own HTTP source. The guest's job is to know how to _ask_; the
host's is to fetch, safely.

### What it receives back

The user's gestures on a provider's rows go back to that server: star and
un-star, rate, a play reported, a playlist created, replaced or deleted. They go
through a per-source outbound queue, `provider_mutation`, applied to the
projection **in the same transaction** that queues them — the pattern
`remote::write` already uses, so an offline star does not look like a button
that did nothing.

Subsonic has no operation identifier, so the queue cannot replay blindly the way
RFC-005 Decision 7 does against our server. Each mutation kind is therefore
classed, in the contract and not by each plugin, as **idempotent or not**, and
the retry rule is:

- a failure **before the request left** — offline, DNS, connection refused, a
  permission refusal — retries anything;
- a failure **after it left, without an answer** — timeout, reset — retries only
  idempotent kinds;
- an answered refusal is final.

Measured on Navidrome, which is what forced the split: `star` twice is harmless,
`setRating` is absolute, but **`scrobble` with the same timestamp twice counts
two plays** (`playCount` 3 after one play, its replay and one more). And the
playlist edit the protocol offers, `updatePlaylist`, removes by **index** — a
replay removes the next song. So the contract offers no index-based edit at all:
a playlist change is `replace-playlist(id, full ordered list)`, which
`createPlaylist` with a `playlistId` implements (measured: it replaces the whole
content) and which is idempotent by construction.

Two more measured facts the host has to own rather than trust: **`setRating`
accepts 7** and stores it, so the host clamps to 0–5 before anything is queued;
and **`star` or `scrobble` on an unknown id answers `ok`**, so a write against a
track the server has since deleted is invisible to the client. That is accepted —
the next walk purges the row — but it means a queue can never use a server's
silence as proof the write landed on something.

### Identifiers

Opaque strings, indexed on `(source_id, item_id)`, never parsed — RFC-005
Decision 4, unchanged. Two providers, or a provider and our server, may emit the
same string. An identifier that has to become a filename (the artwork cache) is
hashed, never used raw.

The [one-path-segment invariant](../architecture/invariants.md#a-server-identifier-is-one-path-segment)
moves with the URL building: the **host** never formats a provider's identifier
into a URL — the guest does — so the host cannot enforce it there. What the host
enforces instead is the shape of every URL the guest hands back
([Decision 4](#decision-4--a-provider-reaches-the-server-the-user-typed-and-nothing-else)),
and the SDK gains the same `segment` and query-component helpers the remote
client uses, so a plugin written with it gets the encoding right by default.
Subsonic puts identifiers in the query, not the path; measured, an `id` of
`../x?y#z`, encoded as a query component, answers "Song not found" and moves
nothing.

### Capabilities come from the server, not the manifest

`describe` returns a capability set — `favorites`, `ratings`, `scrobble`,
`now-playing`, `playlists-write`, `search`, `server-transcoding`,
`transcode-decision`, `time-offset-seek`, `form-post` — and the interface draws
only what is there. This is not `provides`: `provides` says which functions a
_plugin_ answers, and is fixed at publication; a capability says what this
_server_, at this version, will honour, and can change with an upgrade. A Jellyfin
plugin answers `apply` for favourites on every server; a DLNA plugin answers it
with `unsupported` on every server, and says so in `describe` so the star is
never drawn.

### Errors

```wit
variant provider-error {
  auth-refused,              // the credential is wrong or revoked
  not-found,
  unsupported,
  offline,                   // never sent to the network
  transient(option<u32>),    // retry, optionally after N seconds
  server(tuple<u32, string>),// the server's own code and message
}
```

Typed because the host acts on it: `auth-refused` puts the source in the
"credential refused" state the plugin settings already render
([plugins.md](../features/plugins.md#telling-the-user-a-credential-was-refused)),
`transient` lets a walk pause instead of failing. Measured, and the reason the
mapping belongs in the plugin: **Navidrome answers every error with HTTP 200**
and a `status: "failed"` body — code 10 for a missing parameter, 40 for a wrong
credential, 70 for an unknown id — where our server answers 400 and 401. A client
that looked at status codes would read every refusal as success.

## Decision 3 — the connection belongs to the profile, the credential to the host

Plugin options live in one `.plugin-config.json` per plugin
([`plugin_config`](../../src-tauri/crates/core/src/plugin/plugin_config.rs)) —
**per app, not per profile**. A server account is per person. Two profiles on one
machine are two people who may well have two accounts on the same server, so the
connection cannot be a plugin option.

It is a row in the profile database instead — `provider_source` (id, plugin id,
base URL, username, credential, state, created/updated) — reached only through
`state.require_profile_pool()`, like everything else that belongs to a user.
The plugin's own options keep doing what they do today: preferences that are not
an account (a page size, "prefer the server's transcoder").

The credential is handled the way the existing credential store handles one —
**masking, nothing more**, which is a deliberate choice of posture, not an
oversight:

- the field is a password field; the stored value **never travels back to the
  webview** — the IPC answers `isSet`, exactly as a `sensitive = true` option
  does ([plugins.md](../features/plugins.md#credentials-sensitive--true));
- it is stored **in clear at rest**, like `.plugin-config.json` and like
  `auth_credential` — whose `token_encrypted` column, despite its name, holds the
  token's plain bytes. A keyring is a real improvement worth making, for all of
  them at once, in its own change; one table encrypted beside two that are not
  would protect nothing;
- **`sign-in` decides what is kept.** A server that exchanges a password for a
  token (Jellyfin) returns the token, and the password is never written. A
  Subsonic server needs the password itself to compute each salted token, or an
  API key where the server offers one.

The credential does reach the guest's memory — a salted token cannot be computed
without it. What contains it is Decision 4: the only place the guest can send
anything is the server the credential belongs to.

## Decision 4 — a provider reaches the server the user typed, and nothing else

Today a plugin's network is a fixed allowlist in its manifest
(`permissions.http`, glob patterns). A provider cannot work that way: the host it
must reach is whatever the user typed, typically a LAN address. So the world
adds one permission, **`connection`**, and changes nothing else:

- **The grant is the connection's origin**: scheme, host and port of
  `base-url`, under its path prefix. A request to anything else answers
  `permission denied` before a socket opens, exactly as an unlisted host does
  today. A provider's manifest may still list fixed hosts in `http` — none of the
  three planned here needs one.
- **A private or loopback address is allowed through the grant, and only
  through it.** The SSRF guard that refuses internal addresses for plugin URLs
  (`is_safe_motion_url`) stays in force everywhere else; it is the user typing
  `http://192.168.1.20:4533` that authorises that one address, for that one
  source.
- **Every URL the guest returns is checked before the host opens it.** Stream and
  artwork descriptors must be on the connection's origin and path prefix, carry
  no userinfo and no fragment, and keep any secret in the query or a header,
  never the path. Anything else is refused. This is what keeps a plugin — or a
  server answering with an absolute URL — from pointing the decoder somewhere
  the user never authorised. Measured case: Navidrome hands out
  `artistImageUrl` as an absolute URL built from the `Host` it was reached
  under, carrying a signed token in its **path**, fetchable with no credential.
  Behind a reverse proxy that host is wrong, and either way it is a capability
  URL in a place the log keeps. It is refused, and the picture comes from
  `getCoverArt` instead.
- **Redirects stay off for the guest** (the shared client already refuses them,
  so an allowlisted host cannot bounce a request elsewhere) **and become
  same-origin-only for the decoder** when it opens a provider stream — the HTTP
  source follows redirects to any host today, which is right for a radio CDN and
  wrong for a URL carrying a server credential.
- **Plain `http` is allowed**, because most home servers have no certificate, and
  the interface says, on a public address, that the credential travels in
  clear — a Subsonic salted token does not expire, so a captured one is
  replayable. Whether that should be a refusal instead is an
  [open question](#open-questions).

**Nothing opens the network implicitly.** Installing a provider does nothing.
No walk, probe or discovery runs until the user has added a connection, and the
first request goes to the URL they typed. DLNA discovery, which needs multicast,
is a host action the user presses a button for — [below](#a-dlna-nas-what-changes).

**Offline mode** is honoured twice. `waveflow:host/http.send` already answers 503
without a socket when [`offline::is_offline()`](../architecture/invariants.md#process-wide-offline-mode)
is set; on top of that the host **does not call** a provider's network-bound
exports at all while offline — no walk starts, `stream` is not asked, writes
queue. The guest is never woken to be told no.

**No secret reaches the log** ([invariant](../architecture/invariants.md#no-secret-reaches-the-log)),
in three places the existing rule does not yet cover:

- a provider URL is logged as its **origin only** — `redact_url` drops query
  and userinfo but keeps the path, and a provider's path is not ours to vouch
  for;
- a plugin's `log.emit` text goes through the walk the updater's foreign errors
  already use, `describe_error_without_urls` (every URL cut to its origin), and
  the connection's credential is masked wherever it appears verbatim;
- a provider error shown in the interface is rendered through the same path.

The wasmtime sandbox itself is unchanged: 64 MB of linear memory, a fuel budget
per call, a 30 s epoch, no filesystem, calls into one plugin serialised. Whether
the fuel budget fits a page of a few hundred tracks parsed from JSON is the first
thing step 1 measures — a 500-song page from Navidrome weighs 496 KiB.

## Decision 5 — a provider lands in its own projection, beside the server's

### Why not the `remote_*` tables

They look like the place, and they are shaped for exactly one server:
`remote_binding` is a single row (`CHECK (id = 1)`), `remote_track.remote_id` is
the primary key with no source in it, the reconciliation's `remote_track_link`
holds `remote_track_id` as a bare `UNIQUE` string, and half the columns —
`full_hash`, the events cursor, the hex-only artwork hash — describe our
protocol. Putting a source into those keys means rebuilding the tables.
`remote_playlist` is the parent of `remote_playlist_track`, so for it the rebuild
is a `DROP TABLE` of a parent ([forbidden](../architecture/invariants.md#never-drop-table-a-parent-table-in-a-migration));
for the rest it is a migration that rewrites a user's projection to obtain what a
new table gives for free.

So: **new tables, `provider_*`, keyed on `(source_id, item_id)`** —
`provider_source`, `provider_artist`, `provider_album`, `provider_track`,
`provider_track_credit`, `provider_playlist` + `provider_playlist_track`,
`provider_mutation`. The server-side user state (`starred_at`, `rating`,
`play_count`) is a column on the row it describes, because that is how every one
of these protocols returns it — inline with the catalogue, not as a journal.
Like the remote projection, all of it except `provider_mutation` is
**reconstructible**: dropping a source's rows and walking again is always a
valid recovery. And like it, nothing a provider says ever lands in `track`,
`playlist`, `liked_track` or `track.rating` — RFC-005 Decision 1, applied to a
second kind of server.

### The walk

A generic, host-driven loop calls `catalogue-page` kind by kind until the cursor
runs out. It is a long operation and behaves like one: it takes a `TaskHandle`
([invariant](../architecture/invariants.md#a-long-operation-announces-itself-to-the-task-registry)),
stops between two pages when cancelled, writes each page in one transaction, and
parks behind a library scan and retries on `SQLITE_BUSY` like every background
writer ([single writer](../architecture/invariants.md#single-writer-to-sqlite)).
Sort keys are computed with `normalize_name`, as the mirror does, so a provider's
"Björk" sorts beside a local one.

Each walk stamps a generation on what it sees; at the end, rows the walk did not
see are purged — **unless a provider playlist or a pending mutation still
references them**, the mirror's `in_catalogue` rule. A walk that stopped early
purges nothing: absence from a partial walk proves nothing.

### Delta

`changes(token)` answers one of three things:

- `unchanged` — skip the walk;
- `changed(new-token)` — walk everything and diff;
- `items(upserts, deletions, new-token)` — apply them, no walk.

The third is the goal and the first two are what OpenSubsonic allows. Measured
against Navidrome:

- **No entity carries a modification time.** Albums and songs expose `created`
  only. A retag keeps the song's identifier _and_ its `created`, so nothing in
  any listing says it changed. An album-level walk keyed on song counts (the
  mirror's trick) would miss it, as it misses corrections on our own server.
- **`lastModified` on `getIndexes` / `getArtists` is the time of the last
  scan**, and `ifModifiedSince` returns an empty index when it has not moved. A
  scan that changed nothing still moves it, so it is a safe "skip" signal and a
  noisy "walk" signal — never a false "unchanged". That is `unchanged` /
  `changed`.
- **Identifiers survive a retag and a rename** — the same song id before and
  after both, on Navidrome 0.64.2. Without that, every rename would read as a
  deletion plus an addition and orphan the song from every server playlist; with
  it, a full walk is a cheap diff.

A full walk is affordable: 1 226 songs in three pages of 500 took 54 ms on the
same machine, at about 1 KiB of JSON per song — roughly 50 MB for a 50 000-song
library, which is why `unchanged` matters and why a walk follows a `changed`
answer, never a short timer.

### In the unified library

The listings today `UNION` a local half and a remote half, tagged
`'local'` / `'remote'` (`library_sql.rs`, `library_tracks.rs`). A provider adds
a **third half**, tagged with its source, and everything RFC-005 settled for the
second applies verbatim: the source is a filter and a chip inside the list,
not a section beside it; the sort keys are comparable across halves; a local
library filter excludes it; playing a row queues the run of rows from **its own
source**; a provider row carries none of the local gestures and only the
server-side ones its capabilities allow. The filter grows from `local | remote`
to a list of sources. Each listing gains a branch — the cost is
[named below](#what-this-costs).

**Albums are grouped by the server, not by us.** The local grouping rule
(`(canonical_title, album_artist_id)`, [invariant](../architecture/invariants.md#album-grouping--canonical_title-album_artist_id))
does not apply to a provider's rows: its `album-id` is authoritative. Measured
reason to leave it alone: Navidrome split one folder, tagged identically by one
tool, into two albums by container — the M4A files in one, Ogg Vorbis and
WavPack in the other. Regrouping on our side would merge them in one client and
not in the server's own web interface, and every playlist would then disagree
with the album page.

### And RFC-006

Deduplication pairs a local track with a remote one on **content proof** —
`exact_full_hash`, or a MusicBrainz id a person confirmed — and nothing else.
Our server publishes the hash of every file; **no third-party protocol here
does**. Subsonic exposes `size` and a relative `path`, Jellyfin and DLNA no hash
either. The only way to obtain a proof would be to download every file and hash
it, which turns "connect to a server" into "copy the server".

So a provider's tracks are **never paired** in this design: they appear as their
own rows, tagged, exactly as our server's did before RFC-006. That is RFC-006's
Decision 5 applied rather than bent — a missed merge costs a duplicate line; a
wrong one costs trust. `remote_track_link` stays our server's alone. A
MusicBrainz-confirmed suggestion is the one door left open, and it is an
[open question](#open-questions), not a promise.

## Decision 6 — playback: the host opens the stream, the server transcodes

**The queue.** A provider's tracks play through the parallel in-memory queue
RFC-005 Decision 9 built for the server's, not the local `queue_item`. Its entry
gains a source discriminant — `Server` or `Provider(source_id)` — and the
advance seam asks the right party for a URL: a ticket from our server, a
`stream` descriptor from the plugin, on a blocking task in the async layer and
never near the audio callback. The descriptor is validated (Decision 4) and the
engine loads it under the advance's `LoadIntent`, like any other load.

**The URL never crosses the IPC.** Today Web Radio's flow is
`pluginStreamUrl` → webview → `playerPlayUrl`, which hands the URL to the
renderer. A Subsonic stream URL carries a non-expiring salted token in its query;
it is resolved and played inside the backend and the webview never sees it.

**The HTTP source learns three things**, each forced by a measurement or by the
protocols:

- **request headers**, so a server that authenticates by header (Jellyfin)
  never needs its token in a URL at all;
- **same-origin redirects only**, for provider streams (Decision 4);
- **refusing a body that is not audio.** Measured: Navidrome answers a failed
  `stream` — unknown id, bad credential — with **HTTP 200 and
  `Content-Type: application/json`**. A source that checks only the status opens
  that JSON as audio. The descriptor names the expected media type family, and
  the source refuses `application/json` / `text/*` before the decoder sees a
  byte.

**Seeking** depends on what the server sent, and the descriptor says which:

| What was asked                           | Measured answer                                                                                                          | Seek by                                                                      |
| ---------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| original bytes                           | `Content-Length`, `Accept-Ranges: bytes`, 206 with `Content-Range`                                                       | byte ranges, as today; the stream cache can keep it                          |
| a transcode, first time                  | chunked, `Accept-Ranges: none`, no length                                                                                | re-request with a time offset (`timeOffset`, OpenSubsonic `transcodeOffset`) |
| the same transcode again                 | the server cached it: 206 and an exact length                                                                            | byte ranges — a bonus, never relied on                                       |
| a transcode with `estimateContentLength` | a length announced up front — **larger than what is sent** the first time (236 526 bytes delivered of 241 664 announced) | not used: a seek or a progress bar computed on it points past the end        |

`timeOffset` on an original stream is **ignored** (measured: the whole file
comes back), so it is never used for raw bytes. A transcoded stream from a
server without `transcodeOffset` cannot seek at all: the descriptor says
`seek: none` and the scrubber is not drawn, rather than drawn and ignored.

**Transcoding is the server's job; the policy is ours.** The decision to
transcode is the profile's existing remote transcode preference (off by default,
RFC-005 Decision 9), applied to providers unchanged — one setting, one meaning.
The host passes it as `stream-prefs`: the preference, a bitrate ceiling, and **the
containers and codecs our decoder plays**, derived from the same registry the
scanner and playback use (`AUDIO_EXTENSIONS` and the codec registry —
[one registry](../architecture/invariants.md#one-codec-registry-for-the-whole-app)),
never a list a plugin keeps. That last part is not an optimisation: a server
holding WavPack or APE files, which the engine does not decode, must be asked to
transcode **those** whatever the preference says, or they are unplayable rows.
Measured against Navidrome:

- `maxBitRate` alone is not "lossy": it caps the file's _average_ bitrate. A
  100 kbps FLAC came back as FLAC under a 192 cap; a 734 kbps one came back as
  Opus. "Original" means `format=raw`, said explicitly.
- An unknown `format` (`wav`) falls back to Opus without an error.
- The OpenSubsonic `transcoding` extension's `getTranscodeDecision` takes our
  capability list and answers per track: `canDirectPlay` for a 24/96 FLAC, an
  Opus transcode for WavPack, with a signed `transcodeParams` for
  `getTranscodeStream`. Where it exists, the plugin uses it; where it does not,
  the plugin forces a lossy format for containers outside our list.
- A fresh Opus or AAC transcode started answering in about 50 ms on this machine.

**Caching.** The catalogue is in SQL. Pictures go to a disk cache per profile,
as the remote covers do, **only when the descriptor says the answer is
immutable** — measured: Navidrome's versioned cover id (`al-<id>_<hash>`) answers
`Cache-Control: public, max-age=31536000, immutable`, the bare album id answers
`no-cache` — and **only for an entity whose `art-ref` is set**: asked for an album
with no picture, Navidrome returns a 69 KB placeholder image with a 200, which a
cache would keep as that album's cover forever. Original streams with a length can
use the existing stream cache; transcodes cannot.

## What stays ours, what becomes generic

| Ours alone (our server, RFC-005)                                  | Generic (every source)                                                     |
| ----------------------------------------------------------------- | -------------------------------------------------------------------------- |
| PKCE sign-in, refresh rotation, device id                         | A source in the unified library: tag, chip, filter, comparable sort keys   |
| The sync journal, snapshot, ACK, socket                           | The parallel playback queue, with a source discriminant                    |
| The library events feed and its cursor                            | The disk cache for pictures, keyed per source                              |
| `full_hash`, reconciliation, `remote_track_link`, RFC-006 pairing | The HTTP source with headers, same-origin redirects, a media-type check    |
| Stream and Canvas tickets, upload, download, import               | The task registry for walks; the per-source outbound queue and retry rule  |
| Server-side transcode status (`/api/v2/transcode/status`)         | The transcode preference and the playable-format list passed to the server |

The left column is why our server is not a provider. The right column is built
once, in step 1, and the server path is moved onto it where it already does the
same thing — the queue entry and the artwork cache — so there is one of each.

## The Subsonic server the settings card already detects

What happens to `probe.supports_sync === false`:

- **The probe stays.** It is unauthenticated, protocol-generic and correct — its
  job becomes routing the user, not refusing them.
- **The disabled browser sign-in button goes.** For a Subsonic-speaking server
  the card says what it found and offers the way in: _Add as a source_ when the
  OpenSubsonic plugin is installed (opening its connection form with the URL
  already filled), _Install the OpenSubsonic plugin_ when the store has it and
  the app is online, the explanation alone when offline. The
  `remote.server.probeSubsonic` string ("no sign-in flow here yet") is rewritten
  in all 17 locales in that step.
- **`ServerFlavour::Subsonic` stays as a detection result; `RemoteIdentity::Subsonic`
  goes.** Nothing writes a `subsonic` binding today — sign-in refuses before
  any write — so the identity variant and the `username` branch of
  `remote_binding` are dead code. The Rust variant is removed in step 2; the
  table's `CHECK` keeps accepting the value, because migrations are immutable
  and a constraint that allows a value nobody writes costs nothing.

## OpenSubsonic first

OpenSubsonic first because it is the widest door — one plugin, every server that
speaks the protocol — and because the protocol was measured here rather than read.

### Call mapping

| Contract                   | OpenSubsonic call(s)                                                                                                                                                                          |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sign-in`, `describe`      | `ping` (identifies the server even when it fails), `getOpenSubsonicExtensions` (answers without auth), `getUser` (roles), `getMusicFolders`                                                   |
| `catalogue-page` tracks    | `search3` with an empty query, `songCount` / `songOffset` — measured to page the whole catalogue on Navidrome; fallback `getAlbumList2` + `getAlbum` for a server that refuses an empty query |
| `catalogue-page` albums    | `getAlbumList2` `type=alphabeticalByName`, `size` / `offset`                                                                                                                                  |
| `catalogue-page` artists   | `search3` artists **and** `getArtists`: measured, `getArtists` lists album artists only (7 of 10 here); a track-only credit ("Gamma Pulse") appears only through search                       |
| `catalogue-page` playlists | `getPlaylists`, `getPlaylist`                                                                                                                                                                 |
| `changes`                  | `getIndexes` with `ifModifiedSince` (`unchanged` / `changed` only, see Delta)                                                                                                                 |
| `search`                   | `search3` — measured prefix-matching ("lig" finds "Light", "ight" finds nothing) and accent-folding ("Ålpha" finds "Alpha")                                                                   |
| `stream`                   | `stream` (`format`, `maxBitRate`, `timeOffset`), or `getTranscodeDecision` + `getTranscodeStream` with the `transcoding` extension                                                            |
| `artwork`                  | `getCoverArt` with the entity's `coverArt` id and `size`                                                                                                                                      |
| `apply` favourite          | `star` / `unstar` (`id`, `albumId`, `artistId`)                                                                                                                                               |
| `apply` rating             | `setRating` (host clamps 0–5)                                                                                                                                                                 |
| `apply` play               | `scrobble` `submission=true` with `time`; `submission=false` for now-playing                                                                                                                  |
| `apply` playlist           | `createPlaylist` (new, or with `playlistId` to replace), `deletePlaylist` — never `updatePlaylist`'s index removal                                                                            |

Lyrics (`getLyricsBySongId`, the `songLyrics` extension) are deliberately left
out of the provider world: they belong to the `metadata` world's lyrics
waterfall. A manifest declares one world, so a repository that wants both
publishes a `metadata` plugin beside its provider.

### Authentication

In order of preference:

1. **An API key** (`apiKey`), where the server advertises the
   `apiKeyAuthentication` extension. Navidrome 0.64.2 does not: measured, an
   `apiKey` parameter is ignored and the request fails on the missing username.
2. **A salted token** — `t = md5(password + salt)`, `s = salt`, fresh per request.
   The password stays in the connection, never in a URL.
3. **Never `p=`, never `p=enc:`.** The second is hex, which is the first with an
   extra step; both put the password itself in every URL, where a proxy log keeps
   it. A server that can only verify a plain password — one backed by an external
   directory — is refused with a message rather than served this way; whether to
   allow it behind an explicit opt-in is an [open question](#open-questions).

With the `formPost` extension (measured: Navidrome accepts the credentials in a
form body), API calls carry them in the body, not the URL. Stream and cover URLs
are GETs the decoder opens and keep them in the query, which is why they never
leave the backend and are logged as an origin.

### Measured against Navidrome

**Set-up.** `deluan/navidrome:latest` — Navidrome **0.64.2** (OpenSubsonic,
protocol 1.16.1) — under Docker on Fedora 44, capped at 768 MB, the library
mounted read-only. The library was generated: sine tones written by FFmpeg, 26
tracks over FLAC 16/44.1, FLAC 24/96, MP3 320 and 128 kbps CBR, Opus in Ogg, AAC
and ALAC in M4A, Vorbis and WavPack, then 1 200 stream-copied MP3s over 120 albums
for the paging and walk figures. The container peaked at 62 MiB just after its
first scan and held 17–27 MiB afterwards, rescans included.

Advertised extensions: `transcodeOffset`, `formPost`, `songLyrics` (1, 2),
`indexBasedQueue`, `transcoding`, `playbackReport`, `topSongsByArtistId`. Not
advertised: `apiKeyAuthentication`.

**Server / version / format matrix** — Navidrome 0.64.2. "Original" is
`format=raw` with a `Range: bytes=0-99` request; "first transcode" is
`format=mp3&maxBitRate=128` or `format=opus&maxBitRate=96` on a track the server
had not transcoded before:

| Source file  | Original `Content-Type` | Original range | Reported depth / rate | First transcode                                                | Engine plays the original |
| ------------ | ----------------------- | -------------- | --------------------- | -------------------------------------------------------------- | ------------------------- |
| FLAC 16/44.1 | `audio/flac`            | 206            | 16 / 44 100           | chunked, no ranges, ~0.2 s for 20 s of audio                   | yes                       |
| FLAC 24/96   | `audio/flac`            | 206            | 24 / 96 000           | chunked; `getTranscodeDecision` answers direct play            | yes                       |
| MP3 320 CBR  | `audio/mpeg`            | 206            | 0 / 44 100            | chunked MP3 under a 128 cap                                    | yes                       |
| MP3 128 CBR  | `audio/mpeg`            | 206            | 0 / 44 100            | the original itself under a 128 cap (`Content-Length`, ranges) | yes                       |
| Opus (Ogg)   | `audio/ogg`             | 206            | 0 / 48 000            | chunked                                                        | yes                       |
| Vorbis (Ogg) | `audio/ogg`             | 206            | 0 / 44 100            | chunked                                                        | yes                       |
| AAC (M4A)    | `audio/mp4`             | 206            | 16 / 44 100           | chunked                                                        | yes                       |
| ALAC (M4A)   | `audio/mp4`             | 206            | 16 / 44 100           | chunked                                                        | yes                       |
| WavPack      | `audio/x-wavpack`       | 206            | 16 / 44 100           | chunked; `getTranscodeDecision` answers an Opus 128 transcode  | **no — must transcode**   |

Two things the matrix says that the decoder has to know: **the media type does not
name the codec** — Opus and Vorbis both arrive as `audio/ogg`, AAC and ALAC both
as `audio/mp4` — so the host keeps the catalogue's codec hint and sniffs the
ambiguous containers, the scanner's `AMBIGUOUS_CONTAINERS` rule over the network;
and **bit depth is reported as 16 for AAC**, where it means nothing (0 for MP3,
Opus and Vorbis), so it is shown only for lossless codecs.

**Other measured behaviour**, each already used above:

| Behaviour                                 | Measured                                                                                                                          |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| Errors                                    | HTTP 200, `status: "failed"`, codes 10 / 40 / 70                                                                                  |
| `getAlbumList2`, `search3` page caps      | none hit: `songCount=5000` returned all 1 226 songs; the protocol's own 500 cap for album lists was not reachable with 139 albums |
| Latency (loopback, small library)         | 1–7 ms per call; a 500-song page 16–19 ms                                                                                         |
| Full walk                                 | 1 226 songs, 3 pages, 54 ms; ~1 KiB JSON per song                                                                                 |
| Identifier stability                      | unchanged across a retag and a file rename                                                                                        |
| Change signal                             | `lastModified` = last scan time; moves on a no-op scan                                                                            |
| `star` twice, `unstar` twice              | both `ok`                                                                                                                         |
| `star` / `scrobble` an unknown id         | `ok`                                                                                                                              |
| `setRating 7`                             | accepted and stored                                                                                                               |
| `scrobble` replayed with the same `time`  | counted twice                                                                                                                     |
| `updatePlaylist` duplicate add            | allowed — the playlist holds the song twice                                                                                       |
| `createPlaylist` with `playlistId`        | replaces the whole content                                                                                                        |
| `getCoverArt` on an album with no picture | 200, a 69 KB placeholder                                                                                                          |
| `artistImageUrl`                          | absolute, path-signed, fetchable without credentials                                                                              |
| `stream` / `getCoverArt` failure          | HTTP 200, `application/json`                                                                                                      |
| `stream` response                         | sets a `nd-player-*` cookie (ignored; the host keeps no cookie jar)                                                               |

### What was not measured

- **Any other OpenSubsonic server**, and any Navidrome version but 0.64.2. The
  plugin's tests carry the recipe (container, generated library) so the matrix
  is re-run per server in step 2, and the plugin's `describe` reads capabilities
  rather than assuming Navidrome's.
- **A large real library** — real tags, real covers, tens of thousands of
  tracks, so the album-list cap of 500 and the walk's wall time on a NAS over
  Wi-Fi. The figures above are loopback figures on a fast machine.
- **DSD** (DSF/DFF) through a server, **multi-library** accounts (one music
  folder here; an unknown `musicFolderId` answers code 70), and an external
  directory behind authentication.
- **The fuel cost** of parsing a page inside the sandbox — no plugin exists yet.

## Jellyfin: what changes

Not measured — a Jellyfin server is a heavier install than this machine's
budget allowed beside the rest of the wave, and nothing here should be read as a
measurement. From its public API documentation:

- **A different protocol altogether**, REST rather than Subsonic, so a second
  plugin, not a mode of the first.
- **Authentication exchanges the password for an access token**
  (`AuthenticateByName`), sent afterwards in an `Authorization` header of the
  `MediaBrowser` scheme. `sign-in` returns the token and the password is never
  stored; the stream descriptor carries the header, so no secret is in any URL —
  the case the HTTP source's header support exists for.
- **Paging is native** (`StartIndex`, `Limit`, `TotalRecordCount` on items
  queried recursively by type), and items can be filtered by their last-saved
  date, which would make `changes` answer `items(…)` instead of `changed` — to
  be confirmed against a server before the contract relies on it.
- **Transcoding is decided by the server** through its "universal" audio
  endpoint from a container list and a bitrate ceiling — `stream-prefs` maps
  onto it directly.
- **Favourites, played state and playback reporting** exist as user-data and
  session routes; playlists too. Whether a 0–5 rating maps onto its user data
  is to be checked; if not, the capability is simply absent.

## A DLNA NAS: what changes

Not measured either. What is known from the protocol (UPnP AV) and from our own
DLNA server's side of it:

- **Discovery is SSDP multicast**, which a WASM guest cannot do — the sandbox has
  no UDP. Either the user types the device's description URL, or the host gains a
  discovery action the user presses ("Find servers"), returning description URLs
  on the local network. It never runs on its own.
- **Browsing is SOAP** (`ContentDirectory` `Browse` / `Search`), XML over HTTP,
  through `http.send` like the rest.
- **No account, no write-back.** No favourite, rating, play report or playlist
  edit: `describe` returns an empty capability set beyond browsing, and the
  interface draws none of those gestures.
- **The audio URLs (`res`) are absolute** and commonly on another port of the
  same address than the description, so the origin grant for a DLNA source is
  the device's **host** rather than host and port. Whether that width is
  acceptable is an [open question](#open-questions).
- **A change counter exists** (`SystemUpdateID`), which maps onto `unchanged` /
  `changed`.
- **Object identifiers may not survive the NAS's own rescan.** If they do not,
  a DLNA source can never hold a server playlist reference across a rescan, and
  the purge rule is all that keeps it consistent.

## Construction plan

Each step lands only when what it must prove has been shown — a test, a
measurement, or the app on a screen — not inferred.

**Step 1 — the world and the host** (this repository). WIT
`waveflow:provider/v1`, the label in `worlds::ALL` and `worlds.json`, bindings,
the `connection` permission and its origin check, the descriptor validator, the
log scrubber, the `provider_*` migrations, the walk as a task, the third half of
the unified listings, the queue's source discriminant, the HTTP source's
headers / same-origin redirects / media-type refusal, the picture cache, the
outbound queue and its retry rule — driven by an in-repository **fixture
provider** (like `canvas-fixture` and `ui-fixture`) serving a synthetic,
paginated catalogue. It must prove, in Rust tests: a request off the connection's
origin is refused before a socket opens, a redirect to another origin is refused,
a descriptor with userinfo or a path secret is refused; offline mode calls no
export; the credential never appears in the log, verbatim or inside a URL; a walk
is resumable and cancellable between pages and a partial walk purges nothing; a
referenced row survives the purge; a 200 JSON body is refused as audio; the
idempotent / non-idempotent retry split holds; and — measured, not assumed —
that a page of 500 tracks fits the sandbox's fuel and memory. The server path
moves onto the shared queue entry and picture cache in the same step.

**Step 2 — the OpenSubsonic plugin** (its own repository, registry entry with
`min_app_version`). It must prove, against a Navidrome brought up from the
plugin's own recipe: the matrix above re-measured from the plugin; in a sandboxed
app, a whole library browsed in the unified view, every format playing (WavPack
through a transcode), seeking in an original and in a transcode, star / un-star,
a rating, **one** play counted per play, a playlist replaced, a refused
credential shown as such, and offline mode producing no request — checked on the
server's own access log. The settings card routes a detected Subsonic server to
it, the `probeSubsonic` string is rewritten in 17 locales, and
`RemoteIdentity::Subsonic` is removed.

**Step 3 — Jellyfin** (its own repository). It must prove: the password is
never stored once a token is obtained; no secret appears in any URL, the stream
being authorised by header; the server's transcoding decision is followed; and
whether `changes` can answer `items(…)` — measured on a real server, or the
contract keeps `changed` for it.

**Step 4 — DLNA** (its own repository, plus the host's discovery action). It
must prove: nothing multicasts until the user asks; a `res` URL on another port
of the device is accepted and one on another address is refused; the read-only
capability set leaves no write gesture drawn; `SystemUpdateID` skips the walk
when nothing changed.

## What this costs

- **A third half in every unified listing.** The album and artist listings are
  already the expensive compound selects RFC-006 named; one more `UNION ALL`
  branch per provider source, on indexed keys, is the price of one library rather
  than tabs. Measured in step 1, against a synthetic provider of realistic size.
- **Two projections that do similar things.** `remote_*` and `provider_*` are not
  merged, and the shared pieces (queue entry, picture cache, HTTP source) are
  what keeps them from drifting. Merging the tables would mean rebuilding
  parents, which the migration rules forbid, for a gain that is cosmetic.
- **No deduplication for third-party servers.** A track held locally and on a
  Navidrome is listed twice. That is the honest consequence of having no proof,
  and it reads better than a wrong merge.
- **Plugins to maintain in other repositories.** That is the point, and also a
  cost: three repositories, three release cycles, each re-measured when its
  server changes.

## Open questions

These need a decision from the maintainer, not from an implementer:

1. **Plain `http` to a public address**: allowed with a warning (this draft), or
   refused? A home LAN server has no certificate; a public one without TLS sends
   a replayable token to anyone on the path.
2. **A play report whose answer was lost**: dropped (this draft — a missed play
   count) or replayed (a doubled one)? Subsonic cannot tell the two apart.
3. **More than one source per plugin per profile** — two Navidromes. The schema
   allows it; does the first version's interface offer it?
4. **Servers that only accept a plain password** (external directory): refused
   (this draft), or allowed behind an explicit opt-in that says what it does?
5. **A MusicBrainz-confirmed link for a provider track**, the one form of proof
   left — worth a suggestion surface, or is "listed twice" acceptable for good?
6. **Keeping a copy** of a provider's track — the download / import directions
   our server has. Out of this design; wanted later?
7. **The DLNA origin grant** at host width (any port of the device's address):
   acceptable, or should the user confirm each extra port the first time?
8. **The fuel budget per call** for the provider world — kept at the shared
   default, or raised for catalogue pages once step 1 has measured them?
