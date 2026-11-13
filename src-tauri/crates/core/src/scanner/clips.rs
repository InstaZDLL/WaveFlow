//! Music videos: a video file that belongs to a track (discussion #1073).
//!
//! A clip is found by its name, never by a column: the video that carries
//! a track's file name (its extension aside, case ignored), either beside
//! the track or in a `Clips` folder beside it. The disk is the authority,
//! as it is for the hand-set Canvas of #695, so nothing has to be kept in
//! step with it and there is no migration.
//!
//! The rule matters to the scanner for one extension only. `mp4` is in
//! [`AUDIO_EXTENSIONS`](super::extract::AUDIO_EXTENSIONS), legitimately (an
//! `.mp4` often holds nothing but AAC), and
//! [`is_scannable_audio`](super::extract::is_scannable_audio) accepts it
//! without opening it, so a clip dropped beside its song used to become a
//! second track. [`clip_candidates`] names the `.mp4` files the rule would
//! make clips, and the walker drops those of them that [`has_video_track`]
//! confirms. The other video containers are not audio extensions and never
//! reach the walk.
//!
//! What stays a track, on purpose:
//! - an `.mp4` with no video in it, whatever its neighbours: real audio is
//!   never lost to this rule;
//! - a video `.mp4` with no track of the same name: a library made of
//!   music videos alone keeps its tracks (and the player offers each one's
//!   own picture, [`ClipOrigin::Itself`]).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::artwork::mp4_defrag::next_header;

/// The containers a clip can be. Recognised whatever the webview can play:
/// a format it refuses is reported when the clip is opened, which is more
/// useful than a clip that silently does not exist.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv"];

/// The ISO-BMFF ones, whose content [`has_video_track`] can confirm. An
/// `.mp4` or a `.mov` can hold audio alone; a `.webm` or an `.mkv` is
/// taken at its extension.
const ISO_BMFF_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov"];

/// The folder beside a track that holds its clip, case ignored.
pub const CLIPS_DIR_NAME: &str = "Clips";

/// Where a track's clip was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipOrigin {
    /// Beside the track, same name.
    Sibling,
    /// In a `Clips` folder beside the track, same name.
    ClipsDir,
    /// The track's own file is a video.
    Itself,
}

fn lowercase_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
}

/// The name a track and its clip share: the file name without its
/// extension, lower-cased. `None` for a name that is not UTF-8, which no
/// clip is matched against rather than matched loosely.
fn stem_key(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(OsStr::to_str)
        .map(str::to_lowercase)
}

pub fn is_video_extension(path: &Path) -> bool {
    lowercase_extension(path).is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

pub fn is_clips_dir_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.eq_ignore_ascii_case(CLIPS_DIR_NAME))
}

/// The `.mp4` files of a walk that its other files make clips, by name
/// alone: one with a non-`.mp4` file of the same name in its directory, or
/// one in a `Clips` directory whose parent holds a file of that name.
/// Indices into `files`, in order.
///
/// `files` is a walk's list of audio files, local paths or share locators
/// alike: both split on `/`, and the comparison is on components. Only the
/// name is judged here; the caller confirms each candidate with
/// [`has_video_track`] before dropping it, so an audio `.mp4` that merely
/// shares a name with another file stays a track.
pub fn clip_candidates(files: &[PathBuf]) -> Vec<usize> {
    let mut audio: HashSet<(&Path, String)> = HashSet::new();
    for file in files {
        if lowercase_extension(file).as_deref() == Some("mp4") {
            continue;
        }
        if let (Some(dir), Some(stem)) = (file.parent(), stem_key(file)) {
            audio.insert((dir, stem));
        }
    }
    files
        .iter()
        .enumerate()
        .filter(|(_, file)| lowercase_extension(file).as_deref() == Some("mp4"))
        .filter(|(_, file)| {
            let (Some(dir), Some(stem)) = (file.parent(), stem_key(file)) else {
                return false;
            };
            if audio.contains(&(dir, stem.clone())) {
                return true;
            }
            dir.file_name().is_some_and(is_clips_dir_name)
                && dir
                    .parent()
                    .is_some_and(|parent| audio.contains(&(parent, stem)))
        })
        .map(|(index, _)| index)
        .collect()
}

