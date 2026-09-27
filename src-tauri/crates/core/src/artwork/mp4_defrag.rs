//! Rewrite a fragmented MP4 as an ordinary one, without touching a frame.
//!
//! Apple's motion covers are fragmented: an empty sample table in `moov`,
//! `mvex` announcing that the media follows, then `moof`/`mdat` pairs, each
//! `moof` indexing the frames of the `mdat` after it. WebKitGTK cannot
//! stream that from a file: it pauses the download a moment into playback
//! and never resumes it, so the clip stops, or restarts, after a couple of
//! seconds. The same frames laid out as an ordinary MP4 — one `moov`
//! indexing everything, then one `mdat` — play through.
//!
//! [`defragment`] does that conversion. It copies the sample descriptions
//! and every frame byte for byte and only rebuilds the index: each `trun`
//! becomes one chunk, so the frames keep their order and interleaving. The
//! first sample of each track is moved to time zero — an HEVC cover can
//! start ten seconds into its own timeline, which a player would otherwise
//! show as ten seconds of nothing — and an edit list is written only where
//! composition offsets delay the first frame.

use thiserror::Error;

/// Why a file could not be rewritten.
#[derive(Debug, Error)]
pub enum DefragError {
    #[error("malformed mp4: {0}")]
    Malformed(&'static str),
    #[error("unsupported mp4: {0}")]
    Unsupported(&'static str),
}

type Result<T> = std::result::Result<T, DefragError>;

/// Whether the start of a file shows it to be fragmented: `mvex` in a
/// `moov` that fits in `head`, or a `moof` before the head runs out. A
/// `moov` too large for `head` reads as not fragmented; a fragmented
/// file's `moov` is a few hundred bytes, since it indexes nothing.
pub fn looks_fragmented(head: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos + 8 <= head.len() {
        let mut c = Cursor::new(&head[pos..]);
        let Ok(size32) = c.u32() else { return false };
        let kind = &head[pos + 4..pos + 8];
        let (size, header) = match size32 {
            1 => match c.take(4).and_then(|_| c.u64()) {
                Ok(n) => (n, 16u64),
                Err(_) => return false,
            },
            0 => return kind == b"moof",
            n => (u64::from(n), 8u64),
        };
        if size < header {
            return false;
        }
        match kind {
            b"moof" => return true,
            b"moov" => {
                let end = pos as u64 + size;
                if end > head.len() as u64 {
                    return false;
                }
                let body = &head[pos + header as usize..end as usize];
                return parse_boxes(body, 0).is_ok_and(|kids| child(&kids, b"mvex").is_some());
            }
            _ => {}
        }
        match usize::try_from(pos as u64 + size) {
            Ok(next) => pos = next,
            Err(_) => return false,
        }
    }
    false
}

/// `Ok(None)` when `input` is not fragmented (nothing to do), otherwise
/// the same media as an ordinary MP4 with its index first.
pub fn defragment(input: &[u8]) -> Result<Option<Vec<u8>>> {
    let top = parse_boxes(input, 0)?;
    let moov = top
        .iter()
        .find(|b| &b.kind == b"moov")
        .ok_or(DefragError::Malformed("no moov box"))?;
    let moov_children = parse_boxes(moov.body, moov.body_start)?;
    let fragmented =
        moov_children.iter().any(|b| &b.kind == b"mvex") || top.iter().any(|b| &b.kind == b"moof");
    if !fragmented {
        return Ok(None);
    }

    let mut tracks = read_tracks(&moov_children)?;
    for moof in top.iter().filter(|b| &b.kind == b"moof") {
        read_moof(input, moof, &mut tracks)?;
    }
    if tracks.iter().all(|t| t.samples.is_empty()) {
        return Err(DefragError::Malformed("no samples in any fragment"));
    }
    write_progressive(input, &moov_children, &tracks).map(Some)
}

// ----- reading ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct BoxRef<'a> {
    kind: [u8; 4],
    /// Absolute offset of the box header in the input.
    start: usize,
    /// Absolute offset of the body.
    body_start: usize,
    body: &'a [u8],
}

/// The boxes laid end to end in `data`, which starts at absolute offset
/// `base` of the input.
fn parse_boxes(data: &[u8], base: usize) -> Result<Vec<BoxRef<'_>>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let mut cur = Cursor::new(&data[pos..]);
        let size32 = cur.u32()?;
        let mut kind = [0u8; 4];
        kind.copy_from_slice(cur.take(4)?);
        let (size, header) = match size32 {
            0 => ((data.len() - pos) as u64, 8usize),
            1 => (cur.u64()?, 16usize),
            n => (u64::from(n), 8usize),
        };
        let size = usize::try_from(size).map_err(|_| DefragError::Malformed("box too large"))?;
        if size < header || size > data.len() - pos {
            return Err(DefragError::Malformed("box overruns its parent"));
        }
        out.push(BoxRef {
            kind,
            start: base + pos,
            body_start: base + pos + header,
            body: &data[pos + header..pos + size],
        });
        pos += size;
    }
    Ok(out)
}

