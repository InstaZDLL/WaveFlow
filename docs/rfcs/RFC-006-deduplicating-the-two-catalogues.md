# RFC-006 — Deduplicating the two catalogues

- **Status**: Proposed
- **Date**: 2026-08-31
- **Authors**: @InstaZDLL
- **Depends on**: [RFC-005](RFC-005-remote-source-and-sync-v2.md) — this RFC answers the question its Decision 1 deferred, and does not revisit anything else it settled.
- **Implementation**: none yet.

---

## The question this RFC answers

[RFC-005](RFC-005-remote-source-and-sync-v2.md) closed a door and named the RFC
that would reopen it. Twice, in the same words:

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
corruption Decision 1 exists to prevent. Nothing below writes across that line.

**It is not choosing a winner.** "The local album wins" and "the server album
wins" both discard something the user has: local artwork they picked, or server
tracks their local copy does not contain. A pair is not a conflict to resolve.

**It is not a new matching algorithm.** The proof that two things are the same
already exists and is the only one this project accepts — identical bytes. What
follows derives everything from that proof and adds no second way of guessing.

## Decision 1 — deduplication is derived from tracks, never from names

An album is deduplicated because its tracks are, and for no other reason. Same
for an artist. There is no name comparison anywhere in this design.

The temptation is real and worth naming, because the data is sitting right
there: `remote_artist.sort_key` already holds the output of
`name_match::normalize_name`, the same function that produces
`artist.canonical_name` locally. A join on one column would pair most artists in
most libraries, today, with no new table.

It would also be exactly what RFC-005 forbids one level down. Matching tracks by
title, artist and duration is "explicitly forbidden" there; the reasons do not
weaken as the entity grows, they compound. *Greatest Hits* is not one album.
*John Williams* is not one artist — the film composer and the classical
guitarist share a canonical name, and a library holding both would fold them
into one page with no way for its owner to say otherwise. A normalised name is a
good **sort key**, which is what it was introduced for, and it is not evidence.

Deriving from tracks costs a join and nothing else. `track.album_id` names the
local album; `remote_track.album_id` names the server album; a row in
`remote_track_link` sits between them. The same shape gives artists, through
`track.primary_artist` and `remote_track.artist_id`.

## Decision 2 — unanimity, and at least two links

Two entities are the same when **every link that touches either one agrees**,
and there are **at least two of them**.

Both halves earn their place.

**Unanimity**, because a contradiction is the only reliable signal that
something is not what it looks like. If the local album's linked tracks point at
two different server albums, the pairing is not merely uncertain — it is
observably wrong for at least one of them, and neither is worth guessing at.

Unanimity also disposes of the hardest case for free, which is why it is
preferred over any threshold. A local *Various Artists* compilation has tracks
linked to a dozen different server artists; a "majority of links" rule would
have to be taught about compilations to avoid folding VA into whichever artist
happened to win the count. Unanimity never gets there: the second disagreeing
link ends the question. No special case, no `is_compilation` check, nothing to
keep in sync with the album-grouping rules.

**At least two links**, because one is a coincidence waiting to happen. A single
track can legitimately belong to an album, a single, and three compilations; one
link between an album of eleven tracks and an album of nine proves that the two
share a song, which is not the same claim. The exception is the case where there
is nothing else to confuse: when both sides hold exactly one track, one link is
every link there is, and the rule reads as unanimity over a set of size one.

**The threshold is deliberately not a proportion.** "Half the tracks" and
"eighty percent" invite the same objection from opposite directions: an album
whose deluxe edition doubles its track count would fail a proportional test
while being obviously the same record, and a two-track EP would pass one on a
single link. Counting agreements and disagreements needs no denominator.

## Decision 3 — a pair is presented as one, not stored as one

This is what keeps Decision 2's threshold from being dangerous.

A deduplicated pair remains **two rows in two tables**. What changes is that
browses render it **once**, and that the single entry they render is composed
rather than chosen:

- **Identity and presentation come from the local half.** Its title, its
  artwork, its year. Not because it is more correct, but because it is the half
  the user can change: they may have retagged it, and they certainly picked its
  cover. A server value that overrode a local edit would make the edit look
  broken. Where the local half has nothing — no cover — the server's is shown
  instead; that fallback does not exist today and is part of what this RFC
  proposes, not a description of current behaviour.
- **Contents are the union of both halves**, with the rule the track listing
  already applies: a track proven to exist on both sides appears once, from its
  local row, and a track that exists on one side appears with that side's badge.
  So a deluxe edition mirrored on the server shows its eight extra tracks inside
  the album the user has locally, each marked as coming from the server, and
  each playable there.
