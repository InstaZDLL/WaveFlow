//! The audio out of an HLS segment, as one continuous elementary stream.
//!
//! symphonia reads ADTS (AAC) and MPEG audio, but not the MPEG transport
//! stream that most HLS radios put them in — of 37 live HLS stations
//! probed, 30 used MPEG-TS (all AAC) and 7 "packed audio" (ADTS behind an
//! ID3 tag); none used fragmented MP4. So the segments are reduced to their
//! audio here and appended one after the other: the decoder then sees a
//! plain ADTS or MP3 stream, the same as an Icecast mount.
//!
//! The demuxer keeps what it learnt (the program map's PID, the audio
//! PID) across segments, as a stream of them is one transport stream; a
//! packet cut across two `feed` calls is completed by the next one.

/// The audio an elementary stream carries — what the decoder is told to
/// expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioKind {
    /// AAC in ADTS frames (`stream_type` 0x0F).
    Adts,
    /// MPEG-1/2 audio (`stream_type` 0x03 / 0x04): MP3, MP2.
    Mpeg,
}

impl AudioKind {
    /// The extension the decoder's probe is hinted with.
    pub fn extension(self) -> &'static str {
        match self {
            AudioKind::Adts => "aac",
            AudioKind::Mpeg => "mp3",
        }
    }
}

const PACKET: usize = 188;
const SYNC: u8 = 0x47;

/// A stateful MPEG-TS demuxer keeping the first audio stream.
#[derive(Debug, Default)]
pub struct TsDemuxer {
    pmt_pid: Option<u16>,
    audio: Option<(u16, AudioKind)>,
    /// The start of a packet the previous call ended in the middle of.
    carry: Vec<u8>,
}

/// A PSI section (PAT, PMT) out of a packet payload that starts one:
/// past its pointer field, bounded by its own length.
fn section(payload: &[u8]) -> Option<&[u8]> {
    let pointer = *payload.first()? as usize;
    let start = 1 + pointer;
    let header = payload.get(start..start + 3)?;
    let length = (usize::from(header[1] & 0x0f) << 8) | usize::from(header[2]);
    payload.get(start..start + 3 + length)
}