fn child<'a>(boxes: &[BoxRef<'a>], kind: &[u8; 4]) -> Option<BoxRef<'a>> {
    boxes.iter().find(|b| &b.kind == kind).copied()
}

fn children<'a>(parent: &BoxRef<'a>) -> Result<Vec<BoxRef<'a>>> {
    parse_boxes(parent.body, parent.body_start)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or(DefragError::Malformed("box shorter than its fields"))?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }
    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

#[derive(Clone, Copy, Default)]
struct Defaults {
    description_index: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

struct Sample {
    duration: u32,
    size: u32,
    sync: bool,
    /// Composition offset: presentation time minus decode time.
    cts: i64,
}

struct Chunk {
    /// Absolute offset of the chunk's bytes in the input.
    src: usize,
    len: usize,
    samples: u32,
    description_index: u32,
}

struct Track<'a> {
    id: u32,
    trak: BoxRef<'a>,
    /// First non-empty edit's media time, in the track's own timeline.
    edit_start: Option<i64>,
    trex: Defaults,
    samples: Vec<Sample>,
    chunks: Vec<Chunk>,
    /// Decode time of the first sample, the origin the output moves to zero.
    first_dts: Option<u64>,
    /// Decode time just past the last sample read so far.
    next_dts: u64,
}

const SAMPLE_IS_NON_SYNC: u32 = 0x0001_0000;

fn read_tracks<'a>(moov: &[BoxRef<'a>]) -> Result<Vec<Track<'a>>> {
    let mut trex = Vec::new();
    if let Some(mvex) = child(moov, b"mvex") {
        for b in children(&mvex)?.iter().filter(|b| &b.kind == b"trex") {
            let mut c = Cursor::new(b.body);
            c.u32()?; // version + flags
            let id = c.u32()?;
            let defaults = Defaults {
                description_index: c.u32()?,
                duration: c.u32()?,
                size: c.u32()?,
                flags: c.u32()?,
            };
            trex.push((id, defaults));
        }
    }

    let mut tracks = Vec::new();
    for trak in moov.iter().filter(|b| &b.kind == b"trak") {
        let kids = children(trak)?;
        let tkhd = child(&kids, b"tkhd").ok_or(DefragError::Malformed("trak without tkhd"))?;
        let mut c = Cursor::new(tkhd.body);
        let version = c.u32()? >> 24;
        c.take(if version == 1 { 16 } else { 8 })?;
        let id = c.u32()?;
        let edit_start = match child(&kids, b"edts") {
            Some(edts) => first_edit_start(&children(&edts)?)?,
            None => None,
        };
        let defaults = trex
            .iter()
            .find(|(tid, _)| *tid == id)
            .map(|(_, d)| *d)
            .unwrap_or(Defaults {
                description_index: 1,
                ..Defaults::default()
            });
        tracks.push(Track {
            id,
            trak: *trak,
            edit_start,
            trex: defaults,
            samples: Vec::new(),
            chunks: Vec::new(),
            first_dts: None,
            next_dts: 0,
        });
    }
    Ok(tracks)
}

fn first_edit_start(edts: &[BoxRef]) -> Result<Option<i64>> {
    let Some(elst) = child(edts, b"elst") else {
        return Ok(None);
    };
    let mut c = Cursor::new(elst.body);
    let version = c.u32()? >> 24;
    let count = c.u32()?;
    for _ in 0..count {
        let media_time = if version == 1 {
            c.u64()?;
            c.u64()? as i64
        } else {
            c.u32()?;
            i64::from(c.u32()? as i32)
        };
        c.u32()?; // rate
        if media_time >= 0 {
            return Ok(Some(media_time));
        }
    }
    Ok(None)
}

