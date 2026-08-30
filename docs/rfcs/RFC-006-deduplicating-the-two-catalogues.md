# RFC-006 — Deduplicating the two catalogues

- **Status**: Accepted
- **Date**: 2026-08-31
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-005](RFC-005-remote-source-and-sync-v2.md) — this RFC answers the question its Decision 1 deferred, and does not revisit anything else it settled.
- **Implementation**: none yet.
- **Revised** after external review and accepted on 2026-08-31, before any implementation. What the first draft proposed for albums is kept below, as [the rule that was withdrawn](#the-album-rule-that-was-withdrawn), because the reason it fails is the most useful thing in this document.

---

## The question this RFC answers

[RFC-005](RFC-005-remote-source-and-sync-v2.md) closed a door and named the RFC
that would reopen it. Twice:

> Matching a local file to a server track is **out of scope** and needs its own
> RFC.

> An album held both locally and on the server appears **twice**, tagged twice.
> Unifying the navigation is not deduplicating the catalogue; that is reserved
> for its own RFC.

The first half has since been built: `remote_track_link` holds exact,
content-proven pairs, written by a reconciliation pass, by an import, and by an
upload. The second half has not. Two collections that describe the same music
still present it as two of everything above the track: two albums, two artists,
two entries in every browse that ranges over them.

This RFC decides what a proven pair is allowed to change, and what it is not.

## What deduplication is not

Three readings are ruled out before the decisions start, because each has been
proposed at some point in this project's life and each would undo something
RFC-005 got right.

**It is not merging the tables.** The projection lands in `remote_*` and stays
reconstructible: dropping it and re-fetching a snapshot is always a valid
recovery. Writing server rows into `album` or `artist` would fabricate local
entities for things that exist only on a server, which is precisely the
corruption RFC-005's own Decision 1 exists to prevent. Nothing below writes
across that line.

**It is not choosing a winner.** "The local album wins" and "the server album
wins" both discard something the user has: local artwork they picked, or server
tracks their local copy does not contain. A pair is not a conflict to resolve.

**It is not a new matching algorithm.** The proof that two things are the same
already exists and is the only one this project accepts — identical bytes. What
follows derives everything from that proof and adds no second way of guessing.

## Decision 1 — metadata may refuse an identity, never establish one

An album is deduplicated because its tracks are, and for no other reason. Same
for an artist.

The temptation to do otherwise is real and worth naming, because the data is
sitting right there: `remote_artist.sort_key` already holds the output of
`name_match::normalize_name`, the same function that produces
`artist.canonical_name` locally. A join on one column would pair most artists in
most libraries, today, with no new table.

It would also be exactly what RFC-005 forbids one level down. Matching tracks by
title, artist and duration is "explicitly forbidden" there; the reasons do not
weaken as the entity grows, they compound. *Greatest Hits* is not one album.
*John Williams* is not one artist — the film composer and the classical
guitarist share a canonical name, and a library holding both would fold them
into one page with no way for its owner to say otherwise.

The rule is therefore directional rather than absolute, and the direction is the
whole of it:

> **Names and metadata may never establish an identity. They may withhold one,
> and they may feed a suggestion a person confirms.**

Symmetry would be a mistake here. Evidence that two things are the same must be
content; evidence that they are *not* can be anything, because refusing to merge
costs nothing that Decision 5 is not already prepared to pay. A future revision
may well want to say "these two releases share two files but one is called
*Greatest Hits* and the other is not, so do not fold them silently" — a veto,
not a proof. Nothing in this RFC uses such a veto yet, and nothing in it
forecloses one.

## Decision 2 — albums and artists do not get the same rule

They are not the same kind of object, and one predicate for both is what made
the first draft of this document wrong.

**An album is a closed set.** A release is its track list; that is nearly all a
release is. So the question "are these the same album?" is answerable by
comparing sets, and anything less than the set is not an answer.

**An artist is an open grouping.** Nobody's discography is ever complete on
either side, and demanding that it be would mean never pairing an artist at all.
The question "is this the same artist?" is answerable from a sample, because the
sample is evidence about the *person*, not about the extent of their work.

### Albums: a complete bijection over examined sets

Two albums are the same when **every track of each is confirmed-linked to a
track of the other**, one to one, over sets both of which are
[eligible](#decision-3--nothing-is-paired-until-both-sides-have-been-reconciled)
— and when there is **at least one such link**.

That last clause is not redundant. A bijection over two empty sets holds
vacuously: every track of each is linked to a track of the other, because there
are none. Two albums with no tracks would pair, and so would a local album with
an empty server response — a walk that returned nothing, a release whose files
are all unavailable upstream. An album with no tracks is not evidence of
anything, and it never pairs.

Ten local tracks against ten server tracks, all linked: the same album. Ten
against eighteen: **not** automatically the same album, however many links
agree.

That second case is a real record — a standard edition and its deluxe — and
refusing it is a deliberate cost. It is refused because a rule loose enough to
accept it also accepts things that are not records at all, and because
presenting them together is a *different claim* than saying they are the same
release. See [What stays out of scope](#what-stays-out-of-scope): the grouping
of editions is a concept this RFC declines to invent in passing.

### Artists: unanimity, and at least two links

Two artists are the same when **every link touching either one agrees**, and
there are **at least two**.

Unanimity because a contradiction is the only reliable signal that something is
not what it looks like — and because it disposes of the hardest case for free. A
local *Various Artists* compilation has tracks linked to a dozen different
server artists; a "majority of links" rule would have to be taught about
compilations to avoid folding VA into whichever artist won the count. Unanimity
never gets there: the second disagreeing link ends the question. No special
case, no `is_compilation` check, nothing to keep in sync with the album-grouping
rules.

At least two, because one is a coincidence waiting to happen: a single guest
appearance, credited to the featured artist on one side and to the host on the
other, is one link and no evidence.

### The album rule that was withdrawn

The first draft of this document applied the artist rule to albums as well:
unanimity, and at least two links. It is written here rather than deleted,
because the counter-example is the clearest statement of what a link does and
does not prove.

```
Local: "Greatest Hits"          Remote: "Album X"
  Song A  ──────────────────────►  Song A
  Song B  ──────────────────────►  Song B
  Song C                            Song E
  Song D                            Song F
```

Two links. Unanimous. No contradiction. The withdrawn rule concludes that
*Greatest Hits* **is** *Album X*, and hides one of them.

Two links prove that two releases share two recordings. Sharing recordings is
what compilations, singles, soundtracks and reissues are *for*. The withdrawn
rule mistook evidence about tracks for evidence about the set that contains
them — which is exactly the confusion Decision 1 is meant to prevent, committed
one level up instead of by name.

It also failed its own Decision 5. During a mirror walk the links appear one at
a time: two agreeing links arrive, the pair forms, an entity is hidden — and a
third link contradicting them arrives a second later and un-hides it. A rule
that can hide something on incomplete evidence is not made safe by eventually
correcting itself.

## Decision 3 — nothing is paired until both sides have been reconciled

The withdrawn rule had a second flaw, and it survives any threshold: **the
absence of a link is ambiguous**. It means either "these bytes differ" or "this
pair has not been looked at yet", and a predicate that cannot tell them apart is
deciding on evidence it does not have.

So pairing requires an explicit **completeness frontier**. Three properties
have to hold together, and the second is the one that is easy to get wrong.

### Hashed is not reconciled

The obvious frontier — both sides have a digest, therefore both sides have been
examined — is **wrong**, and it is wrong today rather than hypothetically:

```
local track   local_full_hash = ABC123      (written by the upload survey)
remote track  full_hash       = ABC123      (written by the mirror)
remote_track_link             = no row
```

Both files are hashed. Both digests are equal. No link exists, because nothing
has yet compared them — the upload survey fills the cache, and reconciliation
still hashes on its own rather than reading it. A frontier built on "both sides
hashed" reads that absence as *different bytes*, about two files that are
identical.

Digests are an input to reconciliation, not a record that it ran. **Eligibility
requires a completed reconciliation over the exact sets being evaluated**, and
that is a distinct fact which nothing currently records. Adding it — a
reconciliation frontier, per catalogue generation, in the sense below — is a
prerequisite of this RFC, alongside routing reconciliation through
`local_full_hash` so that the two paths stop hashing the same files twice.

### Completeness is versioned, and the version is per entity

`remote_album.mirrored_at` says a walk finished. It does not say the set has not
changed since:

```
12:00  walk completes            mirrored_at = 12:00
12:05  the server adds a track   mirrored_at = 12:00, and now lying
```

A closed set that reopened is not a closed set, so the frontier is attached to a
**generation of that entity's identity inputs** rather than to a moment.
Eligibility holds while the reconciled generation equals the current one.

**Per entity, and not a global counter.** A single number for the whole
catalogue would make an unrelated change invalidate everything: someone stars
one album, the number moves, and five thousand pairs go ineligible until a
reconciliation pass has walked the entire library. Conservative, and unusable.

```
album A   generation 17   reconciled 17   → eligible
album B   generation  9   reconciled  8   → ineligible, and only B
```

**A generation advances only when an identity input changed.** Presentation and
reachability never advance it — that distinction is the whole point of having
two frontiers rather than one:

| Change | Album | Artist | Why |
| --- | --- | --- | --- |
| Track added or removed | invalidates | invalidates | different corpus |
| `full_hash` changed (file replaced or retagged upstream) | invalidates | invalidates | different content proof |
| Track moved to another album | both albums | — | different set |
| `primary_artist` changed | — | both artists | different grouping |
| A link becomes `stale`, or is rejected | invalidates | invalidates | the proof is gone |
| A new `confirmed` link | invalidates | invalidates | the result may differ |
| Album or track title, year, artwork | no | no | presentation |
| `disc_number` / `track_number` | no | no | order only |
| Star, rating, play count | no | no | user state |
| Server unreachable, offline mode | no | no | reachability, not identity |
| Local `is_available` flips | no | no | Decision 5 picks which representation shows |
| Signed out, permission withdrawn | no | no | reachability, not a new proof |

The last three rows matter as much as the first six. A pair whose local file has
gone is still a *proven* pair; what changes is which half can be rendered, which
is Decision 5's business and requires no reconciliation at all.

### The server cursor is a watermark, not a generation

The server's library event stream (its RFC-007) carries a monotonic `cursor`,
and it is the right input — but it is **not** the generation. Comparing a stored
cursor against the server's latest would invalidate the whole library every time
anything anywhere changed, including the star in the table above.

The cursor answers "how far have I observed the server?". The generation answers
"which version of *this entity's* identity inputs have I examined?". So an event
is received, the entities it affects are marked, and only those are reconciled
again.

The stream is well suited to this: a track upsert **carries the track's
`full_hash`**, which the server added precisely because nothing else tells a
client that a file was retagged outside the API. That is the one identity input
a client cannot otherwise observe without re-walking.

**The desktop consumes none of this** — there is no cursor anywhere in
`remote/mirror.rs` — so this RFC's frontier is one more consumer for the work
lot 6 has to do anyway.

Until then the only signal available is `remote_album.song_count`, the mirror's
own freshness check. It detects additions and removals and **cannot detect a
replacement**: a server track whose file was swapped keeps its identifier and
its count, so a `confirmed` link built on the old bytes stays confirmed while
being false. So the approximation is admitted in one direction only:

> `(song_count, mirrored_at)` may **withdraw** eligibility. It may never
> **grant** it. Until an entity's identity inputs have been re-examined —
> today by a reconciliation pass that actually re-read them, later by the
> event stream — the pair is not eligible.

That is strict, and it is the same asymmetry as Decision 5: a pair that is not
yet eligible costs a visible duplicate, and a pair wrongly eligible hides
something on a proof that has quietly stopped being true.

### The corpus is what is observed, not what exists

Artists get the same frontier as albums, with the meaning that makes it
possible: complete does **not** mean "we know this artist's entire discography",
which is unachievable and would pair nobody. It means **every track currently
observed for that artist, on both sides, has been reconciled**.

```
snapshot N        local: A B C      remote: A B D      all reconciled
                  A ↔ A,  B ↔ B,  no contradiction    → eligible

snapshot N+1      a track E appears                    → ineligible until
                                                          reconciled again
```

Without this, artists reproduce the withdrawn rule's failure at smaller scale:
two links arrive and the artist is hidden, a third contradicts them and it comes
back. Decision 5 explains why that behaviour is worse than never hiding it at
all, and the explanation does not stop applying because the entity is an artist.

`remote_track_match_rejection` already records the stronger statement — a
candidate a person examined and refused — and it continues to mean what it
means.

## Decision 4 — a pair is presented as one, not stored as one

A deduplicated pair remains **two rows in two tables**. What changes is that
browses render it **once**, and that the single entry they render is composed
rather than chosen.

**Identity and presentation come from the local half.** Its title, its artwork,
its year, its added-at date. Not because it is more correct, but because it is
the half the user can change: they may have retagged it, and they certainly
picked its cover. A server value that overrode a local edit would make the edit
look broken. Where the local half has nothing — no cover — the server's is shown
instead; that fallback does not exist today and is proposed here, not described.

The added-at date is the local one for the same reason, and one more: an album
the server has held for three years and this machine acquired today belongs at
the top of *recently added*, not buried three years back. A remote-only entry
naturally keeps the server's date; there is no pair to reconcile.

**One pair, one row — and the row is the local one.** With Decision 2's
bijection the two track sets coincide, so a deduplicated album shows each
recording exactly once, rendered from its local row. This is the rule the track
listing already applies, and stating it here removes the reading the earlier
draft invited: nothing produces two rows for one pair, so no deduplicated album
can list a track twice.

**Playback is not decided by the row.** It follows
`remote_track_link.playback_preference` — `local_first` by default,
`server_first` when the user says so — which exists precisely so that which
copy plays is a property of the *pair* rather than of whichever half a listing
happened to render.

**When the local file is gone, the row is the server's.** The pair is still
proven — a missing file is not a disproof — so the entry stays one entry and
takes its identity from the surviving half. Three cases, and none of them
removes the album from the library:

| Local file | Server | The row |
| --- | --- | --- |
| present | either | local, plays per the pair's preference |
| missing | reachable | server, plays from the server |
| missing | unreachable right now | server, and it does not play |

The last is the case worth stating out loud, because the two rules could be read
as fighting: Decision 5 keeps the pair collapsed through a transient outage, and
the local half has filtered itself out. The entry is therefore *shown and not
playable*, which is correct — the music is still in the user's library, and
saying so while failing to play is far better than making the album disappear
because a server did not answer.

**The order is total.** `disc_number`, then `track_number`, then the stable
identifier. There is no local-before-remote tiebreak, because after the two
previous rules there is no pair of rows for it to arbitrate.

**Actions have one meaning, decided here.** "Whichever half the gesture came
from" is not an answer once the user sees one card: they cannot tell which half
they clicked, and they should not have to.

- **Starring applies to both representations that can carry it**, and the entry
  reads as starred when either is. Un-starring clears both. The server half
  travels through the outbound mutation queue like any other remote write, so
  this works offline and lands when the queue drains — and the entry reads from
  the projection, which `remote::write` updates in the *same transaction* as it
  queues the mutation. That is what keeps an offline un-star from looking like
  a button that did nothing until the network returns.
- **Rating and the cover picker write to local rows only.** They have no server
  counterpart in this protocol; nothing is silently dropped because nothing was
  ever offered.
- **Nothing acquires a capability it did not have.** The entry stops being two
  entries; it does not become a third kind of object with powers neither half
  had.

The union-of-presentation is the reason this design does not need to decide
which album is "really" the album. There is no fusion to get wrong, and no
migration that rewrites anyone's library. Turning deduplication off — a
preference, a bug, a future RFC — restores the two entries by changing a query.

## Decision 5 — asymmetric caution, stated as a rule

Getting this wrong in the two directions does not cost the same thing.

A pair left un-deduplicated shows the user two entries for one record. It is
untidy, it is visible, and nothing is lost.

A pair deduplicated wrongly **hides an entity the user has**. Their local album
disappears into a server album that is not it, or the reverse; what they see is
not a duplicate they can reason about but an absence they have no reason to
suspect.

So every rule here fails towards showing too much. Two consequences, and the
first is already shipped:

**Only `confirmed` links count.** A `stale` link is a guess, and hiding on a
guess loses the entity.

**Deduplication never suppresses the last representation of anything.** The
track listing states this today as `is_available = 1` on the local row, which is
the right check for the case it faces and too narrow as a principle. What the
rule means is that a representation may only be hidden while **another one
remains renderable and playable**, and local availability is one of several ways
that can fail: a server track deleted upstream, an account signed out, a
permission withdrawn.

One distinction has to be made explicit or this rule causes the flicker it was
written to prevent: **transient unreachability is not loss.** A server that is
not answering right now, or offline mode being on, must not re-expand every pair
in the library — the representation still exists and will answer again.

But "only actual deletion counts" is too narrow in the other direction, because
some failures are durable without being deletions. The line is whether the
representation can be expected to come back **for this user**:

| Condition | Still hidden? |
| --- | --- |
| Network down, server asleep, offline mode on | Yes — it will answer again |
| Local file missing (`is_available = 0`) | No — the local half already filtered itself out |
| Server track deleted upstream | No |
| Signed out, or the binding forgotten | No |
| Membership or permission withdrawn upstream | No |

The last three are not outages: nothing about waiting fixes them. Treating them
as transient would leave a user looking at a library that quietly lost entries
they still hold locally, which is the exact harm this decision exists to
prevent.

## Decision 6 — a local playlist becomes exactly representable

RFC-005 recorded a loss and its precondition in one paragraph:

> Local playlists no longer travel between machines. That capability existed in
> the v1 design and is genuinely lost. It cannot be recovered without the
> matching layer above, because a local playlist is a list of local files and
> nothing in the protocol can name those on another install.

The matching layer is here, and lot 5 added the missing piece: what has no name
on the server can be given one, by uploading it.

What this RFC decides is narrower than "playlists sync again", and the
distinction matters. It decides **representability**:

> A local playlist whose every entry carries a `confirmed` link can be projected
> into server track identifiers exactly, with no guessing. Publication happens
> only when every entry is representable.

A `confirmed` link is necessary and not sufficient: the same
existence-versus-reach distinction applies. A link naming a server track that
will not resolve — deleted upstream, or on a library this account may no longer
read — is not representable, however confirmed the proof once was. Being unable
to reach the server *right now* is not that, and does not make a playlist
unrepresentable.

**`in_catalogue` does not answer this question, and must not be read as if it
did.** The flag records what the last mirror walk saw. A row can lose it because
the track was cached from user data and never walked; a track deleted upstream
is not flagged at all but *deleted*, by `mirror::drop_vanished`. And
`remote_track_link.remote_track_id` carries no foreign key, so a link can
outlive the row it names — an orphan that still reads as `confirmed`.

Resolvability and authorisation are therefore separate facts a client has to
establish, not flags it can infer:

- **the target still exists** — the link resolves to a catalogue row, or the
  server says so when asked;
- **this account may read it** — a membership or permission the server answers
  for, and whose withdrawal a 404 deliberately does not distinguish from
  absence (RFC-005's own rule);
- **the server is answering** — reachability, which is transient and gates
  nothing here.

A reference that fails the first two is not dropped from the playlist. The local
playlist is unchanged and remains the source of truth; what it loses is
*publishability* until the entry can be named again — which the upload path can
often restore, since a track missing upstream is a track this machine can offer
back.

All or nothing, and never silently. A playlist of forty tracks with three
unlinked ones does not travel as a playlist of thirty-seven: a list missing
three songs is worse than a list that did not sync, because it looks complete.
The three are named, and the offer is to upload them — the operation lot 5
implements, whose commit hands back the track id and digest that write the
missing links. Then the playlist travels whole. The uploads themselves are
resumable and partial; it is the playlist's *visibility* that is transactional.

**What this RFC does not decide** is how a published playlist then behaves:
identity across machines, ordering semantics, duplicates, renames, deletions,
concurrent edits, ownership, and what happens when the server's copy changes.
Those belong to a playlist protocol, not to a deduplication rule, and calling
this "playlists travel again" would promise all of them. It restores the
mapping; it does not solve the sync.

## What this costs

**A derivation that has to be maintained.** The pairing is a function of
`remote_track_link` and of the completeness frontier, and both change — an
import writes a link, a reconciliation pass writes several, deleting a local
track cascades one away, a mirror walk sets `mirrored_at`. Whatever form the
derivation takes, it has exactly one definition, expressed once, the way the
mirror's shared predicate is.

**It should start as a view, not as a materialised table.** The inputs are
links, their status, and availability on both sides; a cache over that has more
invalidation paths than it has query sites, and is the shape most likely to rot.
Both directions of `remote_track_link` are already indexed — `local_track_id` is
the primary key and `remote_track_id` is `UNIQUE` — so the joins have what they
need. Measure, then materialise only if the measurement asks for it.

**Browses get more expensive.** Every listing that ranges over albums or artists
gains a correlated existence check. The track listing already pays it and the
cost is one indexed lookup per row, but the album and artist listings are the
compound ones — the fixture that exercises them describes a select over fourteen
tables — and the measurement belongs to the implementation.

**Eligibility is strict, so early libraries will look un-deduplicated.** Until a
mirror walk completes and a reconciliation pass has hashed the local side,
almost nothing is eligible. That is the intended behaviour under Decision 5 and
it will read as the feature not working. It should be visible in the interface —
"not yet compared" is a different statement from "compared, and different".

## What stays out of scope

- **Grouping editions of one record.** A standard edition and its deluxe are two
  releases that share recordings, and presenting them together is a genuinely
  useful thing this RFC deliberately does not do — because saying "these two are
  the same release" and "these two are close enough to show together" are
  different claims, and the first draft conflated them. A future RFC can
  introduce an explicit album-family concept; it would sit above this one and
  consume the same links.
- **Deduplicating against anything but the bound server.** Two servers, or a
  second profile, are not in this design.
- **Genres, moods, years.** They are labels, not entities.
- **A user-facing merge tool.** The reconciliation surface already lets a person
  confirm or reject a *track* pair, and everything here derives from those. If a
  pair is wrong, the right gesture is on the track it disagrees about.
- **MusicBrainz as a second proof.** RFC-005 allows it as *a suggestion the user
  confirms* for tracks. Nothing here extends it to albums or artists.

## Open questions

- **Whether an ineligible pair should say so.** The cost section argues it
  should; where that surfaces without cluttering a library view is unresolved.
- **How an entity is marked dirty from an event.** Decision 3 enumerates *what*
  advances a generation; mapping each library event onto the albums and artists
  it touches — a track moving between albums touches two of each — is an
  implementation shape this document does not fix.
