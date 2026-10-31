//! Timestamps in the notations each format uses.

/// Parse a TTML time expression into milliseconds.
///
/// Accepts the clock forms (`h:mm:ss.fff`, `m:ss.fff`, `ss.fff`) and the
/// offset forms with a metric (`12.5s`, `1500ms`, `2m`, `1h`). Frames and
/// ticks (`f`, `t`) need a frame rate the documents we read never declare,
/// so they are refused rather than guessed.
pub fn parse_ttml_time(value: &str) -> Option<u64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if v.contains(':') {
        let parts: Vec<&str> = v.split(':').collect();
        if parts.len() > 3 {
            return None;
        }
        let (whole, last) = parts.split_at(parts.len() - 1);
        let seconds = parse_non_negative(last[0])?;
        let mut total = 0.0;
        for part in whole {
            let n: u64 = part.trim().parse().ok()?;
            total = total * 60.0 + n as f64;
        }
        return to_ms(total * 60.0 + seconds);
    }
    let (number, scale) = if let Some(n) = v.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = v.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = v.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = v.strip_suffix('h') {
        (n, 3600.0)
    } else if v.ends_with('f') || v.ends_with('t') {
        return None;
    } else {
        (v, 1.0)
    };
    to_ms(parse_non_negative(number)? * scale)
}

fn parse_non_negative(value: &str) -> Option<f64> {
    let n: f64 = value.trim().parse().ok()?;
    (n.is_finite() && n >= 0.0).then_some(n)
}

fn to_ms(seconds: f64) -> Option<u64> {
    let ms = (seconds * 1000.0).round();
    (ms.is_finite() && ms >= 0.0 && ms < u64::MAX as f64).then_some(ms as u64)
}

/// Write milliseconds as a TTML clock value: `m:ss.mmm`, with hours
/// (`h:mm:ss.mmm`) only once the song is that long.
pub fn format_ttml_time(ms: u64) -> String {
    let millis = ms % 1000;
    let total_seconds = ms / 1000;
    let seconds = total_seconds % 60;
    let total_minutes = total_seconds / 60;
    if total_minutes >= 60 {
        format!(
            "{}:{:02}:{:02}.{:03}",
            total_minutes / 60,
            total_minutes % 60,
            seconds,
            millis
        )
    } else {
        format!("{total_minutes}:{seconds:02}.{millis:03}")
    }
}

/// Parse an LRC timestamp body (`mm:ss.xx`, `mm:ss.xxx`, `mm:ss`), the
/// part between the brackets. Minutes may exceed 59.
pub fn parse_lrc_time(value: &str) -> Option<u64> {
    let (minutes, rest) = value.trim().split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, Some(f)),
        None => (rest, None),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let fraction_ms = match fraction {
        None => 0,
        Some(f) => {
            let f = f.trim();
            if f.is_empty() || f.len() > 3 || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // "5" is half a second, "05" five hundredths, "005" five ms.
            let n: u64 = f.parse().ok()?;
            n * 10u64.pow(3 - f.len() as u32)
        }
    };
    // Minutes come from the file: one too large to hold is no time.
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1000 + fraction_ms)
}

/// Write milliseconds as an LRC timestamp body, `mm:ss.xx` (hundredths,
/// what nearly every player reads) or `mm:ss.xxx`.
pub fn format_lrc_time(ms: u64, millis: bool) -> String {
    if millis {
        let minutes = ms / 60_000;
        let seconds = (ms / 1000) % 60;
        return format!("{minutes:02}:{seconds:02}.{:03}", ms % 1000);
    }
    let cs = (ms + 5) / 10;
    let minutes = cs / 6000;
    let seconds = (cs / 100) % 60;
    format!("{minutes:02}:{seconds:02}.{:02}", cs % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttml_clock_forms() {
        assert_eq!(parse_ttml_time("1:02.345"), Some(62_345));
        assert_eq!(parse_ttml_time("01:02:03.5"), Some(3_723_500));
        assert_eq!(parse_ttml_time("12.5"), Some(12_500));
        assert_eq!(parse_ttml_time("12.5s"), Some(12_500));
        assert_eq!(parse_ttml_time("1500ms"), Some(1_500));
        assert_eq!(parse_ttml_time("2m"), Some(120_000));
        assert_eq!(parse_ttml_time("25f"), None);
        assert_eq!(parse_ttml_time("-1s"), None);
        assert_eq!(parse_ttml_time(""), None);
        assert_eq!(parse_ttml_time("1:2:3:4"), None);
    }

    #[test]
    fn ttml_clock_round_trip() {
        for ms in [0, 999, 62_345, 3_599_999, 3_723_500] {
            assert_eq!(parse_ttml_time(&format_ttml_time(ms)), Some(ms));
        }
        assert_eq!(format_ttml_time(62_345), "1:02.345");
        assert_eq!(format_ttml_time(3_723_500), "1:02:03.500");
    }

    #[test]
    fn lrc_forms() {
        assert_eq!(parse_lrc_time("01:02.34"), Some(62_340));
        assert_eq!(parse_lrc_time("01:02.345"), Some(62_345));
        assert_eq!(parse_lrc_time("01:02.5"), Some(62_500));
        assert_eq!(parse_lrc_time("01:02"), Some(62_000));
        assert_eq!(parse_lrc_time("75:00.00"), Some(4_500_000));
        assert_eq!(parse_lrc_time("01:60.00"), None);
        assert_eq!(parse_lrc_time("ti:Song"), None);
        // Too many minutes to hold in milliseconds.
        assert_eq!(parse_lrc_time("99999999999999999:00.00"), None);
        assert_eq!(
            parse_lrc_time("307445734561825:00.00"),
            Some(18_446_744_073_709_500_000)
        );
    }

    #[test]
    fn lrc_formatting_rounds_to_hundredths() {
        assert_eq!(format_lrc_time(62_345, false), "01:02.35");
        assert_eq!(format_lrc_time(59_996, false), "01:00.00");
        assert_eq!(format_lrc_time(62_345, true), "01:02.345");
    }
}