fn read_moof(input: &[u8], moof: &BoxRef, tracks: &mut [Track]) -> Result<()> {
    // Without an explicit base, a traf's data follows the previous one's.
    let mut previous_end = moof.start;
    for (index, traf) in children(moof)?
        .iter()
        .filter(|b| &b.kind == b"traf")
        .enumerate()
    {
        let kids = children(traf)?;
        let tfhd = child(&kids, b"tfhd").ok_or(DefragError::Malformed("traf without tfhd"))?;
        let mut c = Cursor::new(tfhd.body);
        let flags = c.u32()? & 0x00ff_ffff;
        let id = c.u32()?;
        let track = tracks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(DefragError::Malformed("fragment for an unknown track"))?;
        let mut d = track.trex;
        let base = if flags & 0x01 != 0 {
            usize::try_from(c.u64()?).map_err(|_| DefragError::Malformed("base offset"))?
        } else if flags & 0x02_0000 != 0 || index == 0 {
            moof.start
        } else {
            previous_end
        };
        if flags & 0x02 != 0 {
            d.description_index = c.u32()?;
        }
        if flags & 0x08 != 0 {
            d.duration = c.u32()?;
        }
        if flags & 0x10 != 0 {
            d.size = c.u32()?;
        }
        if flags & 0x20 != 0 {
            d.flags = c.u32()?;
        }

        if let Some(tfdt) = child(&kids, b"tfdt") {
            let mut c = Cursor::new(tfdt.body);
            let version = c.u32()? >> 24;
            let dts = if version == 1 {
                c.u64()?
            } else {
                u64::from(c.u32()?)
            };
            match track.first_dts {
                None => {
                    track.first_dts = Some(dts);
                    track.next_dts = dts;
                }
                // A gap between fragments: the sample table has no holes,
                // so the last sample before it lasts until the next one.
                Some(_) if dts > track.next_dts => {
                    if let Some(last) = track.samples.last_mut() {
                        let gap = u32::try_from(dts - track.next_dts)
                            .map_err(|_| DefragError::Unsupported("gap between fragments"))?;
                        last.duration = last.duration.saturating_add(gap);
                    }
                    track.next_dts = dts;
                }
                Some(_) => {}
            }
        }
        track.first_dts.get_or_insert(track.next_dts);

        let mut data_pos = base;
        for trun in kids.iter().filter(|b| &b.kind == b"trun") {
            data_pos = read_trun(input, trun, base, data_pos, d, track)?;
        }
        previous_end = data_pos;
    }
    Ok(())
}

/// Read one `trun` into `track` as one chunk; returns where its data ends.
fn read_trun(
    input: &[u8],
    trun: &BoxRef,
    base: usize,
    data_pos: usize,
    d: Defaults,
    track: &mut Track,
) -> Result<usize> {
    let mut c = Cursor::new(trun.body);
    let head = c.u32()?;
    let version = head >> 24;
    let flags = head & 0x00ff_ffff;
    let count = c.u32()?;
    let start = if flags & 0x01 != 0 {
        let offset = i64::from(c.u32()? as i32);
        usize::try_from(base as i64 + offset).map_err(|_| DefragError::Malformed("data offset"))?
    } else {
        data_pos
    };
    let first_flags = if flags & 0x04 != 0 {
        Some(c.u32()?)
    } else {
        None
    };
    // A count the file cannot back is rejected before anything is reserved
    // for it: each per-sample field takes 4 bytes of this box, and each
    // sample at least a byte of the file (a size of zero is refused).
    let per_sample = [0x100, 0x200, 0x400, 0x800]
        .iter()
        .filter(|&&f| flags & f != 0)
        .count()
        * 4;
    let fits_box = per_sample == 0 || c.rest().len() / per_sample >= count as usize;
    let fits_file = input.len().saturating_sub(start) >= count as usize;
    if !fits_box || !fits_file || (flags & 0x200 == 0 && d.size == 0 && count > 0) {
        return Err(DefragError::Malformed("trun sample count"));
    }
    track.samples.reserve(count as usize);

    let mut len = 0usize;
    for i in 0..count {
        let duration = if flags & 0x100 != 0 {
            c.u32()?
        } else {
            d.duration
        };
        let size = if flags & 0x200 != 0 { c.u32()? } else { d.size };
        let mut sample_flags = if flags & 0x400 != 0 {
            c.u32()?
        } else {
            d.flags
        };
        if i == 0 {
            sample_flags = first_flags.unwrap_or(sample_flags);
        }
        let cts = if flags & 0x800 != 0 {
            let raw = c.u32()?;
            if version == 0 {
                i64::from(raw)
            } else {
                i64::from(raw as i32)
            }
        } else {
            0
        };
        len = len
            .checked_add(size as usize)
            .ok_or(DefragError::Malformed("sample sizes"))?;
        track.next_dts = track.next_dts.saturating_add(u64::from(duration));
        track.samples.push(Sample {
            duration,
            size,
            sync: sample_flags & SAMPLE_IS_NON_SYNC == 0,
            cts,
        });
    }
    let end = start
        .checked_add(len)
        .filter(|&end| end <= input.len())
        .ok_or(DefragError::Malformed(
            "samples run past the end of the file",
        ))?;
    if count > 0 {
        track.chunks.push(Chunk {
            src: start,
            len,
            samples: count,
            description_index: d.description_index,
        });
    }
    Ok(end)
}

// ----- writing ---------------------------------------------------------------

