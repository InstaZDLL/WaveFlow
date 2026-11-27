# RFC-010 — WaveSync: a shared queue between desktops, without a server

- **Status**: Accepted
- **Date**: 2026-11-25 (drafted), 2026-11-27 (decisions taken)
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-008](RFC-008-phone-link.md) (the LAN listener, its TLS, its private-address filter and its advertiser, none of them built yet), [RFC-006](RFC-006-deduplicating-the-two-catalogues.md) (content proof is the only proof).
- **Builds on**: `DetachedSession` ([`remote_playback`](../../src-tauri/crates/app/src/remote_playback.rs)), the share reader ([`netlib::reader`](../../src-tauri/crates/app/src/netlib/reader.rs), [`netlib::cache`](../../src-tauri/crates/app/src/netlib/cache.rs)), [`player_actions`](../../src-tauri/crates/app/src/player_actions.rs), the load ordering of [#622](../architecture/invariants.md#every-load-command-carries-its-intent).
- **Implementation**: in progress, in the order of [the lots](#lots). The product decisions were taken by the maintainer on 2026-11-27 and are listed in [Decisions taken](#decisions-taken).
- **Name**: **WaveSync**, a brand name kept as it is in all 17 locales, like WaveFlow. In the code the module is `wavesync` (`waveflow_core::wavesync`, the app's `wavesync/`), never `sync`, which names the server's sync v2 (`remote::`, `sync_v2`) and the removed v1 stub.

---

## The situation this RFC answers

Several people, each with WaveFlow and their own library, sit in the same house
and want to listen together: one queue, everyone adding to it from what they
own, everyone hearing what plays, including tracks they do not have. Today
nothing in the app can do it without a WaveFlow Server, and even a server only
plays what was uploaded to it.

Most of the pieces exist, under other names:

| Need                                             | What exists                                                                                                                                                                                                                                                                                                         |
| ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A queue in memory of things that are not my rows | `DetachedSession<E>`: negative ids, no `play_event`, no statistics, no scrobble; takes over next / previous and the end of a track while active; undoes only its own install (`set` / `clear_if` / `restore_if`). Used by the remote queue, files played without importing them, podcast episodes.                  |
| Reading a file somewhere else, range by range    | `NetworkFs::read_at` → `RangeReader` (segments, a stall guard a newer load can release) → `ActiveStream::open_from_source`, with a working file filled by offset and published once whole (`netlib::cache` over `audio::stream_cache`). Built for WebDAV and SMB; nothing in the reader knows which protocol it is. |
| Serving a file with ranges                       | `dlna::http::parse_range` and `range_not_satisfiable`, shared by DLNA and `media_loopback`.                                                                                                                                                                                                                         |
| Being reachable on the LAN, opt-in               | DLNA (axum, SSDP on every interface, the address ranking of `lan_addrs`) and MPD; both bound to `0.0.0.0`, both without real authentication.                                                                                                                                                                        |
| Pairing, TLS, refusing public peers, mDNS        | Designed in RFC-008 Decisions 2–3, **not built**. This RFC needs the same listener and is a reason to build it once.                                                                                                                                                                                                |
| A control surface that is not the webview        | `player_actions`, which the tray, the media keys, the taskbar buttons and MPD already go through.                                                                                                                                                                                                                   |
| Proving two files are the same                   | `(size, full_hash)`, whole-file BLAKE3, through `remote::hashing` (gated on `sync_v2`; RFC-008 stage 1 lifts it out).                                                                                                                                                                                               |

Two things do **not** exist, and the plan has to account for them:

- **A detached session has no gapless and no crossfade.** The decoder's
  `PrefetchNext` is answered only for the library queue
  ([playback](../features/playback.md#crossfade)): "the prefetch is not answered
  for a remote track or a file played without importing it". A WaveSync queue built
  on `DetachedSession` would therefore have a gap at every track change, as the
  remote queue has today. [Lot 4](#lots) fixes it for every detached session at
  once.
- **A stream handed over as a `MediaSource` does not take the DSD path.**
  `open_from_source` goes through symphonia; DSD is a native pipeline over a
  file. Whether a locator opened through `NetworkOpener` reaches it is to be
  measured; until then a DSD track [cannot be shared](#decision-6--what-can-be-shared).

## What WaveSync is not

**It is not a library merge.** A guest's tracks never become rows in anybody
else's library, and nothing a guest hears is written into the listener's music
folders. The bytes a listener's player reads are cached for the length of the
session, in the cache directory, and deleted when it ends.

**It is not copy protection.** A participant's machine receives the bytes of the
tracks that play, as any player receiving a stream does. The design keeps
everything else out of reach — the rest of the library, its paths, its
structure — and keeps the exposure to what was put in the queue, while it is
there. It does not, and could not, stop a participant from recording what their
own machine plays.

**It is not always on.** Nothing listens, advertises or browses until somebody
starts or looks for a session, and everything stops when it ends.

## Decision 1 — the network reach: the LAN, and a private overlay if the user has one

Three options were weighed.

| Option                           | What it needs                                                                                                                                                                                                                                                                                                                                                                                          | Cost (relative)                                     | What does not work                                                                                                    |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| **A. LAN only**                  | One TCP listener on the host, mDNS to find it, a code to join. Everything else in this RFC.                                                                                                                                                                                                                                                                                                            | 1 (the baseline: lots 0–5)                          | Two houses. A network with client isolation (some guest Wi-Fi, some hotels): nobody can reach anybody, and we say so. |
| **B. LAN + private overlay**     | A. plus: the address filter also accepts the shared-address range a mesh VPN hands out (`100.64.0.0/10`) and IPv6 ULA, and a join can be made from an invitation link holding an address instead of from discovery (multicast does not cross most overlays).                                                                                                                                           | ≈ 1.1 (a filter entry, a link format, a text field) | Users without such a VPN. We do not ship or operate one.                                                              |
| **C. Internet, by session code** | A rendezvous that maps a code to addresses (a server, which the brief rules out, or the user's WaveFlow Server if they have one); NAT traversal (STUN-style address discovery and UDP hole punching, so a datagram transport such as QUIC instead of TCP); a relay for the cases punching fails — symmetric NAT, carrier-grade NAT on mobile connections — and a relay is a server that carries audio. | ≈ 3–4, plus an operated service                     | Without a relay, a share of pairs that cannot connect at all, and nothing we can do from the app. With one, a server. |

**Decided: B.** A is the feature; B adds almost nothing and covers the person
who already links their machines privately. C contradicts the premise —
the moment a relay exists, it is a server that sees the audio — and costs more
than the rest of the feature together. If the maintainer wants listening across
the Internet later, the honest shape is a **server RFC**: WaveFlow Server as the
rendezvous and relay for its own users, reusing its accounts. Nothing in A or B
would have to change for it, because the protocol below does not care how the
connection was made ([decision 2](#decisions-taken)).

**How the filter recognises an overlay's addresses.** The private-address filter
of [Decision 5](#decision-5--admission-and-security) judges the peer's source
address before the TLS handshake:

| Range                                           | What it is                                                                     | Accepted                                                                                                                             |
| ----------------------------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------ |
| `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | RFC 1918 private networks                                                      | always                                                                                                                               |
| `169.254.0.0/16`, `fe80::/10`                   | link-local                                                                     | always                                                                                                                               |
| `127.0.0.0/8`, `::1`                            | loopback (two sandboxes on one machine)                                        | always                                                                                                                               |
| `fc00::/7`                                      | IPv6 unique local addresses (RFC 4193)                                         | always; Tailscale's IPv6 prefix `fd7a:115c:a1e0::/48` lies inside it, so a tailnet's IPv6 addresses pass without a rule of their own |
| `100.64.0.0/10`                                 | the shared address space of RFC 6598, from which Tailscale assigns its devices | only with _Allow private VPN addresses_ on, and only while the host has an address of its own in that range                          |
| `::ffff:a.b.c.d`                                | an IPv4 peer reaching a dual-stack socket                                      | judged as the IPv4 address it carries                                                                                                |
| anything else                                   | public                                                                         | never                                                                                                                                |

`100.64.0.0/10` is not accepted by default because it is not only a VPN's:
carriers use the same block for carrier-grade NAT, so on a machine connected
through such a carrier it is the address space of the carrier's other
customers. The switch, in the WaveSync section of Settings and off by default,
says so, and the condition on the host's own address keeps it from applying on
a machine that is not on an overlay at all. Rust's `Ipv4Addr::is_private` does
not cover this block, and `is_shared` is not stable, so the filter tests the
ranges itself, with a unit test per row of this table.

## Decision 2 — the host is the single writer, and its log is the order

**One participant hosts.** The host's machine holds the session: its
participants, its queue, and who may do what. Every other participant holds a
replica and changes nothing locally; a gesture on a guest is a **request** to
the host, and the change happens when the host broadcasts it.

**The replication is an ordered log, not a vector clock.** Each change the host
applies gets the next sequence number, and the host broadcasts
`{seq, op}`. A guest applies ops in order and remembers the last `seq` it
applied. The ops are few:

- `participant_joined` / `participant_left` / `participant_state_changed`
  (listening, buffering, not following, away);
- `entries_added {entries, position}` / `entry_removed` / `entry_moved`, a
  position being the end, next, or after an entry that has not played;
- `transport {cursor, anchor}` — what plays, from where, and whether it is
  playing ([Decision 4](#decision-4--synchronisation-follow-the-host-within-half-a-second));
  every entry before the cursor counts as played, and the oldest played
  entries beyond a hundred are dropped by every participant at the same op;
- `rights_changed`, `ended`.

The host applies each op it makes through the same function the guests apply
it with, so the two cannot disagree on what an op means. A replica that
receives op _n + 2_ after _n_ knows it missed one; one whose op does not apply
knows it diverged; either way it asks for a snapshot, which it checks (cursor in
bounds, the playing entry where the cursor says, no two entries or participants
under one id) before believing it.

A request carries a client id; the host answers it with the `seq` that applied
it or with a refusal (`not_allowed`, `entry_gone`, `queue_full`, …), so a guest's
button can say what happened. There are no concurrent writers, so there is
nothing to merge: RFC-005 dropped logical clocks for the server's sync for the
same reason, and a session that lives for an evening does not need them either.

**Reconnecting replays, or resets.** The host keeps the last few hundred ops in
memory. A guest that drops and comes back within the session presents its
participant token and its last `seq`; the host sends the ops after it, or, if
they have been trimmed, a snapshot (the whole state, which is small: a queue of
entries and a list of participants) and the `seq` it stands at.

**Nothing is persisted.** No table, no migration. A session exists in the
memory of the machines that take part. The only settings are preferences
(the name shown to others, the default rights of a new session), in
`profile_setting` through `useProfileSetting`, defaults in `settingsRegistry.ts`.

**When the host leaves, the session ends.** That is the only rule lot 3 ships:
the guests see _The session has ended_ and their own queue comes back as it
was. Every byte a guest did not own reached it through the host, so the current
track plays to its end only when the guest has all of it — its own file, or a
copy the session cache holds whole; otherwise playback stops at once, whoever
owned the track, rather than at an arbitrary point when the cached part runs
out. Handing the session to someone else is
possible — every guest holds the whole state, so the new host can resume from
its replica — but it changes who serves which bytes and who the others connect
to, and it is a lot of its own ([lot 5](#lots), [decision 5](#decisions-taken)).
A guest whose connection drops takes its entries' bytes with it: they stay in
the queue, marked _owner away_, are skipped when reached, and come back if the
owner reconnects within the session. A guest who leaves on purpose, or whom the
host removes, does not come back as the same participant, so its entries still
to play are removed with it; one already playing finishes from what the
listeners hold, as when the host leaves.

## Decision 3 — the audio crosses as the original file, through the host

**What crosses is the file, range by range.** The owner of an entry reads its
own file and sends the bytes asked for; the listener decodes them as it decodes
a track on a share. The original, not a transcode: a desktop on a LAN has the
bandwidth (CD-quality FLAC is about 1 Mbit/s, 24-bit/96 kHz stereo about 3, a
5.1 FLAC about 4), no encoder runs on anybody's machine, and the listener's own
ReplayGain, EQ and channel mix apply to the real thing. The owner sends the
track's ReplayGain values with the entry, so normalisation sounds the same
everywhere.

**Every connection goes to the host; the host relays.** Two topologies were
weighed:

|                                    | Star: the host relays                                                | Mesh: every owner serves                                      |
| ---------------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------------- |
| Who opens a port                   | the host only                                                        | everyone who adds a track                                     |
| Firewall prompts, network profiles | one machine                                                          | every machine                                                 |
| Owner's upload for N listeners     | one read, shared by the host's cache                                 | N reads                                                       |
| Host's load                        | relays guest-owned bytes (≈ N × the bitrate; tens of Mbit/s at most) | serves its own tracks only                                    |
| Code                               | a ranged read over the guest's existing connection, plus the relay   | a listener per participant, addresses distributed by the host |

**Decided: the star.** Only the host listens, which keeps the opt-in to
one deliberate gesture on one machine, one Windows firewall prompt, one place
where a peer is admitted. Because the participants follow the same position
([Decision 4](#decision-4--synchronisation-follow-the-host-within-half-a-second)),
they ask for the same ranges at nearly the same time, and the relay's segment
cache answers most of them from one read of the owner's file.

**One connection per guest, both ways.** A guest opens one TLS WebSocket to the
host and keeps it for the session. Text frames carry the control messages above;
binary frames carry ranged reads, in both directions:

- `read {req, entry, offset, len}` — `len` at most 1 MiB, answered by
  `data {req, bytes}` or `fail {req, code}`;
- the host sends `read` to the owner of a guest-owned entry, and a guest sends
  `read` to the host for any entry it does not own.

On each side this is a `NetworkFs` whose `read_at` is that exchange (`stat`
returns the size the entry announced; `list` is refused). It plugs into
`RangeReader` and the cache exactly as WebDAV and SMB do, so the stall guard,
the segments and the "published once whole" rule are the ones already tested.
The relay on the host is the same reader, with its segments shared between the
guests asking.

**How the player opens it** is to be decided in lot 3 between two shapes, and the
second is preferred:

1. a new load command carrying the entry, opened through `open_from_source`;
2. an in-memory locator (`wfsync://<session>/<entry>`) handed to the
   `NetworkOpener` already installed for shares, through
   `LoadRemoteFileAndPlay` with a new `DetachedOrigin::WaveSync`. This one reuses
   the path-based `SetNextTrack` when lot 4 opens the prefetch to detached
   sessions. Its risk is that the locator is a path the rest of the app may
   look at: `network_locator::is_locator` must answer `true` for it so nothing
   tries to open it as a file, and nothing may store it — a locator for a share
   is a library row, this one names something that disappears with the session.

**Preloading the next entry.** Each participant starts reading the next entry a
fixed lead before the current one ends (the decoder already asks at
`PrefetchNext`; AutoMix asks `AUTOMIX_PREFETCH_LEAD_MS` earlier), so the first
seconds are in the cache when the hand-off comes. Lot 3 ships the read-ahead;
the gapless or crossfade hand-off itself needs the prefetch to be answered for a
detached session, which is lot 4.

**A track the listener already owns is played from the listener's own file.**
When an entry's `(size, full_hash)` matches exactly one of the listener's local
tracks — the rule of [RFC-006 Decision 1](RFC-006-deduplicating-the-two-catalogues.md#decision-1--metadata-may-refuse-an-identity-never-establish-one),
nothing matched by title — that file plays and nothing crosses. The owner
hashes the file once per entry added and announces the hash with the entry.
The listener looks up its local tracks of exactly that size, and hashes those
candidates on demand when the entry is added, through the same cache keyed by
`(file_size, file_modified)` that `remote::hashing` keeps, so a file is read
once until it changes. Candidates are few (an exact byte size rarely repeats),
but the work is bounded all the same: at most a handful per entry, in the
background. Until a match is proven — no candidate, a hash still being
computed, or a mismatch — the entry streams like any other. Lot 4.

**The session cache is deleted when the session ends**, and again at the next
launch if the app died first. It lives in the cache directory, never in a
library folder, and is bounded (a few GiB, the oldest entries evicted).

## Decision 4 — synchronisation: follow the host, within half a second

Three levels were weighed.

| Level                | What it promises                                                                                           | What it needs                                                                                                                                                                                                                                                                                                                                                       |
| -------------------- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0 — shared queue     | one queue; everyone plays it at their own pace                                                             | the log; each participant's own transport                                                                                                                                                                                                                                                                                                                           |
| 1 — follow the host  | the same entry at the same position, within about half a second; a late joiner starts where the others are | anchors from the host (`{entry, position_ms, at, playing}`, sent on every change and every few seconds), the host's clock estimated on each guest from the WebSocket's round trips (the median of a few, half the round trip as the delay), a seek when a guest drifts past a threshold (≈ 400 ms) and is not buffering                                             |
| 2 — tight, same room | close enough that two machines in one room do not echo (tens of milliseconds)                              | level 1, plus: each output's latency, which the app does not know for every backend (Bluetooth is the worst case and unknowable from here) and so a per-device offset the user sets by ear; drift corrected by a tiny speed change rather than a seek, through the stretcher that already exists; a scheduled start ("begin at host time T") instead of "begin now" |

**Decided: level 1 in lot 3, level 0 as a mode a participant can fall back to,
level 2 later and without a promise.** Level 1 is what "listening
together" means across the rooms of a house and is cheap once anchors exist —
RFC-008 already specified the same anchor shape for the phone. Level 2 is
measurable work with an uncertain end: if two machines in one room is a use case
the maintainer wants, it gets its own lot and its own measurements, and the
interface says nothing about echo until then ([decision 3](#decisions-taken)).

Rules that follow from level 1:

- **The host's playback is the reference.** The host's own transport moves the
  group; a guest pressing pause or seek either sends a request the host applies
  for everyone (if the session allows it) or leaves the group for local
  control: _You stopped following the group — Rejoin_
  ([decision 6](#decisions-taken)).
- **Speed is pinned to 1× while following**, as DoP pins it: a position that
  moves at another rate would be corrected every few seconds.
- **The transition is the host's.** A crossfade or AutoMix the host uses moves
  the boundary between tracks; a guest following the anchors gets that boundary
  from the next anchor and resynchronises after it. With lot 4, guests apply the
  host's crossfade length while following.
- **Nobody waits for a slow guest.** A guest whose bytes have not arrived shows
  _Waiting for the stream_ and joins at the position the group has reached.

## Decision 5 — admission and security

**Nothing listens until somebody hosts.** _Start a session_ opens the listener
on an ephemeral port, starts the mDNS advertisement and shows the code; _End
the session_ closes both. Joining opens no port at all — the guest only
connects out. Browsing for sessions runs only while the _Join_ dialog is open.
Unlike DLNA and MPD, the feature sits behind no switch in Settings
([decision 7](#decisions-taken)): hosting is already an explicit gesture with a
visible state, and nothing listens before it.

**The listener is RFC-008's**, built once for both:

- bound to every interface, refusing a source address that is not private
  (RFC 1918, link-local, loopback, IPv6 ULA and link-local; plus the overlay
  range of [Decision 1](#decision-1--the-network-reach-the-lan-and-a-private-overlay-if-the-user-has-one)
  when that option is on) before the TLS handshake; never asking a router for a
  port mapping;
- TLS with a self-signed certificate generated for the session, its SHA-256
  fingerprint published in the mDNS record and in the invitation link
  (`rustls` is already a dependency; `rcgen` would be new).

**Joining takes a code and, by default, the host's approval.**

1. The host's panel shows an 8-character code drawn from the capitals and
   digits of the alphabet [`generatePassword`](../../src/lib/password.ts) uses
   (32 symbols, no look-alikes), read case-insensitively since it is said
   aloud, and a _Copy invitation_
   link holding the address, the fingerprint and the code, for an overlay or a
   network where multicast does not pass.
2. A guest picks the session from the list mDNS gives (the session's name and
   the host's display name, nothing about the library) or pastes the link, and
   types the code.
3. The host sees _Alex wants to join_ and accepts or refuses
   ([decision 4](#decisions-taken) decides whether approval can be turned
   off). The code is a shared secret said aloud in a room; the approval is
   what makes it safe to say aloud.
4. The host mints a participant token — 32 random bytes — sends it once over
   the connection, keeps only its SHA-256, and compares in constant time on
   reconnection. It lives in memory and dies with the session.

**Wrong codes slow their sender down, they do not lock everyone out.** A
failed exchange makes its source address wait before the next attempt, the
wait doubling with each failure (1 s, 2 s, 4 s … capped at a few minutes), and
the host's panel shows _Someone is trying wrong codes_ with the count. Closing
admission after a fixed number of failures would let any machine on the LAN
keep it closed for everybody by failing on purpose. With eight characters from
32 symbols (2⁴⁰ codes) and a guess costing one online exchange
at a doubling delay, guessing the code is not a practical attack; the host can
also draw a new code at any time.

**The code is never sent; both sides prove they know it, the host first.** A
fingerprint learned from mDNS proves nothing: anybody on the LAN can announce a
session with a certificate of their own, and a guest that typed the code into
it would hand the code to that impostor, or let it sit between the guest and
the real host and relay. So the code is not presented, it is used as the
password of a password-authenticated key exchange (SPAKE2; the `spake2` crate
would be new) bound to the TLS connection it runs in, through the connection's
exporter (RFC 5705). Each side ends with a confirmation only a holder of the
code can compute for _that_ TLS session, and the host sends its own first:

- an impostor that does not know the code cannot produce the host's
  confirmation, and the guest closes the connection having revealed nothing it
  could test guesses against offline;
- a relay in the middle runs two TLS sessions, so neither confirmation matches
  on the other side, and both ends close;
- a wrong code fails the exchange and counts against its sender's address.

A fingerprint from mDNS is therefore only an address hint, and one from an
invitation link is pinned before the exchange as an extra check. Only after the
exchange does the host show the admission request.

**Someone who knows the code can still pose as the host.** The code proves that
both ends were told it, not which machine is the host; a person in the room who
heard it could announce a session of their own under the same name. That is
accepted as an insider's prank, with two cheap checks against it: once in, a
guest sees the participant list, which an impostor's session would not match;
and both the host's panel and the guest's join screen show the host
certificate's fingerprint as four groups of four characters (its first 64 bits,
`3F9A 77C2 0B1E D4A8`), for whoever wants to compare them.

**Only what is in the queue is readable.** An entry has an opaque id
(`wse_` and 128 random bits) minted by the owner when it is added; the owner
maps it to a track **in memory** and answers a `read` only for an entry the
session's cursor has not passed: the one playing and those after it. An entry
the cursor has passed stays listed as played but is no longer readable, and
removing an entry revokes it at once — the playing one included, which removing
skips for everyone. The next `read` of a revoked entry fails, and nothing else — no other track, no browse, no search,
no path — is reachable through the connection. Before answering, the owner
checks again that the path is a local file (not a [locator](../architecture/invariants.md#a-stored-track-path-may-be-a-locator-not-a-file))
and that its size and modification time are still the ones it was hashed at.
A file rewritten since — a tag edit, a replaced file — no longer is the entry
that was announced, even at the same size: the owner refuses the read with
`entry_changed`, and the host removes the entry for everyone, so no listener
ever mixes the old bytes it cached with new ones, nor plays a local copy proven
against a hash the owner's file no longer has.

**What an entry tells the others**: title, credits, album, duration, codec,
sample rate, channels, size, ReplayGain values, and a cover reduced to a small
JPEG. Never a path, a folder, a track id or a file name.

**The file crosses whole, its tags included.** What the owner sends is the
original bytes, so whatever is embedded in them travels too: the full-size
cover, embedded lyrics, comments, identifiers, an encoder's or a ripper's
notes. The listener's player shows only what the entry announced, but the bytes
are on the listener's machine for the length of the session. Stripping the tags
would mean serving a rewritten file whose offsets no longer match the
original's, and every ranged read translated between the two; that is not
done. The add dialog says it in one line (_The whole file is shared, including
what its tags contain_), and the owner decides what to add knowing it.

**Rights.** The host can remove any entry, reorder, skip, and remove a
participant; a guest can add, and remove what they added. Whether guests may
skip or control the transport is a setting of the session, off by default
([decision 6](#decisions-taken)).

**Nothing secret reaches the log** ([invariant](../architecture/invariants.md#no-secret-reaches-the-log)).
The code, the tokens and the invitation link are never logged, and neither is a
participant's display name or a track's title: a line names a participant by a
short id and an entry by its `wse_` id. Errors cross to the interface as
`AppError::shown` with a `ShownKind`
([invariant](../architecture/invariants.md#an-error-shown-to-the-user-carries-a-kind)).

**The webview's CSP does not change.** Everything crosses in the backend: the
webview `invoke`s commands and receives `wavesync:*` events, and a cover is handed
to it as a `data:` URL or a file in the cache read through the asset protocol,
both already allowed. No `connect-src`, no `media-src` source is added
([invariant](../architecture/invariants.md#the-webview-runs-under-a-content-security-policy)).

**Offline mode stops it.** The [invariant](../architecture/invariants.md#process-wide-offline-mode)
is that every outbound path checks `offline::is_offline()`, and a guest's
connection and reads are outbound. DLNA and MPD do not consult it because they
only answer; a session also connects out. So with offline mode on, _Start_ and
_Join_ are disabled with the reason, and turning it on during a session ends it
([decision 8](#decisions-taken)).

**Platforms.**

- **Windows.** The first _Start a session_ makes the firewall ask, as DLNA and
  MPD do. On a network Windows classes as _Public_, the default answer blocks
  inbound connections and guests time out with no error on the host's side,
  which cannot see a connection the firewall dropped. So the host's panel shows
  the hint (_Nobody can join? Check that this network is private_) whenever the
  session has had no guest for a minute.
- **Linux, Flatpak.** `--share=network` is already granted for DLNA and MPD and
  covers a TCP listener and multicast. The mDNS advertiser is the app's own
  (a pure-Rust responder on UDP 5353 with address reuse, as SSDP coexists with
  other UPnP services), not Avahi over D-Bus, so no `--system-talk-name` is
  added to the manifest. Measured in lot 0 on Fedora with Avahi running: both
  answer on UDP 5353, `avahi-browse` lists the advertisement on IPv4 and
  IPv6, and a guest of this build finds it and joins (`lan_probe`).
- **macOS.** `NSLocalNetworkUsageDescription` already exists for DLNA and MPD
  and gets a sentence for sessions in lot 2, when the app itself starts
  advertising. The responder sends and receives multicast on its own sockets
  rather than through the system's Bonjour API, which is what
  `NSBonjourServices` declares service types for; the prompt is checked on the
  Mac with the app in lot 2, since a process started over SSH does not stand
  for the app in the system's privacy settings.

## Decision 6 — what can be shared

| Source                                  | In a session                                                                                                                                                                                                                                                                                             |
| --------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| A library track on a local disk         | **Yes.** The case the feature is for.                                                                                                                                                                                                                                                                    |
| Multichannel (5.1, 7.1)                 | **Yes.** The original crosses; each listener folds it to their own speakers through `ChannelLayout`, as for a local file.                                                                                                                                                                                |
| Hi-res PCM                              | **Yes**, at its bitrate.                                                                                                                                                                                                                                                                                 |
| DSD (`.dsf`, `.dff`)                    | **Not at first.** A stream opened as a `MediaSource` does not reach the DSD pipeline today, and DSD256 is ≈ 23 Mbit/s per listener. Refused with a reason when added; revisited after measuring in lot 3.                                                                                                |
| A track on a network share (`wfnet://`) | **Not at first.** It would cross twice (share → owner → host → listener) and the owner's reader would serve two consumers. Possible later; refused with a reason.                                                                                                                                        |
| A WaveFlow Server track                 | **No.** Its bytes are the server's, under the owner's account and its tickets; serving them to people the server does not know is not the owner's to decide.                                                                                                                                             |
| A file played without importing it      | **No.** Only library tracks can be added; such a file can be imported first.                                                                                                                                                                                                                             |
| Web Radio                               | **No.** Endless, with no file to read by range. Each participant can tune the same station themselves.                                                                                                                                                                                                   |
| Podcast episodes                        | **No.** An episode is not a track ([RFC-009 Decision 2](RFC-009-podcasts.md#decision-2--an-episode-is-not-a-track)), and its address must never leave the machine ([invariant](../architecture/invariants.md#a-podcast-address-reaches-no-log-and-no-archive)).                                          |
| Lyrics, Canvas, music videos            | **Not carried separately.** A listener's own lyrics providers look the track up by its metadata as for any track they play; a sidecar `.lrc`, a Canvas or a music video does not cross. Lyrics embedded in the file's tags do, with the rest of the file ([above](#decision-5--admission-and-security)). |

**History, statistics, scrobbling.** A guest-owned track plays under a negative
id in a detached session and leaves no trace, like a remote or an external
file. The owner's own tracks, heard in the session, count in the owner's
history and statistics and are scrobbled by the owner, who listened to their
own track; nothing anybody else owns ever does, on anybody's machine
([decision 9](#decisions-taken)).

**Caps.** At most 8 participants and 500 entries in the queue, and one file at
most 2 GiB, so the host's upload and every cache stay bounded. A refusal names
the cap.

## Decision 7 — the interface

Built in the skin's layout (`skin.layout`, never a skin id), checked in Fluid
and in a classic skin.

- **Entry point.** _WaveSync…_ in the player bar's "⋯" overflow menu, as
  the [invariant](../architecture/invariants.md#adding-a-new-player-bar-action)
  asks for a new player-bar action, opening a dialog with two choices: _Start_
  (session name, default rights, approval on) and _Join_ (the sessions mDNS
  finds, a field for the code, _Paste an invitation_). Both dialogs use
  `useModalA11y`.
- **The WaveSync panel.** A right panel, a flex sibling mutually exclusive with
  the queue, lyrics and now-playing panels through `PlayerContext`, as they are
  with each other. It shows:
  - the participants, each with initials and a colour, the host marked, and a
    state: _listening_, _waiting for the stream_, _not following_, _away_;
  - the queue, each entry with who added it and its state: _playing_, _ready_,
    _loading_, _owner away — will be skipped_, _cannot be shared_ (and why);
  - the code and _Copy invitation_ for the host, and the admission requests;
  - _Leave_ (_End the session_ for the host).
- **Adding.** _Add to WaveSync_ in the context menu of a track, an album,
  a playlist and a selection, next to _Add to queue_, present only during a
  session. _Add to queue_ keeps meaning the user's own queue.
- **The user's own queue is parked, not lost.** Joining or starting installs the
  group's detached session; leaving restores what the user had, as a stopped
  queue at its old position. Starting a library track by hand during a session
  asks first: _Leave the group's playback?_ — the participant stays in the
  session and can _Rejoin_.
- **Non-frontend surfaces** — media keys, tray, taskbar buttons, MPD — act
  through `player_actions`, which turns next / previous / pause into the same
  requests the panel's buttons send **only while this participant follows the
  group**; a participant who has stopped following keeps local control of what
  they play, from every surface
  ([invariant](../architecture/invariants.md#non-frontend-control-surfaces-go-through-player_actions)).
- **Copy** in all 17 locales, French first.

## Decision 8 — ordering of the loads

A WaveSync-driven load is a load like any other
([invariant](../architecture/invariants.md#every-load-command-carries-its-intent)):
the intent is claimed when the `transport` op (or the user's gesture) arrives,
before the entry is resolved, its cache looked up or its first range read; the
dispatch is claimed under `lock_publish` before the cursor moves; and the session
is installed through `DetachedSession::set`, undone through `clear_if` /
`restore_if` if the load is superseded or fails. A user's pick on a guest during
a session therefore wins over a `transport` op that arrives while it is being
prepared, by the same rule that orders any two local gestures. An output
rebuild puts back the WaveSync entry from `last_load`, as for every detached origin.

## Lots

Each lot ships on its own branch, is tested before the next starts, and does not
change what an earlier lot decided. Machines: this PC (Windows), the Fedora
laptop and the Mac. Rust tests of the app crate run on the Fedora through the
test script, since that crate's test binary does not start on Windows. Two
participants on one machine are two sandboxes with two identifiers and two
ports, which the address filter allows on loopback.

| Lot | Content                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Testable on two machines                                                                                                                                                                                                                                                                                                             |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 0   | **The shared LAN listener** (RFC-008 stage 2's foundation, built once), as `waveflow_core::lan` behind the core's `lan` feature, so its tests run on Windows too: TLS 1.3 with a generated certificate, the private-address filter, the mDNS advertiser and browser, token minting and hashing, the code exchange (SPAKE2 bound to the TLS exporter, host confirming first) with its per-address wait, and the `lan_probe` example that runs it between two machines without the app. | Rust tests (filter, tokens, certificate). PC ↔ Fedora: the Fedora's browser finds the PC's advertisement and completes a TLS handshake against the pinned fingerprint, with Avahi running; a scripted impostor announcing the same session never receives a guest's confirmation. Mac: the local-network prompt and the mDNS browse. |
| 1   | **The protocol, pure**: `waveflow_core::wavesync` — the ops, the log and its replay, the snapshot, the rights, the request/answer pairs, the ranged-read frames. No network, no UI.                                                                                                                                                                                                                                                                                                   | Rust tests only: replay from any `seq`, snapshot after trimming, refusals by right, entry revocation.                                                                                                                                                                                                                                |
| 2   | **Sessions and the shared queue, no audio from others**: start / join / approve / leave, the panel, adding and removing, each participant playing only the entries they own (level 0). Offline mode, caps, refusals by source.                                                                                                                                                                                                                                                        | Two sandboxes on the PC, then PC ↔ Fedora: Fedora adds an album, the PC sees it with "added by"; the host removes an entry and the guest's panel follows; wrong codes from one address are slowed down and announced on the host.                                                                                                    |
| 3   | **Streaming and following the host**: the `NetworkFs` over the connection, the relay on the host, the session cache and its deletion, the read-ahead of the next entry, anchors and level 1, _waiting for the stream_, _owner away_, the host's departure ending the session.                                                                                                                                                                                                         | PC ↔ Fedora: a track only the Fedora owns plays on the PC, seek included; both stay within 0.5 s (measured from the anchors); pull the Fedora's cable and the entry turns _owner away_; end the session and the PC's cache directory is empty.                                                                                       |
| 4   | **Gapless and crossfade for detached sessions** (remote queue, external files and WaveSync alike), the host's crossfade length on guests, and playing a listener's own identical file by `(size, full_hash)`, for which the whole-file hash cache leaves `remote::` (RFC-008 stage 1; moved here from lot 0, which does not use it).                                                                                                                                                  | PC ↔ Fedora: a gapless album owned by the Fedora plays without a gap on the PC; a track both own plays from the PC's file (no read reaches the Fedora).                                                                                                                                                                              |
| 5   | **Resilience**: reconnection by `seq` after a dropped Wi-Fi, an explicit _Hand over hosting_ before leaving, the overlay option of Decision 1 with invitation links.                                                                                                                                                                                                                                                                                                                  | PC ↔ Fedora: the Fedora's Wi-Fi off and on, it resumes without rejoining; the PC hands hosting to the Fedora and playback continues. Over a private overlay if the maintainer has one.                                                                                                                                               |
| 6   | _(only if wanted)_ **Level 2**: scheduled starts, per-device latency offset, drift corrected by speed.                                                                                                                                                                                                                                                                                                                                                                                | PC and Fedora in one room, by ear and by a microphone recording of both.                                                                                                                                                                                                                                                             |

Lots 0 and 1 have no user-visible effect and can run in parallel. Lot 2 is the
first one the maintainer tests by hand.

## Out of scope

- **Listening across the Internet without the user's own private network**
  ([Decision 1](#decision-1--the-network-reach-the-lan-and-a-private-overlay-if-the-user-has-one));
  a server RFC if it is ever wanted.
- **The Android app** as a participant. The protocol lives in the core so that
  it can join later; RFC-008's listener is the same, so a phone that can pair
  could also join. Not designed here.
- **A browser joining by link.** The webview-free design would need an HTTP
  front of its own; a separate card.
- **Persistent sessions** (a weekly session that remembers who was in it).
- **Video, Canvas, lyrics and covers in high resolution** crossing.
- **Voting** on what plays next.

## Decisions taken

Taken by the maintainer on 2026-11-27. Each follows this draft's
recommendation except the name.

1. **Name**: **WaveSync**, kept as it is in every locale; module `wavesync`.
2. **Reach**: the LAN and a private overlay (option B of [Decision 1](#decision-1--the-network-reach-the-lan-and-a-private-overlay-if-the-user-has-one)),
   with the address rules of its table. Listening across the Internet without
   such an overlay would be a server RFC.
3. **Synchronisation**: follow the host (level 1); level 0 as a participant's
   fallback; level 2 later, unpromised.
4. **Admission**: the code and the host's approval; approval is on by default
   and can be turned off per session.
5. **When the host leaves**: the session ends; an explicit _Hand over hosting_
   comes in lot 5. No automatic hand-over.
6. **Guests' rights**: add, and remove what they added; a per-session switch
   _Everyone controls playback_, off by default, for skip, pause and seek;
   reordering and removing others' entries stay the host's.
7. **Access**: the _WaveSync…_ entry is always present in the "⋯" menu; no
   switch in Settings turns the feature on.
8. **Offline mode** stops WaveSync.
9. **History, statistics and scrobbles**: only for the track's owner, on the
   owner's machine.
10. **Formats**: original files only; DSD and tracks on network shares refused
    at first.
11. **The LAN listener is built once**, in lot 0, for RFC-008 and this RFC.
