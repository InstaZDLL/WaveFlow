//! What a lyrics document is: plain text, LRC, enhanced LRC or TTML,
//! and whether it actually carries timing.

use super::*;

/// Heuristic format sniffer: [`LyricsFormat::detect`], shared with the
/// lyrics editor's reader.
pub(super) fn detect_format(content: &str) -> LyricsFormat {
    LyricsFormat::detect(content)
}

/// Whether a TTML document is well formed (its attributes included), rooted
/// at `<tt>`, and holds a
/// `<p>` with text in it: what the renderer needs to show anything, timed
/// or not.
pub(super) fn ttml_has_a_line(content: &str) -> bool {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(content);
    let mut stack: Vec<String> = Vec::new();
    let mut root_is_tt = None;
    let mut line_with_text = false;
    loop {
        match reader.read_event() {
            // A malformed attribute (a duplicate, a missing quote) makes
            // `DOMParser` reject the whole document, as in
            // `ttml_has_timed_line`; quick-xml only reports it when the
            // attributes are read.
            Ok(Event::Start(e)) => {
                if e.attributes().any(|a| a.is_err()) {
                    return false;
                }
                let name = e.local_name().into_inner().to_owned();
                root_is_tt.get_or_insert(name == "tt");
                stack.push(name);
            }
            Ok(Event::Empty(e)) => {
                if e.attributes().any(|a| a.is_err()) {
                    return false;
                }
                root_is_tt.get_or_insert(e.local_name().into_inner() == "tt");
            }
            Ok(Event::End(_)) => {
                if stack.pop().is_none() {
                    return false;
                }
            }
            Ok(Event::Text(t)) => {
                let inside_line = stack.iter().any(|name| name == "p");
                if inside_line && !t.trim().is_empty() {
                    line_with_text = true;
                }
            }
            // A CDATA section is text, as `DOMParser` reads it.
            Ok(Event::CData(c)) => {
                let inside_line = stack.iter().any(|name| name == "p");
                if inside_line && !c.into_inner().trim().is_empty() {
                    line_with_text = true;
                }
            }
            // quick-xml hands references over unresolved. A character
            // reference to a character XML allows (its `Char` production)
            // or one of XML's five predefined entities is text; any other
            // reference (an undefined name, `&#xZZ;`, `&#0;`, `&#1;`)
            // makes `DOMParser` reject the document whole, wherever it
            // appears.
            Ok(Event::GeneralRef(r)) => {
                let predefined = matches!(&*r, "lt" | "gt" | "amp" | "apos" | "quot");
                let legal_char = matches!(
                    r.resolve_char_ref(),
                    Ok(Some(
                        '\u{9}'
                        | '\u{A}'
                        | '\u{D}'
                        | '\u{20}'..='\u{D7FF}'
                        | '\u{E000}'..='\u{FFFD}'
                        | '\u{10000}'..,
                    ))
                );
                if !predefined && !legal_char {
                    return false;
                }
                if stack.iter().any(|name| name == "p") {
                    line_with_text = true;
                }
            }
            Ok(Event::Eof) => {
                return root_is_tt == Some(true) && stack.is_empty() && line_with_text
            }
            Err(_) => return false,
            _ => {}
        }
    }
}

/// A `<span>` carrying a `begin`: TTML timed below the line.
static TTML_TIMED_SPAN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"<(?:[A-Za-z_][\w.-]*:)?span\b[^>]*\bbegin\s*=").expect("static pattern")
});

/// Lyrics the renderer can follow: LRC or Enhanced LRC whose lines carry
/// real timing (see [`lrc_has_timing`]), or a well-formed TTML document with at least one
/// `<p>` whose `begin` it can read. Each rule mirrors the frontend parser,
/// because an answer judged synced here ends the waterfall, and one the
/// renderer then cannot follow is static text that beat synced lyrics.
/// Apple serves lyrics it has no timing for as TTML too, with
/// `itunes:timing="None"` and bare lines.
pub(super) fn lyrics_are_synced(format: &LyricsFormat, content: &str) -> bool {
    match format {
        LyricsFormat::Lrc | LyricsFormat::EnhancedLrc => lrc_has_timing(content),
        LyricsFormat::Plain => false,
        LyricsFormat::Ttml => ttml_has_timed_line(content),
    }
}

