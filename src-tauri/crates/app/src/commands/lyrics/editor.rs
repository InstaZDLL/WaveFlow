//! The lyrics editor's two conversions: a stored document into the
//! model it edits, and the model back into a format. Saving the result
//! goes through [`save_lyrics`] like any other edit, so the editor has
//! no write path of its own.

use serde::{Deserialize, Serialize};
use waveflow_core::lyrics::{self, lrc, plain, ttml, Document, ExportError};

use crate::error::{AppError, AppResult};

use super::*;

/// What [`read_lyrics_document`] made of the content.
#[derive(Debug, Serialize)]
pub struct LyricsDocumentRead {
    /// The format the content was read as.
    pub format: LyricsFormat,
    pub document: Document,
}

/// Read stored lyrics into the editor's model. `format` is the one the
/// payload carries; without it the content is sniffed. A TTML document
/// that does not parse is an error, so the editor never opens on an
/// empty document and saves it over lyrics it could not read.
#[tauri::command]
pub fn read_lyrics_document(
    content: String,
    format: Option<LyricsFormat>,
) -> AppResult<LyricsDocumentRead> {
    let format = format.unwrap_or_else(|| detect_format(&content));
    let document = lyrics::parse_as(&content, format)
        .map_err(|e| AppError::Other(format!("lyrics document: {e}")))?;
    Ok(LyricsDocumentRead { format, document })
}

/// LRC options the editor exposes. TTML needs none: it carries every
/// field the model has.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LyricsWriteOptions {
    /// Translations as lines stamped at their original's time.
    pub translations: bool,
}

/// The editor's model written in a format, or the first unit that keeps
/// it from being written.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LyricsDocumentWrite {
    Written {
        content: String,
    },
    /// Indexes count from zero, into the document's lines and that
    /// line's words; `word` is absent when the line itself has no time.
    Unsynced {
        line: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        word: Option<usize>,
    },
}

/// Write the editor's model as `format`. An unsynced unit is an answer,
/// not an error: the editor shows it and jumps to it.
#[tauri::command]
pub fn write_lyrics_document(
    document: Document,
    format: LyricsFormat,
    options: Option<LyricsWriteOptions>,
) -> LyricsDocumentWrite {
    match write_document(&document, format, &options.unwrap_or_default()) {
        Ok(content) => LyricsDocumentWrite::Written { content },
        Err(ExportError::UnsyncedLine { line }) => {
            LyricsDocumentWrite::Unsynced { line, word: None }
        }
        Err(ExportError::UnsyncedWord { line, word }) => LyricsDocumentWrite::Unsynced {
            line,
            word: Some(word),
        },
    }
}

pub(super) fn write_document(
    document: &Document,
    format: LyricsFormat,
    options: &LyricsWriteOptions,
) -> Result<String, ExportError> {
    match format {
        LyricsFormat::Plain => Ok(plain::write(document)),
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc => lrc::write(
            document,
            &lrc::WriteOptions {
                enhanced: format == LyricsFormat::EnhancedLrc,
                translations: options.translations,
                ..lrc::WriteOptions::default()
            },
        ),
        LyricsFormat::Ttml => ttml::write(document, &ttml::WriteOptions::default()),
    }
}
