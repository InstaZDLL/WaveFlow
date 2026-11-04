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
//! cannot be seen before its reader goes through them.

use std::io::Cursor;

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
        self.reader.read_buf_exact(&mut header)?;
        let size = header[6..10].iter().fold(0_usize, |value, byte| {
            (value << 7) | usize::from(byte & 0x7f)
        });
        let body = self.reader.read_boxed_slice_exact(size)?;
        let tag = [header.as_slice(), &body].concat();
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

/// AIFF reads its `ID3 ` chunk inside the format reader, wherever the chunk is.
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
        Ok(Box::new(GuardedAiffReader(AiffReader::try_new(
            mss, options,
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
