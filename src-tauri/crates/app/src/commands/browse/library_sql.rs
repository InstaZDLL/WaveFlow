//! The compound selects behind the library's album and artist lists, and
//! the RFC-006 predicates that decide when a server entry pairs with a
//! local one.

/// A local track whose bytes have been read and recorded — RFC-006's
/// "examined" side of the completeness frontier.
///
/// Without it, "no link" is ambiguous: it means either "different bytes" or
/// "nobody has looked yet", and pairing on the second is how a rule hides
/// something it has no evidence about. `local_full_hash` answers the
/// difference, and only while its entry still describes the file on disk.
macro_rules! track_examined {
    ($track:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM local_full_hash h
                      WHERE h.track_id = ",
            $track,
            ".id
                        AND h.file_size = ",
            $track,
            ".file_size
                        AND h.file_modified = ",
            $track,
            ".file_modified)"
        )
    };
}

/// A server album that is the same release as a local one, and may therefore
/// be rendered once rather than twice — [RFC-006](../../../../docs/rfcs/RFC-006-deduplicating-the-two-catalogues.md)
/// decisions 2 and 3.
///
/// A **complete, non-empty bijection** over examined sets: every track on each
/// side confirmed-linked to one on the other, and at least one such link. The
/// one-to-one part is free — `remote_track_link` is a primary key on the local
/// id and `UNIQUE` on the remote one — so "each track has a partner" already
/// means "exactly one".
///
/// Two links and unanimity would **not** do here, which is what an earlier
/// draft of that RFC got wrong: a compilation sharing two recordings with an
/// album satisfies it while being a different release. An album is a closed
/// set, so the set is the evidence.
///
/// The emptiness guard is not decoration either: a bijection over two empty
/// sets holds vacuously, so without it a local album would pair with a server
/// walk that returned nothing.
macro_rules! album_pair_proven {
    () => {
        concat!(
            "ra.mirrored_at IS NOT NULL AND EXISTS (
           SELECT 1 FROM (
                  -- The only albums that can possibly pair are those a
                  -- confirmed link already reaches from this one. Ranging over
                  -- every local album instead would be one scan of the local
                  -- catalogue per server album, answering `false` for all but
                  -- a handful. It also subsumes the non-empty requirement: a
                  -- candidate exists only because a link does.
                  SELECT DISTINCT t.album_id AS id
                    FROM remote_track rt
                    JOIN remote_track_link l
                      ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                    JOIN track t ON t.id = l.local_track_id
                   WHERE rt.album_id = ra.remote_id AND rt.in_catalogue = 1
                     AND t.is_available = 1 AND t.album_id IS NOT NULL
                ) al
            WHERE NOT EXISTS (
                    SELECT 1 FROM remote_track rt
                     WHERE rt.album_id = ra.remote_id AND rt.in_catalogue = 1
                       AND NOT EXISTS (
                             SELECT 1 FROM remote_track_link l
                               JOIN track t ON t.id = l.local_track_id
                              WHERE l.remote_track_id = rt.remote_id
                                AND l.status = 'confirmed'
                                AND t.album_id = al.id AND t.is_available = 1))
              AND NOT EXISTS (
                    SELECT 1 FROM track t
                     WHERE t.album_id = al.id AND t.is_available = 1
                       AND (NOT EXISTS (
                              SELECT 1 FROM remote_track_link l
                                JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                               WHERE l.local_track_id = t.id AND l.status = 'confirmed'
                                AND rt.album_id = ra.remote_id AND rt.in_catalogue = 1)
                            OR NOT ",
            track_examined!("t"),
            "))
         )"
        )
    };
}

/// A server artist that is the same person as a local one — RFC-006 decision 2,
/// and deliberately **not** the album rule.
///
/// An artist is an open grouping: nobody's discography is complete on either
/// side, so demanding a bijection would pair nobody. The evidence is about the
/// person, not the extent of their work, so a sample suffices — **unanimity,
/// and at least two confirmed links**.
///
/// Unanimity disposes of *Various Artists* with no special case: a compilation
/// has tracks linked to a dozen different server artists, so the second
/// disagreeing link ends the question. Two links because one guest appearance,
/// credited to the featured artist on one side and the host on the other, is a
/// coincidence rather than evidence.
///
/// Both sides of the disagreement are checked. Looking only from the server
/// side would fold a local *Various Artists* into a real artist whenever the
/// compilation's own tracks happened to agree.
///
/// ## Why `primary_artist`, when the listing joins `track_artist`
///
/// The listing shows a local artist credited anywhere on a track; this
/// predicate considers only the *primary* one, and the two are deliberately
/// not the same set.
///
/// The mirror holds exactly one artist per server track — `remote_track` has
/// an `artist_id` column and there is no remote participants table — so
/// `primary_artist` is the only local column comparable to it. Joining
/// `track_artist` instead would make every guest credit look like a
/// disagreement: a featured artist would "own" a track the server attributes
/// to the host, and the unanimity check would fire against an artist the
/// server never mentioned.
///
/// The cost is a known false negative: an artist never credited as primary is
/// never paired, so they appear twice. That is the direction this design
/// always fails in — a visible duplicate rather than a hidden entity — and it
/// resolves on its own if the mirror ever carries participants.
macro_rules! artist_pair_proven {
    () => {
        concat!(
            "EXISTS (
           SELECT 1 FROM (
                  -- Same candidate narrowing as the albums': only an artist a
                  -- confirmed link already reaches can pair.
                  SELECT DISTINCT t.primary_artist AS id
                    FROM remote_track rt
                    JOIN remote_track_link l
                      ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                    JOIN track t ON t.id = l.local_track_id
                   WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1
                     AND t.is_available = 1 AND t.primary_artist IS NOT NULL
                ) la
            WHERE (SELECT COUNT(*)
                     FROM remote_track rt
                     JOIN remote_track_link l
                       ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                     JOIN track t ON t.id = l.local_track_id
                    WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1
                      AND t.primary_artist = la.id AND t.is_available = 1
                      AND ",
            track_examined!("t"),
            ") >= 2
              AND NOT EXISTS (
                    SELECT 1 FROM remote_track rt
                      JOIN remote_track_link l
                        ON l.remote_track_id = rt.remote_id AND l.status = 'confirmed'
                      JOIN track t ON t.id = l.local_track_id
                     WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1
                       AND t.is_available = 1
                       AND (t.primary_artist IS NULL OR t.primary_artist != la.id))
              AND NOT EXISTS (
                    SELECT 1 FROM track t
                      JOIN remote_track_link l
                        ON l.local_track_id = t.id AND l.status = 'confirmed'
                      JOIN remote_track rt ON rt.remote_id = l.remote_track_id
                     WHERE t.primary_artist = la.id AND t.is_available = 1
                       AND rt.in_catalogue = 1
                       AND (rt.artist_id IS NULL OR rt.artist_id != ra.remote_id))
         )"
        )
    };
}

/// Both halves of the album listing, as one compound select.
///
/// Split out of the command so the SQL can be exercised on its own: the
/// command needs an `AppState`, the query needs only a database, and the query
/// is the part that can be wrong.
pub(super) fn library_albums_sql(order_clause: &str) -> String {
    let album_pair = album_pair_proven!();
    format!(
        r#"
        SELECT source, id, title, artist_name, year, track_count, total_duration_ms,
               artwork_hash, artwork_format, max_bit_depth, max_sample_rate
          FROM (
            SELECT 'local'                             AS source,
                   CAST(al.id AS TEXT)                 AS id,
                   al.title                            AS title,
                   COALESCE(ar.name, al.album_artist)  AS artist_name,
                   al.year                             AS year,
                   COUNT(t.id)                         AS track_count,
                   COALESCE(SUM(t.duration_ms), 0)     AS total_duration_ms,
                   aw.hash                             AS artwork_hash,
                   aw.format                           AS artwork_format,
                   MAX(t.bit_depth)                    AS max_bit_depth,
                   MAX(t.sample_rate)                  AS max_sample_rate,
                   al.canonical_title                  AS sort_title,
                   COALESCE(ar.canonical_name, al.album_artist) AS sort_artist,
                   MIN(t.added_at)                     AS added_at
              FROM album al
              JOIN track t         ON t.album_id = al.id
              LEFT JOIN artist ar  ON ar.id = al.artist_id
              LEFT JOIN artwork aw ON aw.id = al.artwork_id
             WHERE (? IS NULL OR t.library_id = ?)
               AND t.is_available = 1
             GROUP BY al.id
            UNION ALL
            SELECT 'remote',
                   ra.remote_id,
                   ra.title,
                   ra.artist,
                   ra.year,
                   ra.song_count,
                   ra.duration_ms,
                   ra.artwork_hash,
                   NULL,
                   NULL,
                   NULL,
                   COALESCE(ra.sort_title, ra.title),
                   COALESCE(ra.sort_artist, ra.artist),
                   ra.created_at
              FROM remote_album ra
             -- A local library filter is a filter over *local* libraries; a
             -- server album belongs to none of them. Keeping the remote half
             -- visible while the user has narrowed to one local library reads
             -- as the filter having failed.
             WHERE ? IS NULL
             -- One release, one entry: a server album proven to be the same
             -- release as a local one is rendered from the local half rather
             -- than beside it. See `album_pair_proven!`.
               AND NOT ({album_pair})
          )
         WHERE (? IS NULL OR source = ?)
         {order_clause}
"#
    )
}

/// Both halves of the artist listing, as one compound select.
///
/// Split out of the command so the SQL can be exercised on its own: the
/// command needs an `AppState`, the query needs only a database, and the query
/// is the part that can be wrong.
pub(super) fn library_artists_sql(order_clause: &str) -> String {
    let artist_pair = artist_pair_proven!();
    format!(
        r#"
        SELECT source, id, name, track_count, album_count,
               artwork_hash, artwork_format, picture_url, picture_hash
          FROM (
            SELECT 'local'                     AS source,
                   CAST(ar.id AS TEXT)         AS id,
                   ar.name                     AS name,
                   COUNT(DISTINCT t.id)        AS track_count,
                   COUNT(DISTINCT t.album_id)  AS album_count,
                   aw.hash                     AS artwork_hash,
                   aw.format                   AS artwork_format,
                   da.picture_url              AS picture_url,
                   da.picture_hash             AS picture_hash,
                   ar.canonical_name           AS sort_name
              FROM artist ar
              JOIN track_artist ta ON ta.artist_id = ar.id
              JOIN track t         ON t.id = ta.track_id
              LEFT JOIN artwork aw ON aw.id = ar.artwork_id
              LEFT JOIN app.metadata_artist da ON da.deezer_id = ar.deezer_id
             WHERE (? IS NULL OR t.library_id = ?)
               AND t.is_available = 1
             GROUP BY ar.id
            UNION ALL
            SELECT 'remote',
                   ra.remote_id,
                   ra.name,
                   (SELECT count(*) FROM remote_track rt
                     WHERE rt.artist_id = ra.remote_id AND rt.in_catalogue = 1),
                   (SELECT count(*) FROM remote_album al
                     WHERE al.artist_id = ra.remote_id),
                   ra.artwork_hash,
                   NULL,
                   NULL,
                   NULL,
                   COALESCE(ra.sort_key, ra.name)
              FROM remote_artist ra
             -- A local library filter is a filter over local libraries; see
             -- `list_library_albums`.
             WHERE ? IS NULL
             -- One person, one entry. See `artist_pair_proven!` — a different
             -- rule from the albums', because an artist is an open grouping.
               AND NOT ({artist_pair})
          )
         WHERE (? IS NULL OR source = ?)
         {order_clause}
"#
    )
}