/// Append a box of `kind` whose body `body` writes.
fn put_box(
    out: &mut Vec<u8>,
    kind: &[u8; 4],
    body: impl FnOnce(&mut Vec<u8>) -> Result<()>,
) -> Result<()> {
    let at = out.len();
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(kind);
    body(out)?;
    let size = u32::try_from(out.len() - at)
        .map_err(|_| DefragError::Unsupported("index box over 4 GB"))?;
    out[at..at + 4].copy_from_slice(&size.to_be_bytes());
    Ok(())
}

/// Append a full box: version 0, no flags, then `body`.
fn put_full_box(out: &mut Vec<u8>, kind: &[u8; 4], body: impl FnOnce(&mut Vec<u8>)) -> Result<()> {
    put_box(out, kind, |o| {
        o.extend_from_slice(&0u32.to_be_bytes());
        body(o);
        Ok(())
    })
}

/// Copy a box from the input unchanged.
fn put_raw(out: &mut Vec<u8>, input: &[u8], b: &BoxRef) {
    out.extend_from_slice(&input[b.start..b.body_start + b.body.len()]);
}

fn put_u32s(o: &mut Vec<u8>, values: &[u32]) {
    for v in values {
        o.extend_from_slice(&v.to_be_bytes());
    }
}

fn scale(value: u64, from: u32, to: u32) -> u64 {
    if from == 0 {
        return 0;
    }
    u64::try_from(u128::from(value) * u128::from(to) / u128::from(from)).unwrap_or(u64::MAX)
}

/// `(count, value)` runs of consecutive equal values.
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// An `mvhd`, `tkhd` or `mdhd` body split around its duration, whichever
/// version it was written in.
#[derive(Clone, Copy)]
struct Header<'a> {
    flags: u32,
    created: u64,
    modified: u64,
    /// Between the timestamps and the duration: the timescale (`mvhd`,
    /// `mdhd`), or the track id and a reserved word (`tkhd`).
    middle: &'a [u8],
    tail: &'a [u8],
}

impl Header<'_> {
    /// The timescale, for an `mvhd` or `mdhd`.
    fn timescale(&self) -> u32 {
        u32::from_be_bytes([
            self.middle[0],
            self.middle[1],
            self.middle[2],
            self.middle[3],
        ])
    }
}

/// `middle_len` is 4 for `mvhd` and `mdhd`, 8 for `tkhd`.
fn read_header(body: &[u8], middle_len: usize) -> Result<Header<'_>> {
    let mut c = Cursor::new(body);
    let head = c.u32()?;
    let long = head >> 24 == 1;
    let (created, modified) = if long {
        (c.u64()?, c.u64()?)
    } else {
        (u64::from(c.u32()?), u64::from(c.u32()?))
    };
    let middle = c.take(middle_len)?;
    c.take(if long { 8 } else { 4 })?;
    Ok(Header {
        flags: head & 0x00ff_ffff,
        created,
        modified,
        middle,
        tail: c.rest(),
    })
}

/// Write `h` back as version 1, with a new duration.
fn put_header(o: &mut Vec<u8>, kind: &[u8; 4], h: &Header, duration: u64) -> Result<()> {
    put_box(o, kind, |o| {
        o.extend_from_slice(&((1u32 << 24) | h.flags).to_be_bytes());
        o.extend_from_slice(&h.created.to_be_bytes());
        o.extend_from_slice(&h.modified.to_be_bytes());
        o.extend_from_slice(h.middle);
        o.extend_from_slice(&duration.to_be_bytes());
        o.extend_from_slice(h.tail);
        Ok(())
    })
}

/// Per-track values the output needs before any box is written.
struct Plan {
    media_timescale: u32,
    media_duration: u64,
    /// Added to every composition offset so none is negative.
    cts_shift: i64,
    /// Media time the presentation starts at; an edit list when non-zero.
    start: i64,
}

impl Plan {
    fn presented(&self) -> u64 {
        self.media_duration
            .saturating_sub(u64::try_from(self.start).unwrap_or(0))
    }
}

fn plan(track: &Track) -> Result<Plan> {
    let mdia = child(&children(&track.trak)?, b"mdia")
        .ok_or(DefragError::Malformed("trak without mdia"))?;
    let mdhd =
        child(&children(&mdia)?, b"mdhd").ok_or(DefragError::Malformed("mdia without mdhd"))?;
    let media_timescale = read_header(mdhd.body, 4)?.timescale();

    let media_duration = track.samples.iter().map(|s| u64::from(s.duration)).sum();
    let cts_shift = (-track.samples.iter().map(|s| s.cts).min().unwrap_or(0)).max(0);
    let mut dts = 0i64;
    let mut min_pts: Option<i64> = None;
    for s in &track.samples {
        let pts = dts + s.cts + cts_shift;
        min_pts = Some(min_pts.map_or(pts, |m| m.min(pts)));
        dts += i64::from(s.duration);
    }
    // An edit into the source timeline moves with it. One that pointed
    // before the first sample — into the empty stretch this conversion
    // drops — falls back to the first frame shown.
    let first = i64::try_from(track.first_dts.unwrap_or(0)).unwrap_or(i64::MAX);
    let start = match track.edit_start {
        Some(m) if m >= first => m - first + cts_shift,
        _ => min_pts.unwrap_or(0),
    };
    Ok(Plan {
        media_timescale,
        media_duration,
        cts_shift,
        start,
    })
}

