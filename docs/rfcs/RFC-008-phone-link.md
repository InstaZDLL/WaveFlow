# RFC-008 — Phone link: control, playback hand-over and library sync

- **Status**: Draft
- **Date**: 2026-10-29
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-005](RFC-005-remote-source-and-sync-v2.md) (the server path, opaque remote identifiers), [RFC-006](RFC-006-deduplicating-the-two-catalogues.md) (content proof is the only proof).
- **Builds on released versions**: WaveFlow Server **v2.0.0** — every server file and route cited below is read at the tag `v2.0.0` (`dd48a1d`), not a working branch — and QuickPlay **v1.2.0**, whose _Continue in WaveFlow_ ships the `core::handoff` relay in production. The server path below is therefore not hypothetical: it is what a user can install today.
- **Server-side context**: `waveflow-server` RFC-002 (credentials and scopes), RFC-003 (sync v2), RFC-004 (local/server reconciliation) and **its own RFC-008, _Receiving a file_ — a different document with the same number**, see [the naming trap](#the-rfc-008-naming-trap).
- **Implementation**: none. Decided on 2026-10-24: the specification is written during 2.0, the construction happens in 2.1, in the order of [the build plan](#build-plan-for-21).

---

## The situation this RFC answers

A phone and a desktop that belong to the same person know nothing of each
other. Each piece the feature needs exists somewhere, in a shape that does not
reach the other device:

- **The desktop can already hand playback over** — to itself.
  [`waveflow_core::handoff`](../../src-tauri/crates/core/src/handoff.rs) is the
  contract QuickPlay v1.2.0 writes and WaveFlow reads, in production: a versioned
  `Handoff { version, paths, index, position_ms, playing }`, carried in a file
  on the same machine. [`external_playback::take_over`](../../src-tauri/crates/app/src/external_playback.rs)
  keeps the current track's place, drops what is gone, and settles the seek and
  the pause before the first packet. QuickPlay offers the hand-over only when
  the installed WaveFlow is 2.0.0-beta.1 or later, because an older one ignores
  `--handoff` and opens empty (QuickPlay's `docs/ARCHITECTURE.md`). Every entry
  is an absolute path, which means nothing on another device.
- **The desktop can already be controlled from the LAN** — through MPD
  ([`docs/features/mpd.md`](../features/mpd.md)): opt-in, off by default, bound
  to `0.0.0.0`, a cleartext password that its own page calls "a nuisance
  filter, not a security boundary", and responses that expose file paths. It is
  the right surface for existing MPD clients. It cannot hand anything over or
  sync anything.
- **The server already syncs user data**, for server tracks only — as released
  in v2.0.0. Its journal
  (`GET /api/v2/sync/snapshot`, `/sync/changes`, `/sync/ack`, the wake-up
  `/sync/socket`) carries playlists, favourites, ratings, scrobbles, the queue,
  shares and bookmarks, ordered by one cursor, last write wins
  (`waveflow-server/src/api/sync/mod.rs`, migrations
  `20260809000000_sync_journal.sql` and, for bookmarks,
  `20260818150000_sync_event_bookmark.sql`). `GET`/`PUT /api/v2/queue` stores
  `current`, `position_ms`, `changed_by` and the songs: one row per user
  (`waveflow-server/src/services/playback.rs`). The desktop consumes all of it
  through `crate::remote`.
- **The server already has devices and hashed tokens**, in v2.0.0 as on its
  main branch. Opaque `wfa_` / `wfr_`
  tokens of 32 random bytes, stored as SHA-256 only
  (`waveflow-server/src/auth/security/mod.rs`), one `session` per `device`,
  each with `revoked_at` (`migrations-v2/20260802000000_foundations.sql`). No
  route lists the devices of an account or revokes one, and nothing writes
  `device.revoked_at` or `device.last_seen_at` after the insert
  (`src/storage/database/sessions.rs`).
- **The Android app plays local and server music and syncs nothing.** It signs
  in with a username and a password (`POST /api/v2/auth/login`, no PKCE), keeps
  its tokens in a plain Preferences DataStore — deliberately, the reasoning is
  in `SessionStore.kt` — reads MediaStore without writing to it, keeps playlists
  and a play history in Room, and caches server streams in a 200 MB Media3
  `SimpleCache` (`playback/RemoteMediaCache.kt`). Its play history is **one row
  per media id with a count and a last-played time**
  (`data/local/PlayHistoryEntity.kt`), not a log of events. Its README lists
  server user-data sync as not done. It has no NSD, no WebSocket client and no
  custom media-session command.

The work is therefore not to invent a protocol from nothing. It is to decide
which of these pieces carries what, what has to be added between them, and what
must stay impossible.

## The RFC-008 naming trap

The server repository numbers its RFCs independently, and its RFC-008 is
_Receiving a file_ — the hash-first upload negotiation into a library that opted
in (`/api/v2/libraries/{id}/uploads`, chunks, commit). This document is the
desktop's RFC-008 and is unrelated, except that [Decision 8](#decision-8--a-file-crosses-only-when-someone-asked-for-it)
reuses that negotiation's shape. Anything citing "RFC-008" across the two
repositories names the repository.

## What the phone link is not

**It is not a second sync protocol for server tracks.** When both devices are
bound to the same WaveFlow Server account, everything about server tracks
already travels through the journal. The link never carries a favourite, a
rating or a play of a server track, so there is exactly one path for each piece
of data and no two of them can disagree.

**It is not a way to merge two catalogues.** A phone's files and a desktop's
files stay two libraries. What the link establishes between them is a set of
**proven pairs**, under the same rule RFC-005 and RFC-006 set for the server:
identical bytes, or a copy whose provenance was recorded when it was made.
Nothing is matched by title, artist or duration.

**It is not always on.** Nothing in this design opens a port, joins a multicast
group or advertises anything until the user turns the link on, and nothing
crosses the network that the user did not start.

## Decision 1 — two transports, one vocabulary

There are two ways the devices can reach each other, and each is good at a
different half of the problem.

| Need                                  | Same server account                                         | Direct on the LAN                                                                               |
| ------------------------------------- | ----------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| User data about **server** tracks     | the journal, as today                                       | never — see above                                                                               |
| User data about **local-only** tracks | not carried (the server has no row for them)                | sync session with preview, [Decision 7](#decision-7--sync-is-a-reviewed-three-way-merge)        |
| Hand-over                             | park the queue on `PUT /api/v2/queue`, pick it up elsewhere | live offer and acceptance, [Decision 5](#decision-5--hand-over-extends-the-handoff-contract)    |
| Real-time remote control              | **not in 2.1** — the socket carries cursors only            | WebSocket on the link, [Decision 4](#decision-4--control-goes-through-player_actions-both-ways) |
| Copying files                         | upload to the server (server RFC-008), stream or cache      | consented transfer, [Decision 8](#decision-8--a-file-crosses-only-when-someone-asked-for-it)    |

Both transports speak one vocabulary, defined once in a new
`waveflow_core::link` module — the same reason `handoff` lives in the core:
"so the sender and the receiver cannot drift apart". The Android app does not
link the core; it implements the same JSON shapes, and the core module's tests
pin them with fixture files the Android repository copies.

**Real-time control across networks is out of scope.** It would need a relay on
the server, which today only sends `{"cursor": n}` wake-ups. That is a server
RFC of its own ([open question 7](#open-questions)).

## Decision 2 — the desktop hosts, the phone connects, and only when asked

**The desktop is the only side that listens.** Android stops background sockets
under Doze and app-standby, a phone changes networks as its owner walks, and the
desktop already hosts two LAN services. The phone always opens the connection;
once it is open, messages flow both ways on it.

**Off by default, opt-in, process-wide.** A Settings card, _Phone link_, beside
DLNA and MPD, stored in `app_setting` like them (`link.enabled`, `link.port`).
Turning it off closes every connection and withdraws the advertisement.
Offline mode does not stop it: today neither DLNA nor MPD consult
`offline::is_offline()`, which governs outbound calls to internet services, and
the link is neither.

**Bound to every interface, refusing public peers.** `0.0.0.0` for the reason
MPD's page gives — on loopback the feature loses its point — with two additions
neither existing service has: a connection whose source address is not private
(RFC 1918, link-local, IPv6 ULA / link-local, loopback) is closed before the
TLS handshake, and the desktop never asks a router for a port mapping.

**Encrypted, always.** The pairing token is a bearer credential, and a café
network is exactly where MPD's cleartext password is the wrong answer. The
listener speaks TLS with a self-signed certificate generated the first time the
link is turned on (`rustls` is already in the dependency tree through
`hyper-rustls`; `rcgen` would be new). Its SHA-256 fingerprint is what the
phone pins at pairing ([Decision 3](#decision-3--pairing-is-a-one-time-code-confirmed-on-the-desktop)),
so no certificate authority is involved and none is needed.

**Discovery is a convenience, not a requirement.** While the link is on, the
desktop advertises `_waveflow-link._tcp` over mDNS with a TXT record holding a
random instance id (stable across launches) and the protocol version — no
profile name, no machine name beyond the advertised host, nothing about the
library. The phone uses Android's `NsdManager` to find a desktop it is already
paired with after an address change, and to list candidates during a pairing
by code. A pairing by QR code does not need discovery at all: the address is in
the code. (RFC-001 §6.8 decided mDNS for the server; the server has not built
it, so the link's advertiser is the project's first.)

**The pairing belongs to a profile.** User data is per profile, so a phone is
paired with the profile that was active when it was paired. Pairings live in
`app.db` (`phone_link_device`, with a `profile_id`) because the listener has to
check a token whatever profile is open; a request from a phone whose profile is
not the active one is refused with `profile_inactive` rather than switching
profiles from the phone ([open question 1](#open-questions)).

## Decision 3 — pairing is a one-time code, confirmed on the desktop

1. On the desktop, _Pair a phone_ opens a pairing window for **120 seconds**. It
   shows a QR code and, under it, an 8-character code drawn from the alphabet
   [`generatePassword`](../../src/lib/password.ts) already uses (no look-alike
   characters).
2. The QR code holds `waveflow-link://<host>:<port>?id=<instance>&fp=<sha256>&c=<secret>`:
   the address, the certificate fingerprint, and a 256-bit secret. `<host>` is
   an IPv4 address, or an IPv6 one in brackets (`[fd12::3]:7700`), as a URI
   authority writes it; every query value is percent-encoded. A link-local
   IPv6 address is never put in the code: its zone id names an interface of
   the desktop and means nothing on the phone, which finds such a desktop
   through NSD instead. When the desktop has several private addresses, the
   code carries the preferred one, ranked as DLNA ranks them
   ([which address](../features/dlna.md#which-address-is-advertised)). The typed
   code is the fallback when there is no camera: the phone lists the desktops
   NSD finds, the user picks one and types the code.
3. The phone connects, checks the certificate against `fp` (QR path), presents
   the secret or the code, and sends its name (`Build.MODEL`, editable).
4. **The desktop asks** — _Allow "Pixel 8" to control WaveFlow?_ — and only an
   acceptance there completes the pairing. On the typed-code path, which carries
   no fingerprint, both screens also show a 6-digit number derived from the
   certificate fingerprint and the phone's key share, and the user accepts only
   if they match: the code proves who typed it, the number proves no one sat in
   between.
5. The desktop mints a token — `wfl_` and 32 random bytes, the server's
   `generate_token` shape — returns it **once**, and stores only its SHA-256.
   SHA-256 and not Argon2 for the server's own reason: the token is 256 bits of
   randomness, so a slow hash buys nothing.

Three rules come from what the server measured about its own codes (RFC-005
Decision 5):

> **A pairing secret is spent on first presentation, whatever the outcome.** A
> wrong code closes the attempt; five wrong codes close the window. A retry
> starts a new window, on the desktop, by the person sitting there.

> **The token never appears in a URL.** It travels as a Bearer header inside
> TLS, and anything logged goes through `redact_url` as the
> [invariant](../architecture/invariants.md#no-secret-reaches-the-log) requires.

> **Revocation is immediate.** Settings lists each paired phone — name, profile,
> paired on, last seen — with _Revoke_, which sets `revoked_at` and closes that
> phone's open connection in the same step. _Forget all phones_ revokes them
> all. Turning the link off revokes nothing.

There is no refresh rotation on the link. The server rotates because its tokens
cross the internet and live for 30 days; a LAN token pinned to one certificate
and revocable from the desk it was issued at does not need the rotation race
RFC-005 had to single-flight. Whether an unused pairing should expire is
[open question 4](#open-questions).

On the phone, the token is kept the way `SessionStore` keeps the server session
— a Preferences DataStore excluded from backups — for the reason that file
gives: the only androidx library that would encrypt it never left alpha. A
rooted phone yields the token; the answer is the desktop's _Revoke_.

**What a pairing may do is chosen at pairing time.** Three grants, shown as
checkboxes on the acceptance dialog: _control and hand-over_ (on by default),
_sync library data_, _copy files_. They are stored on the pairing and checked
where the request is resolved, so a route cannot be added without saying which
grant it needs — the server's scope model, without its vocabulary.

## Decision 4 — control goes through `player_actions`, both ways

Once connected, the phone holds one WebSocket on the TLS link. Commands are
small JSON messages — `play`, `pause`, `toggle`, `next`, `previous`, `seek`,
`set_volume`, `like_current`, `play_at_index`, `play_tracks`, `enqueue` — and
the desktop executes them through
[`player_actions`](../../src-tauri/crates/app/src/player_actions.rs), like the
tray, the media keys and MPD
([invariant](../architecture/invariants.md#non-frontend-control-surfaces-go-through-player_actions)).
A command that loads a track claims its `LoadIntent` when it starts, so a phone
tapping _next_ and a user clicking a row resolve by the same ordering rules as
any two local gestures
([invariant](../architecture/invariants.md#every-load-command-carries-its-intent)).

**State is pushed as anchors, not ticks.** The frontend reads the position at
4 Hz; the phone does not. The desktop sends a state message on every change —
track, queue, volume, playing or paused — and a position anchor
`{ position_ms, at, rate }` on play, pause, seek and track change. The phone
extrapolates between anchors. Nothing is sent while nothing changes.

**No file path crosses the link.** MPD's page lists what its responses expose,
and paths are on that list. Tracks are referenced by an id the desktop issues
(`ltk_<opaque>`), stable for as long as the track exists so that a pair or a
playlist entry recorded with it still means the same track at the next
connection — opaque so it says nothing about the library, and never a path. The
phone does the same with its own tracks (it already names them
`local:<MediaStore _ID>` internally). The phone browses through
redacted reads: search, albums, artists, playlists, the queue — titles, credits,
durations, an artwork URL on the link. Artwork is served by a `GET` on the same
listener, behind the same token.

**The phone shows the desktop as a player in its own system controls.** The
Android app exposes the connected desktop as a second Media3 session whose
`Player` forwards to the link, so the lock screen and a watch control the
desktop the way they control the phone. Which foreground-service type Android
accepts for controlling a player that plays elsewhere is
[a point to verify](#decision-9--android-permissions-asked-for-at-the-moment-they-are-needed)
before building it.

**The desktop can control the phone, while the phone keeps the link open.** The
same socket carries commands the other way, and the phone executes them through
its `PlaybackService`. In practice that is while the phone's media session is
alive; when Android has stopped the app, the phone is not shown as reachable.
Whether this half belongs in 2.1 is [open question 9](#open-questions).

## Decision 5 — hand-over extends the handoff contract

The semantics of `core::handoff` are right and are kept: the queue, the current
entry, the position, playing or paused; the position honoured only when the
current track is still the one it belongs to; a paused hand-over stays silent; a
newer format than the reader knows is refused rather than played wrong. What
cannot be kept is the path.

So `core::handoff` gains a sibling, not a version bump of the file format
QuickPlay writes:

```rust
pub struct LinkHandoff {
    pub version: u32,
    pub tracks: Vec<TrackRef>,   // in play order
    pub index: usize,
    pub position_ms: u64,
    pub playing: bool,
    pub origin: String,          // the sending device's pairing id, opaque
}

pub struct TrackRef {
    pub full_hash: Option<String>, // whole-file BLAKE3, hex — Decision 6
    pub size: Option<u64>,
    pub server_track: Option<String>, // opaque server id, RFC-005 Decision 4
    pub peer_id: Option<String>,      // the sender's stable id for it (`ltk_…`)
    pub title: String,                // display only, never for matching
    pub artist: String,
    pub duration_ms: u64,
}
```

The file-based `Handoff` stays version 1: QuickPlay v1.2.0 is built against it
and installed on users' machines, so changing its format would break a shipped
product to make room for one that does not exist yet. `LinkHandoff` shares its
limits (`MAX_PATHS` becomes the cap on `tracks`) and its refusal rule, and the
resolution rules live next to `handed_over_queue` so both receivers drop missing
entries the same way.

**Resolving a `TrackRef`, strongest first.** A local track proven identical
([Decision 6](#decision-6--identity-is-proven-content-or-recorded-provenance));
the server track, when both devices are bound to the same account; and only
desktop → phone, a stream of the desktop's file over the link, with range
requests, for as long as the phone stays connected. An entry that resolves to
none of these is dropped and counted — "3 tracks could not be handed over" —
and the position follows the take-over rule that already exists: kept only if
the current entry survived.

**An older peer refuses, it never ignores.** QuickPlay has to read the installed
WaveFlow's version before offering a hand-over, because the older ones drop
`--handoff` and open empty. The link does not repeat that: the first message on
a connection carries each side's protocol version, and a message type a peer
does not know is answered `unsupported` rather than skipped. A hand-over offered
to a desktop that cannot take it fails on the sender's screen, with the music
still playing there.

**Two phases, so nothing plays twice or not at all.** The sender offers; the
receiver resolves, starts, and acknowledges; only then does the sender pause.
An offer not acknowledged within 10 seconds is withdrawn and the sender keeps
playing. On the desktop the take-over runs `external_playback::take_over`'s
sequence under the hand-over's own intent, so a click on the desktop during the
offer wins, as it does today.

**Through the server, hand-over is a parked queue.** When no link is up, _Hand
over_ on the sending device writes `PUT /api/v2/queue` with server track ids,
`current` and `position_ms`; the other device offers _Continue from Pixel 8_
when it next reads the queue. The server stores no playing flag, so that
hand-over always arrives paused, and it carries server tracks only. The server
side exists and needs no change. The desktop has both halves already — the
`SaveQueue` mutation drained to `PUT /api/v2/queue`, and the journal's queue in
its projection — but, as RFC-005 notes, nothing drives them from the player yet;
wiring _Hand over_ to them is desktop work. The Android app, which already holds
a server session for streaming, adds a client for the `/api/v2/queue` routes. Both are in [stage 4](#build-plan-for-21), apart from the sync
v2 client of stage 8.

## Decision 6 — identity is proven content, or recorded provenance

**`track.file_hash` cannot be the identity.** Two code paths write that column
with two incompatible algorithms and nothing records which one did:
[`scanner::hash_file`](../../src-tauri/crates/core/src/scanner/extract/mod.rs)
hashes the length, the first MiB and the last MiB of any file over 2 MiB;
[`rehash_track_file`](../../src-tauri/crates/app/src/commands/edit/update.rs)
hashes the whole file with no length prefix after a tag, cover or rating write,
and the lyrics writer does the same through its own `hash_file_blake3`. A phone
cannot reproduce a value whose algorithm is unknown.

**The identity is the server's: whole-file BLAKE3, hex, with the byte size.**
It is what `full_hash` already means on the server (an unkeyed BLAKE3 over the
whole file, `services/model/catalog.rs`), what the desktop's reconciliation
computes, and what server RFC-004 links on. The desktop gets it through
[`remote::hashing`](../../src-tauri/crates/app/src/remote/hashing.rs), whose
module comment already separates discovery (cached in `local_full_hash`,
invalidated by `(file_size, file_modified)`) from verification (read the bytes
now). The link uses the same two paths for the same two purposes. The Android
app needs the same cache keyed on MediaStore `_ID`, `SIZE` and `DATE_MODIFIED`.

**Hashing is paid for what is used, not up front.** A hand-over hashes its
queue, a playlist sync hashes that playlist's tracks. Only a library-wide sync
preview needs every file, and it runs as an announced task — a `TaskHandle` on
the desktop
([invariant](../architecture/invariants.md#a-long-operation-announces-itself-to-the-task-registry)),
a WorkManager job with progress on the phone.

**The rule for a pair is RFC-005's, word for word.** Automatic only on an exact,
unique `(size, full_hash)` match on both sides; a MusicBrainz recording id or an
ISRC raises a candidate a person confirms; title, artist and duration never
establish anything ([RFC-006 Decision 1](RFC-006-deduplicating-the-two-catalogues.md#decision-1--metadata-may-refuse-an-identity-never-establish-one)).

**A copy made by the link is a pair by construction.** Phones often hold
re-encoded copies, which no hash will ever match. When [Decision 8](#decision-8--a-file-crosses-only-when-someone-asked-for-it)
produces a file — the original or a transcode — the desktop records the pair at
that moment, with the source `copied`, the way `remote/import.rs` links an
imported file to its server track. The pair records the bytes that actually
crossed: a transcode is produced before it is announced, and its own
`(size, full_hash)` — not its source's — is what the transfer announces, what
the phone verifies on arrival and what the pair stores, next to the source
track it was made from. A transcode is therefore identified for as long as
neither side rewrites it.

Pairs live in each profile's `data.db`, in a table shaped like
`remote_track_link`: the paired device, the local track id, the peer's opaque
id, the peer's `(size, full_hash)`, a status (`confirmed` / `stale`) and a source
(`exact` / `copied` / `confirmed_by_user`). A pair whose local file changed is
`stale` until verified again; a stale pair resolves nothing. A peer id can move
without the bytes moving — Android may reindex MediaStore and hand out new
`_ID`s — so a pair whose peer id no longer answers is not deleted but looked up
again by its `(size, full_hash)`, and re-pointed when exactly one track matches.

## Decision 7 — sync is a reviewed three-way merge

Without the server, there is no journal and no cursor, and RFC-005 explains why
the project dropped logical clocks. The link uses the oldest tool that needs
neither: a **baseline per paired device** — the state both sides agreed on at
the end of the last sync — and a three-way merge against it. A change made on one
side only is taken; the same change on both sides is no change; two different
changes are a conflict, and a conflict is shown, never resolved silently.

**Every sync starts with a preview.** The desktop computes it and shows it on
the desktop, grouped by kind and direction: what comes in, what goes out, what
conflicts, and what cannot be carried because the track has no pair on the
other side. The user applies all of it, one kind, or single entries. Whether a
later sync with no conflict may apply without asking is
[open question 5](#open-questions).

**Applying is two local commits and a durable plan, not a distributed
transaction.** Two devices cannot commit together, so the protocol is ordered
for the case where only one of them did:

1. The desktop writes the accepted plan to its database under a new `sync_id`:
   every entry as a **target state** — "liked", "rating 4", "this playlist's
   entries are …", "history event `<origin>/<id>` exists" — never as a delta,
   so applying it twice changes nothing the second time.
2. The phone applies its half and, **in the same transaction**, stores the new
   baseline and `sync_id` as the last one it applied; then it acknowledges.
3. The desktop applies its half and, in the same transaction, stores the same
   baseline and marks the plan applied.

The baseline on each side therefore moves only with that side's data. If the
connection or a device dies in between, the next connection starts by comparing
the last `sync_id` each side applied: a plan the phone applied and the desktop
did not is finished by the desktop from its stored copy, which target states
make safe to replay; a plan the phone never applied is discarded, and a fresh
preview is computed. No baseline is ever advanced on the strength of the other
side's acknowledgement alone.

What each kind does:

- **Likes.** Per pair, liked or not. `liked_track` deletes its row on unlike,
  so there is no tombstone to read; the baseline is what tells an unlike on one
  side from a like that never reached the other.
- **Ratings.** Per pair; both sides changed to different values is a conflict.
- **Playlists.** Matched by a link-issued id stored on both sides after their
  first sync, never by name. Additions and removals merge per entry; a reorder
  on both sides is a conflict for that playlist. An entry whose track has no
  pair on the receiving side arrives as a **placeholder** — the mechanism
  playlist import already has (migration `20261027120000_playlist_placeholder.sql`)
  — so it is visible, not dropped, and resolves if a pair appears later.
- **History.** Append-only, no conflicts. Each event carries its origin device
  and its origin id, so applying twice imports nothing twice. An event whose
  track has no pair arrives orphaned, with its snapshot fields — the shape
  `play_event` already uses for history that outlived its track
  ([storage](../architecture/storage.md#listening-history-outlives-its-tracks)) —
  and `reattach_orphaned_play_events` may reattach it **by hash only**: the
  artist-and-title step that exists for local orphans would be a metadata match
  across devices. Imported plays count in statistics and are never scrobbled
  again ([open question 8](#open-questions)).

**The phone has no history to send yet.** Its `play_history` table is an
aggregate — a count and a last-played time per media id — and an aggregate
cannot honestly become events. The Android app starts logging events when this
lands; plays from before that are not converted.

**Server tracks are excluded on both sides.** When both devices are bound to the
same account, a like, a rating, a playlist entry or a play that concerns a
server track is the journal's, and the preview does not list it.

## Decision 8 — a file crosses only when someone asked for it

Copies are never automatic and never part of a sync. They start from an explicit
gesture — _Copy to phone_ on an album, a playlist, a selection; _Copy to the
desktop_ on the phone — and the dialog shows the count, the total size and the
free space on the receiving side before anything moves.

The transfer reuses the shape of the server's own RFC-008 rather than inventing
one: the sender announces `(size, full_hash)` for every file first, the receiver
answers with what it already holds, and only the rest crosses, in resumable
chunks, each file verified against its hash on arrival. A file that fails
verification is discarded, not kept half-written.

- **Phone → desktop.** Files land in the managed `Music/WaveFlow` folder through
  the same path a drop on the window takes, which keeps library folders from
  overlapping
  ([invariant](../architecture/invariants.md#library-folders-never-overlap)),
  become local tracks on the next scan of that folder, and are recorded as
  `copied` pairs.
- **Desktop → phone.** The original file, or a transcode the user chose in the
  dialog, produced and hashed before the announcement so that what is announced
  is what crosses ([open question 6](#open-questions) decides whether 2.1 offers
  transcodes at all). On Android 10 and later the app
  inserts its own files into MediaStore without a storage permission; where
  they land is [open question 2](#open-questions).
- **Through the server.** Already possible and unchanged: the desktop offers the
  server what it lacks (`remote/upload.rs`), and the phone streams or caches it.

## Decision 9 — Android permissions, asked for at the moment they are needed

The app is `minSdk 26`, `targetSdk 36` and declares today `READ_MEDIA_AUDIO`,
`READ_EXTERNAL_STORAGE` (max SDK 32), `FOREGROUND_SERVICE`,
`FOREGROUND_SERVICE_MEDIA_PLAYBACK` and `INTERNET`. Nothing below is requested
at install or at first launch; each is asked for when the user turns on the
part that needs it, with the reason on screen, and the feature degrades rather
than nags when it is refused.

| Permission                            | Why                                                                                                            | When it is asked                                                  | Without it                                                                          |
| ------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `ACCESS_NETWORK_STATE` (normal)       | know whether the phone is on Wi-Fi before trying the LAN                                                       | declared, no prompt                                               | —                                                                                   |
| Local-network access                  | reach a private address and use NSD; Android is making this a runtime permission for apps targeting newer SDKs | when the link is turned on                                        | pairing by QR still works where the platform does not enforce it; otherwise no link |
| `POST_NOTIFICATIONS` (API 33+)        | a hand-over offered by the desktop while the app is in the background                                          | when _Receive hand-overs_ is turned on                            | offers are shown only while the app is open                                         |
| `READ_MEDIA_AUDIO` (already declared) | hash local files for pairs                                                                                     | already granted for the library                                   | no pairs, so no sync and no local resolution                                        |
| `WRITE_EXTERNAL_STORAGE`, max SDK 28  | write copies on Android 8 and 9, where MediaStore inserts need it                                              | at the first copy on those versions                               | no copies to the phone on Android 8 and 9                                           |
| Camera                                | scan the pairing QR code                                                                                       | only if the scanner needs it ([open question 3](#open-questions)) | the typed code                                                                      |

Two points are to be verified against the platform when the Android half is
built, not assumed here: the exact name and SDK threshold of the local-network
permission, and which foreground-service type a session that controls a remote
player may declare (`mediaPlayback` describes playing media; `connectedDevice`
describes talking to another device).

## Security summary

- Nothing listens, advertises or connects until the user turns the link on.
- The listener refuses public source addresses and never maps a router port.
- Every connection is TLS, pinned to a certificate fingerprint learned at pairing.
- Pairing needs a one-time secret **and** an acceptance on the desktop.
- Tokens are 256-bit, shown once, stored as SHA-256, never in a URL or a log,
  revocable one by one with immediate effect.
- Each pairing carries its grants; control is the default, sync and copies are
  granted explicitly.
- No file path crosses the link; tracks are opaque ids for the connection.
- MPD and DLNA are unchanged and remain separate, opt-in surfaces for
  third-party clients. The link replaces neither.

## Build plan for 2.1

Each stage ships on its own and is useful on its own; later stages do not
change what earlier ones decided.

1. **The contract.** `waveflow_core::link`: messages, `LinkHandoff`,
   `TrackRef`, versioning, with fixture files the Android repository copies.
   Lift the whole-file hash cache out of `remote::` so the link does not depend
   on the `sync_v2` feature. Rust tests only.
2. **The desktop listener.** Settings card, TLS certificate, the private-address
   filter, pairing window, `phone_link_device` in `app.db`, the paired-phone list
   with _Revoke_, mDNS advertisement. Tested with a scripted client; no phone
   yet.
3. **Pairing and control on Android.** QR and typed-code pairing, token store,
   the WebSocket, a remote screen, the second media session for the system
   controls.
4. **Hand-over both ways on the LAN**, then _Continue from…_ through the server
   queue for bound devices — on Android, a client for
   `GET`/`PUT /api/v2/queue` on the server session it already holds; nothing
   on the server.
5. **Identity.** The Android full-hash cache, the pair table on both sides, the
   candidate screen for MusicBrainz / ISRC suggestions.
6. **Sync with preview** — likes and ratings first, then playlists with
   placeholders, then history once the phone logs events.
7. **Consented copies**, phone → desktop then desktop → phone.
8. **Server-side asks**, for the repository that owns them: a route listing an
   account's devices and one revoking a device; writing `device.last_seen_at`;
   naming the device in now-playing (one row per user today). Separately, the
   Android app's own sync v2 client — it implements none of the journal today —
   and signing the phone into the server from the desktop's session (the server
   guide's "pairing a device is done from a session"), so a phone joins the
   account without typing a password.

Stages 1–4 are the feature a user notices; 5–7 are what makes the word _sync_ in
the title true; 8 is the work the server path needs to be more than a parked
queue.

## Out of scope

- **Real-time control across networks** (a relay on the server).
- **A browser remote** from a phone by QR code (a separate 2.1 card). It would
  be a third client of the same listener and the same pairing, which is a reason
  to build the listener first and the browser page later, not a reason to merge
  the two.
- **More than one desktop per phone in a single sync.** A phone may pair with
  several desktops; each has its own baseline and its own pairs, and they are
  never merged with each other.
- **iOS.** The Android app is the only mobile client.
- **Grouping a transcode and its source as one track.** A `copied` pair is
  identity for resolving and syncing; the two remain two files in two
  libraries.

## Open questions

1. **A phone paired with a profile that is not the active one.** Refuse with
   `profile_inactive` (this draft), or let the phone switch the desktop's
   profile? Switching from a pocket is surprising for whoever sits at the
   desktop.
2. **Where copies land on the phone.** `Music/WaveFlow` through MediaStore —
   visible to other players, kept on uninstall — or the app's own storage —
   private to WaveFlow, deleted with it? This draft leans to MediaStore: a copy
   the user asked for is theirs.
3. **Scanning the QR code.** Play services' code scanner needs no camera
   permission but needs Play services; a bundled scanner needs the camera
   permission and works everywhere. Is a build without Play services a goal of
   the Android app?
4. **Expiry of an unused pairing.** Never (revocation is the control), or after
   90 days without a connection?
5. **Applying a sync without a preview** once a first sync has been reviewed,
   when it holds no conflict — offered as an option, or never?
6. **Transcoding for copies to the phone** in 2.1 (which codec, which bitrates),
   or originals only at first?
7. **Real-time control outside the LAN.** Worth a server RFC for a relay, or is
   LAN-only control the accepted shape?
8. **Plays imported from the phone**: counted in statistics and Wrapped but never
   scrobbled (this draft) — or kept out of statistics too?
9. **The desktop controlling the phone** in 2.1, or after the phone → desktop
   direction has been used for a while?
