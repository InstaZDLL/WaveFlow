//! symphonia's two ways into an ID3v2 tag, with the check its parser does not have.
//!
//! Its `Id3v2Reader` recurses on nested chapter frames like every other parser
//! (`tagio::chapter_nesting`), and it is what every stream goes through: a library
//! file, a radio, a podcast, a share. So the probe holds these ahead of symphonia's
//! own readers, at the preferred tier, and a tag refused here refuses the stream.
//! That covers what no path check could: a stream has no second look, and the tag
//! comes from whichever host served it.
//!
//! The price is an AIFF served with no way to seek, which no longer opens: its chunks
//! cannot be seen before its reader goes through them. And symphonia sees no other
//! chunk of an AIFF, so its markers, its text and its tag are not in what it reports:
//! the library reads those through lofty, and only a live stream's tags come from here.

use std::io::{self, Cursor, Read, Seek, SeekFrom};

use symphonia::core::errors::{decode_error, unsupported_error, Result};
use symphonia::core::formats::probe::{
    ProbeFormatData, ProbeMetadataData, ProbeableFormat, ProbeableMetadata, Score, Scoreable,
};
use symphonia::core::formats::{
    Attachment, FormatInfo, FormatOptions, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo,
    Track,
};
use symphonia::core::io::{MediaSource, MediaSourceStream, ReadBytes, ScopedStream};
use symphonia::core::meta::{
    ChapterGroup, Metadata, MetadataBuffer, MetadataInfo, MetadataOptions, MetadataReader,
};
use symphonia::core::packet::Packet;
use symphonia::default::formats::AiffReader;
use symphonia::default::meta::Id3v2Reader;

use crate::tagio::chapter_nesting;

const NESTED: &str = "id3v2: nested chapter frames";

/// The chunks walked to find the two that are read. A real AIFF has a handful.
const MAX_AIFF_CHUNKS: usize = 65_536;

/// What is kept of a `COMM`. 18 bytes, and an AIFC adds a type and a name of 255 at most.
const MAX_COMMON_LEN: u64 = 1024;

/// Reads the whole tag, checks it, and only then lets symphonia parse the copy.
pub struct GuardedId3v2Reader<'s> {
    reader: MediaSourceStream<'s>,
    options: MetadataOptions,
}

impl Scoreable for GuardedId3v2Reader<'_> {
    fn score(src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        Id3v2Reader::score(src)
    }
}

impl ProbeableMetadata<'_> for GuardedId3v2Reader<'_> {
    fn try_probe_new(
        mss: MediaSourceStream<'_>,
        options: MetadataOptions,
    ) -> Result<Box<dyn MetadataReader + '_>> {
        Ok(Box::new(GuardedId3v2Reader {
            reader: mss,
            options,
        }))
    }

    fn probe_data() -> &'static [ProbeMetadataData] {
        Id3v2Reader::probe_data()
    }
}

impl MetadataReader for GuardedId3v2Reader<'_> {
    fn metadata_info(&self) -> &MetadataInfo {
        &Id3v2Reader::probe_data()[0].info
    }

    fn read_all(&mut self) -> Result<MetadataBuffer> {
        let mut header = [0_u8; 10];
        self.reader.read_exact(&mut header)?;
        let size = header[6..10].iter().fold(0_usize, |value, byte| {
            (value << 7) | usize::from(byte & 0x7f)
        });
        let mut tag = vec![0_u8; header.len() + size];
        tag[..header.len()].copy_from_slice(&header);
        self.reader.read_exact(&mut tag[header.len()..])?;
        if chapter_nesting::check_tag(&tag).is_err() {
            return decode_error(NESTED);
        }
        let copy = MediaSourceStream::new(Box::new(Cursor::new(tag)), Default::default());
        Id3v2Reader::try_new(copy, self.options)?.read_all()
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        self.reader
    }
}

/// AIFF reads its `ID3 ` chunk inside the format reader, and no check of the file can
/// say where that reader will look: it takes what it needs from a `COMM` and goes on
/// there whatever length was declared, and reads a tag as far as the tag declares, so
/// a chunk can be reached in the middle of the audio. So it is not left to look. It
/// reads the two chunks that make the sound, in an order of [`SoundThenCommon`]'s.
pub struct GuardedAiffReader<'s>(AiffReader<'s>);

impl Scoreable for GuardedAiffReader<'_> {
    fn score(src: ScopedStream<&mut MediaSourceStream<'_>>) -> Result<Score> {
        AiffReader::score(src)
    }
}

impl ProbeableFormat<'_> for GuardedAiffReader<'_> {
    fn try_probe_new(
        mut mss: MediaSourceStream<'_>,
        options: FormatOptions,
    ) -> Result<Box<dyn FormatReader + '_>> {
        // The chunks have to be seen before the reader goes through them, and a live
        // stream cannot be read twice. Nothing broadcasts AIFF.
        if !mss.is_seekable() {
            return unsupported_error("aiff: a stream that cannot seek");
        }
        if chapter_nesting::check_reader(&mut mss).is_err() {
            return decode_error(NESTED);
        }
        let only_the_sound = SoundThenCommon::over(mss)?;
        Ok(Box::new(GuardedAiffReader(AiffReader::try_new(
            MediaSourceStream::new(Box::new(only_the_sound), Default::default()),
            options,
        )?)))
    }

    fn probe_data() -> &'static [ProbeFormatData] {
        AiffReader::probe_data()
    }
}