/// How much timing a lyrics document carries, as the renderer will
/// follow it. Ordered: word timing beats line timing, which beats none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LyricsQuality {
    /// Plain text, or a timed format the renderer shows as static text.
    Unsynced,
    /// Synced line by line.
    Lines,
    /// Synced word by word: Enhanced LRC, or TTML with a timed `<span>`.
    Words,
}

/// Rank a document by the timing the renderer can follow in it: unsynced
/// unless [`lyrics_are_synced`], then word-timed for Enhanced LRC and for
/// TTML with a timed `<span>`. The richest local sidecar and the plugin
/// answer kept are both chosen by it.
pub(super) fn lyrics_quality(format: &LyricsFormat, content: &str) -> LyricsQuality {
    if !lyrics_are_synced(format, content) {
        return LyricsQuality::Unsynced;
    }
    let words = match format {
        LyricsFormat::EnhancedLrc => true,
        LyricsFormat::Ttml => TTML_TIMED_SPAN.is_match(content),
        LyricsFormat::Lrc | LyricsFormat::Plain => false,
    };
    if words {
        LyricsQuality::Words
    } else {
        LyricsQuality::Lines
    }
}

/// `LRC_LINE_STAMP_RE` in `src/lib/tauri/lyrics.ts`: the renderer keeps a
/// line only when it carries a complete stamp of this shape.
static LRC_LINE_STAMP: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]").expect("static pattern")
});

/// Whether stamped LRC is actually timed. A complete line stamp is not
/// enough: some sources stamp every line `[00:00.00]`, which the renderer
/// shows as plain text (`isUntimedLrc` in `src/lib/tauri/lyrics.ts`), so
/// an answer like that must not end the search ahead of real timing. The
/// rule is the frontend's: untimed when more than half of the sung lines
/// (a stamp followed by text) share one time. Enhanced LRC word stamps at
/// two or more different times are timing however the lines are stamped:
/// those words still carry the karaoke. Only a stamp followed by text
/// counts; the one closing a line marks where it ends, not a word. Text
/// before a line's first word stamp is a word sung at the line's own time,
/// as `parseEnhancedLrc` reads it.
fn lrc_has_timing(content: &str) -> bool {
    let mut word_times = std::collections::HashSet::new();
    for line in content.lines() {
        // Read like the frontend: lines without a line stamp are skipped,
        // and the words are found in what remains once those are removed.
        let line_times: Vec<u64> = LRC_LINE_STAMP
            .captures_iter(line)
            .map(|stamp| lrc_stamp_ms(&stamp))
            .collect();
        if line_times.is_empty() {
            continue;
        }
        let body = LRC_LINE_STAMP.replace_all(line, "");
        let stamps: Vec<_> = LRC_WORD_STAMP.captures_iter(&body).collect();
        let Some(first) = stamps.first().and_then(|stamp| stamp.get(0)) else {
            continue;
        };
        if !body[..first.start()].trim().is_empty() {
            word_times.extend(line_times.iter().copied());
        }
        for (i, stamp) in stamps.iter().enumerate() {
            let from = stamp.get(0).map_or(body.len(), |m| m.end());
            let to = stamps
                .get(i + 1)
                .and_then(|next| next.get(0))
                .map_or(body.len(), |m| m.start());
            if !body[from..to].trim().is_empty() {
                word_times.insert(lrc_stamp_ms(stamp));
            }
        }
    }
    if word_times.len() >= 2 {
        return true;
    }
    let mut per_time: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut sung = 0usize;
    for line in content.lines() {
        if LRC_LINE_STAMP.replace_all(line, "").trim().is_empty() {
            continue;
        }
        for stamp in LRC_LINE_STAMP.captures_iter(line) {
            *per_time.entry(lrc_stamp_ms(&stamp)).or_default() += 1;
            sung += 1;
        }
    }
    let top = per_time.values().copied().max().unwrap_or(0);
    sung > 0 && !(sung >= 2 && top * 2 > sung)
}