struct Layout<'a> {
    input: &'a [u8],
    moov: &'a [BoxRef<'a>],
    tracks: &'a [Track<'a>],
    plans: Vec<Plan>,
    movie: Header<'a>,
    movie_duration: u64,
    /// `(track, chunk)` in source order, which keeps tracks interleaved.
    order: Vec<(usize, usize)>,
}

impl Layout<'_> {
    /// The `moov` box, with chunk offsets counted from `media_origin`.
    fn moov(&self, media_origin: u64, co64: bool) -> Result<Vec<u8>> {
        let mut offsets: Vec<Vec<u64>> = self
            .tracks
            .iter()
            .map(|t| vec![0; t.chunks.len()])
            .collect();
        let mut at = media_origin;
        for &(t, c) in &self.order {
            offsets[t][c] = at;
            at += self.tracks[t].chunks[c].len as u64;
        }
        let next_track_id = self
            .tracks
            .iter()
            .map(|t| t.id.saturating_add(1))
            .max()
            .unwrap_or(1);

        let mut out = Vec::new();
        put_box(&mut out, b"moov", |o| {
            // next_track_ID is the last word of mvhd's 80-byte tail.
            let mut tail = self.movie.tail.to_vec();
            if tail.len() < 80 {
                return Err(DefragError::Malformed("mvhd too short"));
            }
            tail[76..80].copy_from_slice(&next_track_id.to_be_bytes());
            let movie = Header {
                tail: &tail,
                ..self.movie
            };
            put_header(o, b"mvhd", &movie, self.movie_duration)?;
            for b in self.moov {
                match &b.kind {
                    // Without mvex the file is no longer fragmented.
                    b"mvhd" | b"mvex" | b"trak" => {}
                    _ => put_raw(o, self.input, b),
                }
            }
            for (i, track) in self.tracks.iter().enumerate() {
                self.trak(o, track, &self.plans[i], &offsets[i], co64)?;
            }
            Ok(())
        })?;
        Ok(out)
    }

    fn trak(
        &self,
        o: &mut Vec<u8>,
        track: &Track,
        p: &Plan,
        offsets: &[u64],
        co64: bool,
    ) -> Result<()> {
        let duration = scale(p.presented(), p.media_timescale, self.movie.timescale());
        put_box(o, b"trak", |o| {
            for b in children(&track.trak)? {
                match &b.kind {
                    b"tkhd" => {
                        put_header(o, b"tkhd", &read_header(b.body, 8)?, duration)?;
                        if p.start > 0 {
                            put_box(o, b"edts", |o| {
                                put_box(o, b"elst", |o| {
                                    put_u32s(o, &[1 << 24, 1]);
                                    o.extend_from_slice(&duration.to_be_bytes());
                                    o.extend_from_slice(&p.start.to_be_bytes());
                                    put_u32s(o, &[0x0001_0000]);
                                    Ok(())
                                })
                            })?;
                        }
                    }
                    // Rewritten above, against the rebased timeline.
                    b"edts" => {}
                    b"mdia" => self.mdia(o, &b, track, p, offsets, co64)?,
                    _ => put_raw(o, self.input, &b),
                }
            }
            Ok(())
        })
    }

    fn mdia(
        &self,
        o: &mut Vec<u8>,
        mdia: &BoxRef,
        track: &Track,
        p: &Plan,
        offsets: &[u64],
        co64: bool,
    ) -> Result<()> {
        put_box(o, b"mdia", |o| {
            for b in children(mdia)? {
                match &b.kind {
                    b"mdhd" => put_header(o, b"mdhd", &read_header(b.body, 4)?, p.media_duration)?,
                    b"minf" => put_box(o, b"minf", |o| {
                        for m in children(&b)? {
                            if &m.kind == b"stbl" {
                                self.stbl(o, &m, track, p, offsets, co64)?;
                            } else {
                                put_raw(o, self.input, &m);
                            }
                        }
                        Ok(())
                    })?,
                    _ => put_raw(o, self.input, &b),
                }
            }
            Ok(())
        })
    }

    fn stbl(
        &self,
        o: &mut Vec<u8>,
        stbl: &BoxRef,
        track: &Track,
        p: &Plan,
        offsets: &[u64],
        co64: bool,
    ) -> Result<()> {
        let stsd =
            child(&children(stbl)?, b"stsd").ok_or(DefragError::Malformed("stbl without stsd"))?;
        let samples = &track.samples;
        put_box(o, b"stbl", |o| {
            put_raw(o, self.input, &stsd);

            let stts = runs(samples.iter().map(|s| s.duration));
            put_full_box(o, b"stts", |o| {
                put_u32s(o, &[stts.len() as u32]);
                for &(n, d) in &stts {
                    put_u32s(o, &[n, d]);
                }
            })?;

            if samples.iter().any(|s| s.cts + p.cts_shift != 0) {
                let shifted = samples
                    .iter()
                    .map(|s| u32::try_from(s.cts + p.cts_shift))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| DefragError::Unsupported("composition offset range"))?;
                let ctts = runs(shifted.into_iter());
                put_full_box(o, b"ctts", |o| {
                    put_u32s(o, &[ctts.len() as u32]);
                    for &(n, off) in &ctts {
                        put_u32s(o, &[n, off]);
                    }
                })?;
            }

            // No stss means every sample is a sync sample.
            if samples.iter().any(|s| !s.sync) {
                let sync: Vec<u32> = (1u32..)
                    .zip(samples)
                    .filter(|(_, s)| s.sync)
                    .map(|(n, _)| n)
                    .collect();
                put_full_box(o, b"stss", |o| {
                    put_u32s(o, &[sync.len() as u32]);
                    put_u32s(o, &sync);
                })?;
            }

            let mut stsc: Vec<[u32; 3]> = Vec::new();
            for (n, chunk) in (1u32..).zip(&track.chunks) {
                if stsc.last().map_or(true, |l| {
                    (l[1], l[2]) != (chunk.samples, chunk.description_index)
                }) {
                    stsc.push([n, chunk.samples, chunk.description_index]);
                }
            }
            put_full_box(o, b"stsc", |o| {
                put_u32s(o, &[stsc.len() as u32]);
                for entry in &stsc {
                    put_u32s(o, entry);
                }
            })?;

            put_full_box(o, b"stsz", |o| {
                put_u32s(o, &[0, samples.len() as u32]);
                for s in samples {
                    put_u32s(o, &[s.size]);
                }
            })?;

            if co64 {
                put_full_box(o, b"co64", |o| {
                    put_u32s(o, &[offsets.len() as u32]);
                    for off in offsets {
                        o.extend_from_slice(&off.to_be_bytes());
                    }
                })
            } else {
                let narrow = offsets
                    .iter()
                    .map(|&off| u32::try_from(off))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| DefragError::Unsupported("chunk offset over 4 GB"))?;
                put_full_box(o, b"stco", |o| {
                    put_u32s(o, &[narrow.len() as u32]);
                    put_u32s(o, &narrow);
                })
            }
        })
    }
}

