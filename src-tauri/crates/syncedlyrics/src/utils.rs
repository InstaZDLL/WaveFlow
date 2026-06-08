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
            rest
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    let body = &line.get(1..close)?;
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
            let Some(end) = rest.find('>') else { break };
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
}