/// An Enhanced LRC word stamp, `<mm:ss.xx>`, same fields as a line stamp.
static LRC_WORD_STAMP: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"<(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?>").expect("static pattern")
});

/// A captured `[mm:ss.xx]` or `<mm:ss.xx>` stamp in milliseconds. Only compared for
/// equality, but normalised the way the frontend reads it: a fraction of
/// one, two or three digits is tenths, hundredths or thousandths.
fn lrc_stamp_ms(stamp: &regex::Captures) -> u64 {
    let number = |i: usize| {
        stamp
            .get(i)
            .map_or(0, |m| m.as_str().parse::<u64>().unwrap_or(0))
    };
    let fraction = stamp.get(3).map_or(0, |m| {
        let digits = m.as_str();
        digits.parse::<u64>().unwrap_or(0) * 10u64.pow(3 - digits.len().min(3) as u32)
    });
    number(1) * 60_000 + number(2) * 1000 + fraction
}

fn ttml_has_timed_line(content: &str) -> bool {
    use quick_xml::events::Event;

    let mut reader = quick_xml::Reader::from_str(content);
    let mut depth: usize = 0;
    let mut timed = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                depth += 1;
                match is_timed_line(&e) {
                    Some(t) => timed |= t,
                    None => return false,
                }
            }
            Ok(Event::Empty(e)) => match is_timed_line(&e) {
                Some(t) => timed |= t,
                None => return false,
            },
            Ok(Event::End(_)) => match depth.checked_sub(1) {
                Some(d) => depth = d,
                None => return false,
            },
            Ok(Event::Eof) => return timed && depth == 0,
            Err(_) => return false,
            _ => {}
        }
    }
}

/// Whether this element is a `<p>` with a `begin` the renderer can read,
/// or `None` when one of its attributes is malformed (a duplicate, a
/// missing quote). Every element's attributes are checked, not only the
/// lines': `DOMParser` rejects the whole document for any of them.
fn is_timed_line(e: &quick_xml::events::BytesStart<'_>) -> Option<bool> {
    let is_line = e.local_name().into_inner() == "p";
    let mut timed = false;
    for attr in e.attributes() {
        let attr = attr.ok()?;
        if is_line
            && attr.key.local_name().into_inner() == "begin"
            && ttml_time_ms(&attr.value).is_some_and(|ms| ms >= -0.5)
        {
            timed = true;
        }
    }
    Some(timed)
}

/// `parseTtmlTime` in `src/lib/tauri/lyrics.ts`, which drops a line whose
/// `begin` it cannot read or that rounds below zero.
fn ttml_time_ms(value: &str) -> Option<f64> {
    // `Number("")` is 0 in JavaScript, which the renderer inherits.
    fn number(s: &str) -> Option<f64> {
        let t = s.trim();
        if t.is_empty() {
            return Some(0.0);
        }
        t.parse::<f64>().ok().filter(|n| n.is_finite())
    }
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(n) = s.strip_suffix("ms") {
        return number(n);
    }
    if let Some(n) = s.strip_suffix('s') {
        return number(n).map(|v| v * 1000.0);
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        return match parts.as_slice() {
            [m, sec] => Some(number(m)? * 60_000.0 + number(sec)? * 1000.0),
            [h, m, sec] => {
                Some(number(h)? * 3_600_000.0 + number(m)? * 60_000.0 + number(sec)? * 1000.0)
            }
            _ => None,
        };
    }
    number(s).map(|v| v * 1000.0)
}
