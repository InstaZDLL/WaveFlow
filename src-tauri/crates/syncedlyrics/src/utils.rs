#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsFormat {
    Plain,
    Lrc,
    EnhancedLrc,
}

pub(crate) fn synced_to_plaintext(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let mut rest = line.trim_start();
            while let Some(stripped) = strip_lrc_stamp(rest) {
                rest = stripped.trim_start();
            }
            // Enhanced LRC carries inline word-stamps (`<mm:ss.xx>`) that the
            // line-stamp strip above leaves untouched; drop them so plaintext
            // output never leaks timing tokens.
            strip_word_stamps(rest).trim().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove inline `<mm:ss.xx>` / `<mm:ss>` word-stamps from a line, leaving any
/// other angle-bracketed text intact.
fn strip_word_stamps(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(lt) = rest.find('<') {
        match rest[lt + 1..].find('>') {
            Some(gt_rel) => {
                let gt = lt + 1 + gt_rel;
                if is_time_token(&rest[lt + 1..gt]) {
                    out.push_str(&rest[..lt]);
                    rest = &rest[gt + 1..];
                } else {
                    out.push_str(&rest[..=lt]);
                    rest = &rest[lt + 1..];
                }
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// `true` when `s` looks like a timestamp body: `mm:ss` or `mm:ss.xx`.
fn is_time_token(s: &str) -> bool {
    let Some((minutes, after)) = s.split_once(':') else {
        return false;
    };
    if minutes.is_empty() || !minutes.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let seconds = match after.split_once('.') {
        Some((secs, frac)) => {
            if frac.is_empty() || !frac.bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            secs
        }
        None => after,
    };
    !seconds.is_empty() && seconds.bytes().all(|b| b.is_ascii_digit())
}

pub fn detect_format(content: &str) -> LyricsFormat {
    let mut has_line_stamp = false;
    for line in content.lines().take(40) {
        let line = line.trim_start();
        if strip_lrc_stamp(line).is_some() {
            has_line_stamp = true;
            if let Some(close) = line.find(']') {
                if word_stamp_present(&line[close + 1..]) {
                    return LyricsFormat::EnhancedLrc;
                }
            }
        }
    }
    if has_line_stamp {
        LyricsFormat::Lrc
    } else {
        LyricsFormat::Plain
    }
}

fn strip_lrc_stamp(line: &str) -> Option<&str> {
    let close = line.find(']')?;
    let body = line.get(1..close)?;
    let colon = body.find(':')?;
    if colon == 0 || !body[..colon].chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(&line[close + 1..])
}

fn word_stamp_present(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            let d1 = scan_digits(bytes, j);
            if d1 > 0 {
                j += d1;
                if bytes.get(j) == Some(&b':') {
                    j += 1;
                    let d2 = scan_digits(bytes, j);
                    if d2 > 0 {
                        j += d2;
                        if matches!(bytes.get(j), Some(b'.') | Some(b':')) {
                            j += 1 + scan_digits(bytes, j + 1);
                        }
                        if bytes.get(j) == Some(&b'>') {
                            return true;
                        }
                    }
                }
            }
        }
        i += 1;
    }
    false
}

fn scan_digits(bytes: &[u8], start: usize) -> usize {
    let mut n = 0;
    while matches!(bytes.get(start + n), Some(b) if b.is_ascii_digit()) {
        n += 1;
    }
    n
}

pub(crate) fn format_time(seconds: f64) -> String {
    let total_cs = (seconds * 100.0).floor().max(0.0) as u64;
    let minutes = total_cs / 6000;
    let secs = (total_cs / 100) % 60;
    let cs = total_cs % 100;
    format!("{minutes:02}:{secs:02}.{cs:02}")
}

pub(crate) fn str_score(a: &str, b: &str) -> f64 {
    let a_norm = normalize(a);
    let b_norm = normalize(b);
    let a_tokens = token_set(&a_norm);
    let b_tokens = token_set(&b_norm);
    if a_tokens.is_empty() && b_tokens.is_empty() {
        return 100.0;
    }
    if a_tokens.is_empty() || b_tokens.is_empty() {
        return 0.0;
    }
    let intersect = a_tokens.iter().filter(|tok| b_tokens.contains(tok)).count();
    if intersect == a_tokens.len().min(b_tokens.len()) {
        return 100.0;
    }
    let precision = intersect as f64 / a_tokens.len() as f64;
    let recall = intersect as f64 / b_tokens.len() as f64;
    if precision + recall == 0.0 {
        0.0
    } else {
        200.0 * precision * recall / (precision + recall)
    }
}

fn normalize(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() {
                c
            } else {
                ' '
            }
        })
        .collect()
}

fn token_set(input: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for tok in input.split_whitespace() {
        if !out.contains(&tok) {
            out.push(tok);
        }
    }
    out
}

pub(crate) fn html_text_decode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while !rest.is_empty() {
        if let Some(after) = rest
            .strip_prefix("<br/>")
            .or_else(|| rest.strip_prefix("<br>"))
        {
            out.push('\n');
            rest = after;
        } else if rest.starts_with('<') {
            // Malformed HTML (a `<` with no closing `>`) used to
            // `break` and truncate the remainder of the input —
            // costing every character after the stray `<`. Consume
            // the lone `<` as a literal instead and keep going so a
            // single rough byte in a scraped lyric page can't drop
            // the rest of the verse.
            let Some(end) = rest.find('>') else {
                out.push('<');
                rest = &rest[1..];
                continue;
            };
            rest = &rest[end + 1..];
        } else if let Some(after) = rest.strip_prefix("&amp;") {
            out.push('&');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("&lt;") {
            out.push('<');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("&gt;") {
            out.push('>');
            rest = after;
        } else if let Some(after) = rest.strip_prefix("&quot;") {
            out.push('"');
            rest = after;
        } else if let Some(after) = rest
            .strip_prefix("&#x27;")
            .or_else(|| rest.strip_prefix("&#39;"))
        {
            out.push('\'');
            rest = after;
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_subset_scores_like_query_match() {
        assert_eq!(
            str_score("Gloria Estefan - Get on Your Feet", "get on your feet"),
            100.0
        );
    }

    #[test]
    fn plaintext_drops_word_stamps() {
        let enhanced = "[00:01.00]<00:01.00>Hello <00:01.50>world";
        assert_eq!(synced_to_plaintext(enhanced), "Hello world");
    }

    #[test]
    fn plaintext_keeps_non_timestamp_brackets() {
        let line = "[00:01.00]a < b and c > d";
        assert_eq!(synced_to_plaintext(line), "a < b and c > d");
    }
}
