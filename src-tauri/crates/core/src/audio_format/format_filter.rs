//! What "this format" means in SQL, for every filter that offers one.
//!
//! The search panel's chips and the rule editor name formats the way a
//! listener does (`OGG`, `ALAC`, `DSF`). `track.codec` holds something else:
//! what the scanner read off the stream ([`file_type_label`], [`codec_label`],
//! the DSD reader's rate). Comparing one to the other as text left four names
//! with no row to match, and `AAC` with rows it should not have.
//!
//! [`file_type_label`]: crate::scanner::file_type_label
//! [`codec_label`]: super::codec_label

/// SQL: whether the track aliased `t` is in one of `formats`, and the values
/// for its `?` placeholders, in order. Upper or lower case alike; an empty
/// list matches nothing.
///
/// Never NULL, not even for a track with no codec. `NOT (NULL)` is NULL, so
/// a condition that could answer it would drop those tracks from a rule
/// saying "not this format" as well as from the rule itself.
///
/// | Format        | Matches                                                  |
/// | ------------- | -------------------------------------------------------- |
/// | `OGG`         | `Vorbis`                                                 |
/// | `AAC`         | `AAC` with no bit depth                                  |
/// | `ALAC`        | `ALAC`, or `AAC` with a bit depth                        |
/// | `DSF` / `DFF` | a known codec on a file with that extension              |
/// | `WAV`         | `WAV`, or `PCM` on a `.wav`                              |
/// | `AIFF`        | `AIFF`, or `PCM` on a `.aif` / `.aiff`                   |
/// | anything else | that codec, spelled the same                             |
///
/// **`OGG` does not include Opus.** Ogg is a container and Opus travels in
/// it too, but the filters offer `OPUS` right beside it and the library shows
/// `Opus` on those rows: a stream is judged by what it is, as
/// `is_scannable_audio` does, and a file does not belong to two chips. The
/// same goes for a FLAC stream in an Ogg file, which is `FLAC`. Picking both
/// chips gives both.
///
/// **`ALAC` is told from `AAC` by its bit depth**, as in
/// [`lossless_track_sql`](crate::lossless_track_sql): the scanner labels the
/// whole MP4 container `AAC`, and no lossy codec reports a depth. A FLAC
/// stream in an MP4 file looks the same, so it lands under `ALAC`.
///
/// **`DSF` and `DFF` are told apart by extension**, because the codec is the
/// rate (`DSD64`) for both. The extension is what sends a file down the DSD
/// reader, which refuses anything else, so on an indexed row it is reliable.
/// `PCM` is what the scanner calls a file only the decoder could describe
/// (#873), a WAV with an open-ended `data` chunk for one, and it names no
/// container, so there too the extension decides.
pub fn formats_condition<S: AsRef<str>>(formats: &[S]) -> (String, Vec<String>) {
    if formats.is_empty() {
        return ("0".to_string(), Vec::new());
    }
    let mut conditions = Vec::with_capacity(formats.len());
    let mut values = Vec::new();
    for format in formats {
        let format = format.as_ref().to_ascii_uppercase();
        conditions.push(match format.as_str() {
            "OGG" => "UPPER(COALESCE(t.codec, '')) = 'VORBIS'",
            "AAC" => "(UPPER(COALESCE(t.codec, '')) = 'AAC' AND t.bit_depth IS NULL)",
            "ALAC" => {
                "(UPPER(COALESCE(t.codec, '')) = 'ALAC' \
                 OR (UPPER(COALESCE(t.codec, '')) = 'AAC' AND t.bit_depth IS NOT NULL))"
            }
            "DSF" => "(t.codec IS NOT NULL AND LOWER(t.file_path) LIKE '%.dsf')",
            "DFF" => "(t.codec IS NOT NULL AND LOWER(t.file_path) LIKE '%.dff')",
            "WAV" => {
                "(UPPER(COALESCE(t.codec, '')) = 'WAV' \
                 OR (UPPER(COALESCE(t.codec, '')) = 'PCM' AND LOWER(t.file_path) LIKE '%.wav'))"
            }
            "AIFF" => {
                "(UPPER(COALESCE(t.codec, '')) = 'AIFF' \
                 OR (UPPER(COALESCE(t.codec, '')) = 'PCM' \
                     AND (LOWER(t.file_path) LIKE '%.aif' OR LOWER(t.file_path) LIKE '%.aiff')))"
            }
            _ => {
                values.push(format);
                "UPPER(COALESCE(t.codec, '')) = ?"
            }
        });
    }
    (format!("({})", conditions.join(" OR ")), values)
}

#[cfg(test)]
mod tests;