- **Gestures stay addressed to the half that can carry them.** Rating and the
  cover picker write to local rows; starring the album writes to whichever
  half's favourites the gesture came from. Nothing acquires a capability it did
  not have; the entry simply stops being two entries.

The union is the reason this design does not need to decide which album is
"really" the album. There is no fusion to get wrong, and no migration that
rewrites anyone's library. Turning deduplication off — a preference, a bug, a
future RFC — restores the two entries by changing a query.

## Decision 4 — asymmetric caution, stated as a rule

Getting this wrong in the two directions does not cost the same thing.

A pair left un-deduplicated shows the user two entries for one record. It is
untidy, it is visible, and nothing is lost.

A pair deduplicated wrongly **hides an entity the user has**. Their local album
disappears into a server album that is not it, or the reverse; what they see is
not a duplicate they can reason about but an absence they have no reason to
suspect.

So every rule here fails towards showing too much, and the ones already shipped
say so out loud. The track listing hides a remote row only on a `confirmed`
link — never on a `stale` one, because a stale link is a guess — and only while
the local row is `is_available = 1`, because when the local file is gone the
local half has already filtered itself out and hiding the remote half too would
remove the recording from the library while the server can still play it.

**Those two narrowings are hereby the general rule**, not a detail of one query:
deduplication follows `confirmed` links only, and never removes the last
reachable representation of anything.

## Decision 5 — local playlists travel again

RFC-005 recorded a loss and its precondition in one paragraph:

> Local playlists no longer travel between machines. That capability existed in
> the v1 design and is genuinely lost. It cannot be recovered without the
> matching layer above, because a local playlist is a list of local files and
> nothing in the protocol can name those on another install.

The matching layer is here, and lot 5 added the missing piece: what has no name
on the server can be given one, by uploading it. A local playlist whose every
track carries a `confirmed` link is expressible in server identifiers, exactly,
with no guessing — which is the standard RFC-005 set.

The rule is **all or nothing, and never silently**. A playlist of forty tracks
with three unlinked ones does not travel as a playlist of thirty-seven: a list
missing three songs is worse than a list that did not sync, because it looks
complete. The three are named, and the offer is to upload them — the operation
lot 5 already implements, whose commit hands back the track id and digest that
write the missing links. Then the playlist travels whole.

This is the first place where the seven lots visibly compound rather than merely
follow one another, and it is the reason this RFC waited for lot 5 rather than
being written after lot 4 as originally planned.

## What this costs

**A derivation that has to be maintained.** The pairing is a function of
`remote_track_link`, and that table changes — an import writes a row, a
reconciliation pass writes several, deleting a local track cascades one away.
Whether the derivation is a view, a materialised table refreshed on those
events, or a join inside each browse is an implementation question this RFC does
not settle; what it does settle is that the derivation has exactly one
definition, expressed once, the way the mirror's shared predicate is.

**Browses get more expensive.** Every listing that ranges over albums or artists
gains a correlated existence check. The track listing already pays it and the
cost is one indexed lookup per row, but the album and artist listings are the
compound ones — the fixture that exercises them describes a select over fourteen
tables — and the measurement belongs to the implementation rather than to this
document.

**Partial pairs will look odd before they look right.** A library mirrored
halfway — the walk is resumable and long — holds albums whose tracks are only
partly mirrored, so pairs will form as the mirror progresses. That is correct
behaviour and it will read as flicker. The `mirrored_at` column already
distinguishes a walked album from an un-walked one; whether deduplication should
wait for a complete walk is left open below.

## What stays out of scope

- **Deduplicating against anything but the bound server.** Two servers, or a
  second profile, are not in this design.
- **Genres, moods, years.** They are labels, not entities; they were never
  doubled in a way anyone noticed.
- **A user-facing merge tool.** The reconciliation surface already lets a person
  confirm or reject a *track* pair, and everything here derives from those. If
  an album pair needs overriding by hand, the right gesture is on the track it
  disagrees about.
- **MusicBrainz as a second proof.** RFC-005 allows it as *a suggestion the user
  confirms* for tracks. Nothing here extends it to albums or artists, and doing
  so later is a change to the link layer, not to this one.

## Open questions

- **Does deduplication wait for a complete mirror walk?** Suppressing pairs
  until `remote_album.mirrored_at` is set would trade early correctness for
  fewer visible transitions. Both defensible; unmeasured.
- **Where the derivation lives** — view, materialised table, or per-query join —
  and what that costs on a library of tens of thousands of albums.
- **Whether a deduplicated album's "recently added" date is the local one or the
  earlier of the two.** The local one is simpler; the earlier one is arguably
  what the user means by when they got the record.