/// Take the clips out of a walk's list: every [`clip_candidates`] entry
/// `is_video` confirms. Returns how many went. `is_video` is the caller's
/// because reading a file differs between a local disk and a share; an
/// answer of `false`, a read that failed included, keeps the file a track.
pub fn drop_clips(files: &mut Vec<PathBuf>, mut is_video: impl FnMut(&Path) -> bool) -> usize {
    let clips: HashSet<usize> = clip_candidates(files)
        .into_iter()
        .filter(|&index| is_video(&files[index]))
        .collect();
    if clips.is_empty() {
        return 0;
    }
    let mut index = 0;
    files.retain(|_| {
        let keep = !clips.contains(&index);
        index += 1;
        keep
    });
    clips.len()
}

/// Boxes visited before giving up on a file. A real one has a handful of
/// tracks; this only bounds what a hostile file can make the walk read.
const MAX_BOXES: usize = 4096;

/// Sample entries that are moving pictures. A `vide` track can also be a
/// chapter's still image (`jpeg`, `png `), which an audiobook carries and
/// which is no clip.
const VIDEO_CODECS: &[&[u8; 4]] = &[
    b"avc1", b"avc3", b"hvc1", b"hev1", b"dvh1", b"dvhe", b"av01", b"vp08", b"vp09", b"mp4v",
    b"encv", b"apcn", b"apch", b"apcs", b"apco", b"ap4h",
];

/// Whether an ISO-BMFF file (`.mp4`, `.m4v`, `.mov`) carries a moving
/// picture: a track whose handler is `vide` and whose first sample entry
/// is a video codec.
///
/// Reads box headers and two small bodies per track, seeking over the
/// rest, so the cost is a few small reads whatever the file's size and
/// wherever its index sits. The walk is flat and bounded: it descends a
/// fixed path (`moov` → `trak` → `mdia` → `hdlr` / `minf` → `stbl` →
/// `stsd`), never recursing on what the file says, and stops after
/// [`MAX_BOXES`]. A malformed header ends the walk as "no video", the
/// direction that keeps a file a track.
pub fn has_video_track<R: Read + Seek>(file: &mut R) -> std::io::Result<bool> {
    let end = file.seek(SeekFrom::End(0))?;
    let mut budget = MAX_BOXES;
    let mut pos = 0;
    while let Some((kind, body, next)) = next_header(file, pos, end)? {
        budget = budget.saturating_sub(1);
        if budget == 0 {
            return Ok(false);
        }
        if &kind == b"moov" {
            let mut at = body;
            while let Some((kind, body, trak_end)) = next_header(file, at, next)? {
                budget = budget.saturating_sub(1);
                if budget == 0 {
                    return Ok(false);
                }
                if &kind == b"trak" && track_is_video(file, body, trak_end, &mut budget)? {
                    return Ok(true);
                }
                at = trak_end;
            }
            return Ok(false);
        }
        pos = next;
    }
    Ok(false)
}

/// The first child of `[start, end)` of type `kind`: its body and end.
fn find_child<R: Read + Seek>(
    file: &mut R,
    start: u64,
    end: u64,
    kind: &[u8; 4],
    budget: &mut usize,
) -> std::io::Result<Option<(u64, u64)>> {
    let mut at = start;
    while let Some((found, body, child_end)) = next_header(file, at, end)? {
        *budget = budget.saturating_sub(1);
        if *budget == 0 {
            return Ok(None);
        }
        if &found == kind {
            return Ok(Some((body, child_end)));
        }
        at = child_end;
    }
    Ok(None)
}