impl FormatReader for GuardedAiffReader<'_> {
    fn format_info(&self) -> &FormatInfo {
        self.0.format_info()
    }

    fn media_info(&self) -> &MediaInfo {
        self.0.media_info()
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.0.metadata()
    }

    fn chapters(&self) -> Option<&ChapterGroup> {
        self.0.chapters()
    }

    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo> {
        self.0.seek(mode, to)
    }

    fn tracks(&self) -> &[Track] {
        self.0.tracks()
    }

    fn attachments(&self) -> &[Attachment] {
        self.0.attachments()
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        self.0.next_packet()
    }

    fn into_inner<'s>(self: Box<Self>) -> MediaSourceStream<'s>
    where
        Self: 's,
    {
        Box::new(self.0).into_inner()
    }
}

/// An AIFF as its reader is given it: the form, the `SSND` and then the `COMM`, and
/// nothing else.
///
/// The order is the point. The reader counts each chunk by what it declares and stops
/// when the count reaches the form's length, before reading anything further
/// (`ChunksReader::next`). With the `COMM` last and the form's length made to end there,
/// what the reader does not take from it is never read as a chunk, and no chunk header
/// is read after it. It skips the audio by the length declared, in one seek, without
/// reading it. So there is no model here of what it reads from each chunk, which is
/// what the bytes kept beating, and nothing it could land on.
struct SoundThenCommon<'s> {
    inner: MediaSourceStream<'s>,
    /// Where the audio chunk's body is in the file.
    audio_start: u64,
    /// What goes before the audio: the form's header and the `SSND`'s.
    head: Vec<u8>,
    audio_len: u64,
    /// What goes after it: a padding byte if the audio asks for one, and the `COMM`.
    tail: Vec<u8>,
    /// Where the reader is in this view.
    position: u64,
    /// Where `inner` is in the file, when it is where the audio needs it.
    inner_at: Option<u64>,
}

impl<'s> SoundThenCommon<'s> {
    /// Walks the chunks of the form at the stream's position by their declared lengths.
    /// A file those lengths lead astray gets the wrong audio or none, and no more.
    fn over(mut mss: MediaSourceStream<'s>) -> Result<Self> {
        let start = mss.pos();
        let mut form = [0_u8; 12];
        mss.read_exact(&mut form)?;
        let mut common = None;
        let mut sound = None;
        let mut at = start + form.len() as u64;
        for _ in 0..MAX_AIFF_CHUNKS {
            let mut header = [0_u8; 8];
            mss.seek(SeekFrom::Start(at))?;
            if mss.read_exact(&mut header).is_err() {
                break;
            }
            let declared = u64::from(u32::from_be_bytes([
                header[4], header[5], header[6], header[7],
            ]));
            if common.is_none() && &header[..4] == b"COMM" {
                let kept = declared.min(MAX_COMMON_LEN) as usize;
                let mut body = vec![0_u8; kept];
                mss.read_exact(&mut body)?;
                common = Some(body);
            } else if sound.is_none() && &header[..4] == b"SSND" {
                sound = Some((at + 8, declared));
            }
            if common.is_some() && sound.is_some() {
                break;
            }
            at += 8 + declared + (declared & 1);
        }
        let Some(common) = common else {
            return decode_error("aiff: missing common element");
        };

        let (audio_start, audio_len) = sound.unwrap_or((0, 0));
        let mut tail = Vec::with_capacity(common.len() + 9);
        if audio_len % 2 == 1 {
            tail.push(0);
        }
        tail.extend_from_slice(b"COMM");
        tail.extend_from_slice(&(common.len() as u32).to_be_bytes());
        tail.extend_from_slice(&common);

        let mut head = form.to_vec();
        if sound.is_some() {
            head.extend_from_slice(b"SSND");
            head.extend_from_slice(&(audio_len as u32).to_be_bytes());
        }
        // What the form's length counts is all that follows its own eight bytes.
        let total = head.len() as u64 + audio_len + tail.len() as u64;
        let Ok(form_len) = u32::try_from(total - 8) else {
            return unsupported_error("aiff: larger than a form can declare");
        };
        head[4..8].copy_from_slice(&form_len.to_be_bytes());
        Ok(Self {
            inner: mss,
            audio_start,
            head,
            audio_len,
            tail,
            position: 0,
            inner_at: None,
        })
    }

    fn len(&self) -> u64 {
        self.head.len() as u64 + self.audio_len + self.tail.len() as u64
    }
}

/// As much of `bytes` as fits, from `from`.
fn copy_from(bytes: &[u8], from: u64, buffer: &mut [u8]) -> usize {
    let rest = &bytes[from as usize..];
    let most = rest.len().min(buffer.len());
    buffer[..most].copy_from_slice(&rest[..most]);
    most
}

impl Read for SoundThenCommon<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let head_len = self.head.len() as u64;
        let read = if self.position < head_len {
            copy_from(&self.head, self.position, buffer)
        } else if self.position < head_len + self.audio_len {
            let into_audio = self.position - head_len;
            let wanted = self.audio_start + into_audio;
            if self.inner_at != Some(wanted) {
                self.inner.seek(SeekFrom::Start(wanted))?;
            }
            let left = self.audio_len - into_audio;
            let most = buffer
                .len()
                .min(usize::try_from(left).unwrap_or(usize::MAX));
            let read = self.inner.read(&mut buffer[..most])?;
            self.inner_at = Some(wanted + read as u64);
            read
        } else {
            let into_tail = self.position - head_len - self.audio_len;
            if into_tail >= self.tail.len() as u64 {
                return Ok(0);
            }
            copy_from(&self.tail, into_tail, buffer)
        };
        self.position += read as u64;
        Ok(read)
    }
}

impl Seek for SoundThenCommon<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::Current(by) => self.position.checked_add_signed(by),
            SeekFrom::End(by) => self.len().checked_add_signed(by),
        };
        self.position = target.ok_or(io::ErrorKind::InvalidInput)?;
        Ok(self.position)
    }
}

impl MediaSource for SoundThenCommon<'_> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.len())
    }
}