fn write_progressive(input: &[u8], moov: &[BoxRef], tracks: &[Track]) -> Result<Vec<u8>> {
    let mvhd = child(moov, b"mvhd").ok_or(DefragError::Malformed("moov without mvhd"))?;
    let movie = read_header(mvhd.body, 4)?;
    let plans = tracks.iter().map(plan).collect::<Result<Vec<_>>>()?;
    let movie_duration = plans
        .iter()
        .map(|p| scale(p.presented(), p.media_timescale, movie.timescale()))
        .max()
        .unwrap_or(0);
    let mut order: Vec<(usize, usize)> = tracks
        .iter()
        .enumerate()
        .flat_map(|(t, track)| (0..track.chunks.len()).map(move |c| (t, c)))
        .collect();
    order.sort_by_key(|&(t, c)| tracks[t].chunks[c].src);
    let media_len: u64 = order
        .iter()
        .map(|&(t, c)| tracks[t].chunks[c].len as u64)
        .sum();

    let layout = Layout {
        input,
        moov,
        tracks,
        plans,
        movie,
        movie_duration,
        order,
    };

    let mut ftyp = Vec::new();
    put_box(&mut ftyp, b"ftyp", |o| {
        o.extend_from_slice(b"isom");
        put_u32s(o, &[0x200]);
        for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
            o.extend_from_slice(brand);
        }
        Ok(())
    })?;
    let mdat_header: u64 = if media_len + 8 > u64::from(u32::MAX) {
        16
    } else {
        8
    };

    // The chunk offsets live in moov and depend on its size. They are
    // fixed-width, so a first build measures moov and a second writes the
    // real offsets at the same size; 32-bit ones unless the media reaches
    // past 4 GB.
    let mut co64 = false;
    let mut measured = layout.moov(0, co64)?.len() as u64;
    if ftyp.len() as u64 + measured + mdat_header + media_len > u64::from(u32::MAX) {
        co64 = true;
        measured = layout.moov(0, co64)?.len() as u64;
    }
    let origin = ftyp.len() as u64 + measured + mdat_header;
    let moov_bytes = layout.moov(origin, co64)?;
    if moov_bytes.len() as u64 != measured {
        return Err(DefragError::Malformed("index size changed between passes"));
    }

    let capacity = usize::try_from(origin + media_len)
        .map_err(|_| DefragError::Unsupported("file too large"))?;
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov_bytes);
    if mdat_header == 16 {
        put_u32s(&mut out, &[1]);
        out.extend_from_slice(b"mdat");
        out.extend_from_slice(&(media_len + 16).to_be_bytes());
    } else {
        put_u32s(&mut out, &[(media_len + 8) as u32]);
        out.extend_from_slice(b"mdat");
    }
    for &(t, c) in &layout.order {
        let chunk = &tracks[t].chunks[c];
        out.extend_from_slice(&input[chunk.src..chunk.src + chunk.len]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(kind: &[u8; 4], version_flags: u32, body: &[u8]) -> Vec<u8> {
        let mut b = version_flags.to_be_bytes().to_vec();
        b.extend_from_slice(body);
        bx(kind, &b)
    }

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// mvhd / mdhd version 0: created, modified, timescale, duration, tail.
    fn hd(kind: &[u8; 4], timescale: u32, tail_len: usize) -> Vec<u8> {
        let mut body = words(&[0, 0, timescale, 0]);
        body.extend(std::iter::repeat(0u8).take(tail_len));
        full(kind, 0, &body)
    }

    fn moov(fragmented: bool) -> Vec<u8> {
        let tkhd = full(b"tkhd", 3, &cat(&[words(&[0, 0, 1, 0, 0]), vec![0; 60]]));
        let stsd = full(b"stsd", 0, &cat(&[words(&[1]), bx(b"avc1", &[7; 20])]));
        let empty = |kind: &[u8; 4], n: usize| full(kind, 0, &words(&vec![0; n]));
        let stbl = bx(
            b"stbl",
            &cat(&[
                stsd,
                empty(b"stts", 1),
                empty(b"stsc", 1),
                empty(b"stsz", 2),
                empty(b"stco", 1),
            ]),
        );
        let minf = bx(b"minf", &cat(&[full(b"vmhd", 1, &[0; 8]), stbl]));
        let hdlr = full(
            b"hdlr",
            0,
            &cat(&[words(&[0]), b"vide".to_vec(), vec![0; 13]]),
        );
        let mdia = bx(b"mdia", &cat(&[hd(b"mdhd", 1000, 4), hdlr, minf]));
        let trak = bx(b"trak", &cat(&[tkhd, mdia]));
        // trex: track 1, description 1, duration 40, size 0, non-sync.
        let mvex = bx(
            b"mvex",
            &full(b"trex", 0, &words(&[1, 1, 40, 0, SAMPLE_IS_NON_SYNC])),
        );
        let mut parts = vec![hd(b"mvhd", 1000, 80), trak];
        if fragmented {
            parts.push(mvex);
        }
        bx(b"moov", &cat(&parts))
    }

    /// A moof + mdat pair: samples of `sizes` bytes, each filled with its
    /// own marker byte, the first one sync, all with composition offset 80.
    fn fragment(sequence: u32, base_dts: u64, sizes: &[u32], marker: u8) -> Vec<u8> {
        let tfhd = full(b"tfhd", 0x02_0000, &words(&[1]));
        let tfdt = full(b"tfdt", 1 << 24, &base_dts.to_be_bytes());
        // data offset + first-sample flags + size + cts offset.
        let mut entries = Vec::new();
        for &size in sizes {
            entries.extend(words(&[size, 80]));
        }
        let trun_len = 8 + 4 + 4 + 4 + 4 + entries.len();
        let traf_len = 8 + tfhd.len() + tfdt.len() + trun_len;
        let moof_len = 8 + 16 + traf_len;
        let data_offset = (moof_len + 8) as u32;
        let trun = full(
            b"trun",
            0x01 | 0x04 | 0x200 | 0x800,
            &cat(&[words(&[sizes.len() as u32, data_offset, 0]), entries]),
        );
        let traf = bx(b"traf", &cat(&[tfhd, tfdt, trun]));
        let moof = bx(
            b"moof",
            &cat(&[full(b"mfhd", 0, &words(&[sequence])), traf]),
        );
        assert_eq!(moof.len(), moof_len);
        let mut media = Vec::new();
        for (i, &size) in sizes.iter().enumerate() {
            media.extend(std::iter::repeat(marker + i as u8).take(size as usize));
        }
        cat(&[moof, bx(b"mdat", &media)])
    }

    fn find_box<'a>(boxes: &[BoxRef<'a>], path: &[&[u8; 4]]) -> BoxRef<'a> {
        let found = child(boxes, path[0]).unwrap_or_else(|| panic!("no {:?}", path[0]));
        if path.len() == 1 {
            found
        } else {
            find_box(&children(&found).unwrap(), &path[1..])
        }
    }

    fn table(b: &BoxRef, skip: usize) -> Vec<u32> {
        b.body[4 + skip..]
            .chunks(4)
            .map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]]))
            .collect()
    }

    #[test]
    fn an_ordinary_mp4_is_left_alone() {
        let file = cat(&[
            bx(b"ftyp", b"isom\0\0\0\0"),
            moov(false),
            bx(b"mdat", &[1, 2, 3]),
        ]);
        assert!(defragment(&file).unwrap().is_none());
    }

    #[test]
    fn fragments_become_one_indexed_mdat() {
        let file = cat(&[
            bx(b"ftyp", b"iso6\0\0\0\0"),
            moov(true),
            // Starts ten seconds into its own timeline, like Apple's HEVC.
            fragment(1, 10_000, &[5, 3, 4], 0x10),
            fragment(2, 10_120, &[6, 2], 0x20),
        ]);
        assert!(looks_fragmented(&file));
        let out = defragment(&file).unwrap().expect("fragmented input");
        assert!(!looks_fragmented(&out));
        assert!(defragment(&out).unwrap().is_none());

        let top = parse_boxes(&out, 0).unwrap();
        let kinds: Vec<_> = top.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [*b"ftyp", *b"moov", *b"mdat"]);
        let moov = children(&top[1]).unwrap();
        assert!(child(&moov, b"mvex").is_none());

        let stbl = [b"trak", b"mdia", b"minf", b"stbl"];
        let at = |kind: &[u8; 4]| {
            let mut path = stbl.to_vec();
            path.push(kind);
            find_box(&moov, &path)
        };
        let sizes = table(&at(b"stsz"), 0);
        assert_eq!(sizes, [0, 5, 5, 3, 4, 6, 2]);
        assert_eq!(table(&at(b"stts"), 0), [1, 5, 40]);
        assert_eq!(table(&at(b"stss"), 0), [2, 1, 4]);
        assert_eq!(table(&at(b"ctts"), 0), [1, 5, 80]);
        assert_eq!(table(&at(b"stsc"), 0), [2, 1, 3, 1, 2, 2, 1]);

        // Every sample's bytes sit where the index says.
        let offsets = table(&at(b"stco"), 0);
        assert_eq!(offsets[0], 2);
        let mut sample = 0;
        for (chunk, marker, count) in [(0, 0x10u8, 3), (1, 0x20u8, 2)] {
            let mut at = offsets[1 + chunk] as usize;
            for i in 0..count {
                let size = sizes[2 + sample] as usize;
                assert!(out[at..at + size].iter().all(|&b| b == marker + i as u8));
                at += size;
                sample += 1;
            }
        }

        // Moved to zero; the composition offset delays the first frame by
        // 80, which the edit list skips.
        let elst = find_box(&moov, &[b"trak", b"edts", b"elst"]);
        let media_time = i64::from_be_bytes(elst.body[16..24].try_into().unwrap());
        assert_eq!(media_time, 80);
        let mdhd = find_box(&moov, &[b"trak", b"mdia", b"mdhd"]);
        let duration = u64::from_be_bytes(mdhd.body[24..32].try_into().unwrap());
        assert_eq!(duration, 200);
    }

    #[test]
    fn an_impossible_sample_count_is_refused() {
        // Data offset only: no per-sample field bounds the count by the box,
        // so it has to be bounded by the file.
        let tfhd = full(b"tfhd", 0x02_0000 | 0x10, &words(&[1, 4]));
        let trun = full(b"trun", 0x01, &words(&[u32::MAX, 0]));
        let traf = bx(b"traf", &cat(&[tfhd, trun]));
        let moof = bx(b"moof", &cat(&[full(b"mfhd", 0, &words(&[1])), traf]));
        let file = cat(&[bx(b"ftyp", b"iso6\0\0\0\0"), moov(true), moof]);
        assert!(defragment(&file).is_err());
    }

    #[test]
    fn a_truncated_fragment_is_refused() {
        let mut file = cat(&[
            bx(b"ftyp", b"iso6\0\0\0\0"),
            moov(true),
            fragment(1, 0, &[50], 1),
        ]);
        file.truncate(file.len() - 10);
        assert!(defragment(&file).is_err());
    }

    /// Point at a real file to check a conversion by hand:
    /// `WF_DEFRAG_IN=in.mp4 WF_DEFRAG_OUT=out.mp4 cargo test … -- --ignored`.
    #[test]
    #[ignore]
    fn convert_a_real_file() {
        let input = std::fs::read(std::env::var("WF_DEFRAG_IN").unwrap()).unwrap();
        let out = defragment(&input).unwrap().expect("fragmented input");
        std::fs::write(std::env::var("WF_DEFRAG_OUT").unwrap(), out).unwrap();
    }
}