/// Four bytes at `offset`, if they lie before `end`.
fn read_fourcc<R: Read + Seek>(
    file: &mut R,
    offset: u64,
    end: u64,
) -> std::io::Result<Option<[u8; 4]>> {
    if offset.saturating_add(4) > end {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut out = [0u8; 4];
    file.read_exact(&mut out)?;
    Ok(Some(out))
}

fn track_is_video<R: Read + Seek>(
    file: &mut R,
    trak: u64,
    trak_end: u64,
    budget: &mut usize,
) -> std::io::Result<bool> {
    let Some((mdia, mdia_end)) = find_child(file, trak, trak_end, b"mdia", budget)? else {
        return Ok(false);
    };
    // `hdlr`: version and flags (4), pre_defined (4), handler_type (4).
    let Some((hdlr, hdlr_end)) = find_child(file, mdia, mdia_end, b"hdlr", budget)? else {
        return Ok(false);
    };
    if read_fourcc(file, hdlr + 8, hdlr_end)?.as_ref() != Some(b"vide") {
        return Ok(false);
    }
    let Some((minf, minf_end)) = find_child(file, mdia, mdia_end, b"minf", budget)? else {
        return Ok(false);
    };
    let Some((stbl, stbl_end)) = find_child(file, minf, minf_end, b"stbl", budget)? else {
        return Ok(false);
    };
    let Some((stsd, stsd_end)) = find_child(file, stbl, stbl_end, b"stsd", budget)? else {
        return Ok(false);
    };
    // `stsd`: version and flags (4), entry_count (4), then the first
    // entry's size (4) and format (4).
    let codec = read_fourcc(file, stsd + 12, stsd_end)?;
    Ok(codec.is_some_and(|codec| VIDEO_CODECS.contains(&&codec)))
}

/// [`has_video_track`] on a local file. A file that cannot be opened or
/// read answers `false`, which keeps it a track: the scan's extractor
/// reports an unreadable file better than its absence would.
pub fn file_has_video_track(path: &Path) -> bool {
    fs::File::open(path)
        .and_then(|mut file| has_video_track(&mut file))
        .unwrap_or(false)
}

/// Whether a local video file is one: confirmed for the ISO-BMFF
/// containers, taken at its extension for the others.
fn is_local_video(path: &Path) -> bool {
    match lowercase_extension(path) {
        Some(ext) if ISO_BMFF_EXTENSIONS.contains(&ext.as_str()) => file_has_video_track(path),
        Some(ext) => VIDEO_EXTENSIONS.contains(&ext.as_str()),
        None => false,
    }
}

/// The video in `dir` named like `stem`, other than `track` itself.
/// Several candidates (a `.webm` and an `.mp4`) are taken in
/// [`VIDEO_EXTENSIONS`] order, so the answer does not depend on the order
/// a directory lists in.
fn video_named_in(dir: &Path, stem: &str, track: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path != track)
        .filter(|path| is_video_extension(path))
        .filter(|path| stem_key(path).as_deref() == Some(stem))
        .collect();
    found.sort_by_key(|path| {
        let ext = lowercase_extension(path).unwrap_or_default();
        VIDEO_EXTENSIONS.iter().position(|known| *known == ext)
    });
    found
        .into_iter()
        .find(|path| path.is_file() && is_local_video(path))
}

/// The `Clips` directory of `dir`, whatever its case.
fn clips_dir_of(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| is_clips_dir_name(&entry.file_name()) && entry.path().is_dir())
        .map(|entry| entry.path())
}

/// The clip of a local track: beside it, else in its `Clips` folder, else
/// the track's own file when it is a video. Blocking (a directory listing
/// or two and a few header reads); run it off the async runtime.
///
/// A track inside a `Clips` folder is not offered its neighbours: they
/// are the clips of the tracks above, and it is most likely one of them
/// that the scan kept because it holds no picture.
pub fn find_clip(track: &Path) -> Option<(PathBuf, ClipOrigin)> {
    let dir = track.parent()?;
    let stem = stem_key(track)?;
    let in_clips_dir = dir.file_name().is_some_and(is_clips_dir_name);
    if !in_clips_dir {
        if let Some(path) = video_named_in(dir, &stem, track) {
            return Some((path, ClipOrigin::Sibling));
        }
        if let Some(path) = clips_dir_of(dir).and_then(|clips| video_named_in(&clips, &stem, track))
        {
            return Some((path, ClipOrigin::ClipsDir));
        }
    }
    let own_extension = lowercase_extension(track)?;
    (ISO_BMFF_EXTENSIONS.contains(&own_extension.as_str()) && file_has_video_track(track))
        .then(|| (track.to_path_buf(), ClipOrigin::Itself))
}

#[cfg(test)]
mod tests;