impl TsDemuxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The audio found so far, once the program map has named it.
    pub fn audio_kind(&self) -> Option<AudioKind> {
        self.audio.map(|(_, kind)| kind)
    }

    /// Demux `data`, appending the audio it carries to `out`.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) {
        let mut buf = std::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let mut at = 0;
        while at + PACKET <= buf.len() {
            if buf[at] != SYNC {
                // Lost sync: look for the next sync byte that has another
                // one a packet later, which a stray 0x47 inside a payload
                // rarely has.
                at += 1;
                while at + PACKET < buf.len() && !(buf[at] == SYNC && buf[at + PACKET] == SYNC) {
                    at += 1;
                }
                continue;
            }
            self.packet(&buf[at..at + PACKET], out);
            at += PACKET;
        }
        self.carry = buf[at..].to_vec();
    }

    fn packet(&mut self, p: &[u8], out: &mut Vec<u8>) {
        let start = p[1] & 0x40 != 0;
        let pid = (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
        let control = (p[3] >> 4) & 0x3;
        // 1: payload only; 2: adaptation field only; 3: both.
        if control == 0 || control == 2 {
            return;
        }
        let mut offset = 4;
        if control == 3 {
            offset += 1 + p[4] as usize;
        }
        let Some(payload) = p.get(offset..) else {
            return;
        };
        if payload.is_empty() {
            return;
        }

        if pid == 0 {
            if start {
                self.read_pat(payload);
            }
        } else if Some(pid) == self.pmt_pid {
            if start {
                self.read_pmt(payload);
            }
        } else if let Some((audio_pid, _)) = self.audio {
            if pid != audio_pid {
                return;
            }
            if start {
                // PES header: 00 00 01, stream id, length (2), flags (2),
                // header length, then that many bytes of optional fields.
                if payload.len() < 9 || payload[..3] != [0, 0, 1] {
                    return;
                }
                let skip = 9 + payload[8] as usize;
                if let Some(es) = payload.get(skip..) {
                    out.extend_from_slice(es);
                }
            } else {
                out.extend_from_slice(payload);
            }
        }
    }

    /// The program association table: the first program's map PID.
    fn read_pat(&mut self, payload: &[u8]) {
        let Some(section) = section(payload) else {
            return;
        };
        // 8 bytes of header before the program loop, 4 of CRC after it.
        let Some(programs) = section.get(8..section.len().saturating_sub(4)) else {
            return;
        };
        for entry in programs.chunks_exact(4) {
            let program = (u16::from(entry[0]) << 8) | u16::from(entry[1]);
            if program != 0 {
                self.pmt_pid = Some((u16::from(entry[2] & 0x1f) << 8) | u16::from(entry[3]));
                return;
            }
        }
    }

    /// The program map: the audio stream to keep. The one already chosen
    /// while the map still lists it — a map is resent in every segment —
    /// and otherwise the first one WaveFlow can decode: an encoder that
    /// restarted, or a discontinuity, may renumber the streams, and holding
    /// on to a PID no packet carries any more would demux nothing, for good.
    /// A map naming no decodable audio leaves none chosen.
    fn read_pmt(&mut self, payload: &[u8]) {
        let Some(section) = section(payload) else {
            return;
        };
        let Some(info) = section.get(10..12) else {
            return;
        };
        let program_info = (usize::from(info[0] & 0x0f) << 8) | usize::from(info[1]);
        let end = section.len().saturating_sub(4);
        let mut at = 12 + program_info;
        let mut decodable: Vec<(u16, AudioKind)> = Vec::new();
        while at + 5 <= end {
            let stream_type = section[at];
            let pid = (u16::from(section[at + 1] & 0x1f) << 8) | u16::from(section[at + 2]);
            let es_info = (usize::from(section[at + 3] & 0x0f) << 8) | usize::from(section[at + 4]);
            let kind = match stream_type {
                0x0f => Some(AudioKind::Adts),
                0x03 | 0x04 => Some(AudioKind::Mpeg),
                _ => None,
            };
            if let Some(kind) = kind {
                decodable.push((pid, kind));
            }
            at += 5 + es_info;
        }
        let kept = self
            .audio
            .and_then(|(pid, _)| decodable.iter().find(|(p, _)| *p == pid));
        self.audio = kept.or(decodable.first()).copied();
    }
}

/// Whether a segment is an MPEG transport stream: a sync byte at the start
/// and again a packet later.
pub fn is_transport_stream(segment: &[u8]) -> bool {
    segment.first() == Some(&SYNC) && segment.get(PACKET).map_or(true, |b| *b == SYNC)
}

/// A "packed audio" segment (ADTS or MP3 written as is) without the ID3v2
/// tag HLS puts in front of it to carry its timestamp — or the segment
/// itself when it has none.
pub fn strip_id3(segment: &[u8]) -> &[u8] {
    let mut rest = segment;
    while rest.len() >= 10 && &rest[..3] == b"ID3" {
        let size = (usize::from(rest[6] & 0x7f) << 21)
            | (usize::from(rest[7] & 0x7f) << 14)
            | (usize::from(rest[8] & 0x7f) << 7)
            | usize::from(rest[9] & 0x7f);
        // A footer (flag 0x10) adds ten more bytes.
        let footer = if rest[5] & 0x10 != 0 { 10 } else { 0 };
        let Some(next) = rest.get(10 + size + footer..) else {
            return &[];
        };
        rest = next;
    }
    rest
}

/// What packed audio holds, by its first frame's sync word.
pub fn packed_kind(audio: &[u8]) -> Option<AudioKind> {
    match audio {
        [0xff, second, ..] if second & 0xf6 == 0xf0 => Some(AudioKind::Adts),
        [0xff, second, ..] if second & 0xe0 == 0xe0 => Some(AudioKind::Mpeg),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
