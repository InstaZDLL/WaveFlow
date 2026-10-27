mod richsync_tests {
    //! Tests for the regression #5 scenarios. Exercise the shared
    //! [`validate_rows`] helper directly so production + tests run
    //! against a single source of truth — when the production path
    //! evolves, the tests follow automatically.

    use super::super::{richsync_line, validate_rows};
    use serde_json::json;

    #[test]
    fn segments_are_written_unpadded() {
        // Invented text. The space between two words is a segment of its
        // own; nothing is added around a segment.
        let words = [
            json!({"c": "Hel", "o": 0.0}),
            json!({"c": "lo", "o": 0.25}),
            json!({"c": " ", "o": 0.5}),
            json!({"c": "you", "o": 0.5}),
        ];
        assert_eq!(
            richsync_line(1.0, Some(&words)),
            "[00:01.00]<00:01.00>Hel<00:01.25>lo<00:01.50> <00:01.50>you"
        );
        let characters = [json!({"c": "一", "o": 0.0}), json!({"c": "二", "o": 0.4})];
        assert_eq!(
            richsync_line(2.0, Some(&characters)),
            "[00:02.00]<00:02.00>一<00:02.40>二"
        );
        assert_eq!(richsync_line(3.0, None), "[00:03.00]");
    }

    #[test]
    fn rejects_all_zero_timestamps() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 0.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_string_timestamps() {
        // Musixmatch occasionally returns `ts` as a JSON string. The
        // pre-fix code coerced to `0.0`; the fix must reject.
        let rows = vec![
            json!({"ts": "1.0", "l": []}),
            json!({"ts": "2.0", "l": []}),
            json!({"ts": "3.0", "l": []}),
            json!({"ts": "4.0", "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_too_few_rows() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn rejects_majority_duplicate_timestamps() {
        // 5 rows, only 2 unique → 40 % unique, below the 80 % gate.
        let rows = vec![
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }

    #[test]
    fn accepts_well_formed_rows() {
        let rows = vec![
            json!({"ts": 0.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
            json!({"ts": 4.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_some());
    }

    #[test]
    fn rejects_negative_or_non_finite_timestamps() {
        let rows = vec![
            json!({"ts": -1.0, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());

        let rows = vec![
            json!({"ts": f64::NAN, "l": []}),
            json!({"ts": 1.0, "l": []}),
            json!({"ts": 2.0, "l": []}),
            json!({"ts": 3.0, "l": []}),
        ];
        assert!(validate_rows(&rows).is_none());
    }
}

mod redact_tests {
    //! GH #234 — Musixmatch passes `usertoken` as a query parameter.
    //! Verify the redaction path: even when the underlying
    //! `reqwest::Error` would normally Display with the full URL,
    //! the error returned by [`redacted_get`] must not contain the
    //! literal token.
    //!
    //! Tests don't hit the real Musixmatch — they point `redacted_get`
    //! at an unresolvable hostname so `send().await` errors out
    //! deterministically with a DNS/connect failure. The exact error
    //! reason doesn't matter; the property we assert is "the token
    //! string never appears in the formatted error".

    use super::super::redacted_get;

    const SECRET: &str = "deadbeefcafe1234usertokenSECRET";

    fn client() -> reqwest::Client {
        // Short timeouts so the test fails fast on real networks too.
        reqwest::Client::builder()
            // A system proxy would answer for `.invalid` and hide the DNS
            // failure these tests are about.
            .no_proxy()
            .connect_timeout(std::time::Duration::from_millis(500))
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .expect("client builds")
    }

    #[tokio::test]
    async fn send_failure_drops_token_from_displayed_error() {
        // `.invalid` is RFC 6761-reserved — guaranteed never to
        // resolve, so the DNS layer rejects deterministically.
        let url = "https://musixmatch-token-redact-test.invalid/track.search".to_string();
        let err = redacted_get(
            &client(),
            url,
            &[
                ("q", "any"),
                ("usertoken", SECRET),
                ("app_id", "web-desktop-app-v1.0"),
            ],
            "test endpoint",
        )
        .await
        .expect_err("DNS for .invalid must fail");
        let rendered = format!("{err}");
        assert!(
            !rendered.contains(SECRET),
            "token leaked into formatted error: {rendered}",
        );
        // Sanity: the redacted host + path should still be there for
        // operator diagnostics.
        assert!(
            rendered.contains("musixmatch-token-redact-test.invalid"),
            "redacted host context missing: {rendered}",
        );
        assert!(
            rendered.contains("/track.search"),
            "redacted path context missing: {rendered}",
        );
    }

    /// The cause survives the redaction: a caller walking `source()`
    /// reaches the DNS failure, and no layer of it carries the token.
    #[tokio::test]
    async fn send_failure_keeps_its_cause_in_the_chain() {
        let url = "https://musixmatch-token-redact-test.invalid/track.search".to_string();
        // Not `client()`: its 500 ms would cut a Windows lookup (DNS
        // suffixes, LLMNR: over 11 s) and leave a timeout where this test
        // asks for the DNS failure. The lookup always ends.
        let slow = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("client builds");
        let err = redacted_get(&slow, url, &[("usertoken", SECRET)], "test endpoint")
            .await
            .expect_err("DNS for .invalid must fail");
        let mut chain = Vec::new();
        let mut layer: Option<&dyn std::error::Error> = Some(&err);
        while let Some(current) = layer {
            chain.push(current.to_string());
            layer = current.source();
        }
        let chain = chain.join(": ");
        assert!(chain.contains("dns error"), "cause lost: {chain}");
        assert!(
            !chain.contains(SECRET),
            "token leaked into the chain: {chain}"
        );
    }

    #[tokio::test]
    async fn debug_formatted_error_also_drops_token() {
        // Belt-and-suspenders: some `tracing::warn!(?err, …)` sites
        // use `Debug` rather than `Display`. The `without_url()`
        // strip must hold there too.
        let url = "https://musixmatch-token-redact-test.invalid/track.search".to_string();
        let err = redacted_get(&client(), url, &[("usertoken", SECRET)], "test endpoint")
            .await
            .expect_err("DNS for .invalid must fail");
        let rendered = format!("{err:?}");
        assert!(
            !rendered.contains(SECRET),
            "token leaked into Debug-formatted error: {rendered}",
        );
        // Symmetry with `send_failure_drops_token_from_displayed_error`:
        // the diagnostic context (redacted host + path) must survive
        // through the Debug formatter so an operator chasing a
        // `tracing::warn!(?err, …)` line in the rolling log still
        // sees WHICH endpoint failed.
        assert!(
            rendered.contains("musixmatch-token-redact-test.invalid"),
            "redacted host context missing from Debug: {rendered}",
        );
        assert!(
            rendered.contains("/track.search"),
            "redacted path context missing from Debug: {rendered}",
        );
    }
}

mod split_lrc_line_tests {
    use super::super::split_lrc_line;

    #[test]
    fn peels_single_numeric_stamp() {
        let (stamps, text) = split_lrc_line("[00:12.34]Hello world");
        assert_eq!(stamps, vec!["[00:12.34]"]);
        assert_eq!(text, "Hello world");
    }

    #[test]
    fn peels_multi_stamp_chorus_line() {
        let (stamps, text) = split_lrc_line("[00:30.10][01:05.40][02:11.00]Chorus line");
        assert_eq!(stamps, vec!["[00:30.10]", "[01:05.40]", "[02:11.00]"]);
        assert_eq!(text, "Chorus line");
    }

    #[test]
    fn preserves_header_tags() {
        // `[ar:Artist]` starts with a letter, not a digit — must stay
        // in the text half so the LRC header lines round-trip.
        let (stamps, text) = split_lrc_line("[ar:The Artist]");
        assert!(stamps.is_empty());
        assert_eq!(text, "[ar:The Artist]");
    }

    #[test]
    fn handles_empty_line() {
        let (stamps, text) = split_lrc_line("");
        assert!(stamps.is_empty());
        assert_eq!(text, "");
    }

    #[test]
    fn handles_stamp_only_line() {
        // Musixmatch occasionally returns instrumental marker lines
        // as bare stamps with empty text.
        let (stamps, text) = split_lrc_line("[01:30.00]");
        assert_eq!(stamps, vec!["[01:30.00]"]);
        assert_eq!(text, "");
    }

    #[test]
    fn accepts_seconds_only_stamp() {
        // LRC dialect with `[ss]` instead of `[mm:ss.xx]` is rare but
        // still starts with a digit — must be peeled too.
        let (stamps, text) = split_lrc_line("[00]intro");
        assert_eq!(stamps, vec!["[00]"]);
        assert_eq!(text, "intro");
    }
}

mod merge_translations_tests {
    use super::super::split_lrc_line;
    use std::collections::HashMap;

    // Pure-helper variant of the merge loop in `add_translations`,
    // factored here so the test doesn't have to roundtrip through
    // a real Musixmatch HTTP call. Mirrors the exact algorithm
    // (split → lookup trim → re-emit under same stamps).
    fn merge(lrc: &str, table: &HashMap<String, String>) -> String {
        let mut out = String::with_capacity(lrc.len() * 2);
        for raw_line in lrc.split_inclusive('\n') {
            let line_terminator: &str = raw_line
                .find(['\r', '\n'])
                .map(|idx| &raw_line[idx..])
                .unwrap_or("");
            let line_no_eol = raw_line.trim_end_matches(['\r', '\n']);
            out.push_str(line_no_eol);
            out.push_str(line_terminator);
            let (stamps, text) = split_lrc_line(line_no_eol);
            if stamps.is_empty() {
                continue;
            }
            let lookup = text.trim();
            if lookup.is_empty() {
                continue;
            }
            let Some(translated) = table.get(lookup) else {
                continue;
            };
            for stamp in &stamps {
                out.push_str(stamp);
            }
            out.push('(');
            out.push_str(translated);
            out.push(')');
            out.push_str(line_terminator);
        }
        out
    }

    #[test]
    fn injects_translation_below_original_with_same_stamp() {
        let lrc = "[00:12.34]Hello world\n[00:15.00]How are you\n";
        let mut table = HashMap::new();
        table.insert("Hello world".into(), "Bonjour le monde".into());
        table.insert("How are you".into(), "Comment ça va".into());
        let out = merge(lrc, &table);
        assert_eq!(
            out,
            "[00:12.34]Hello world\n[00:12.34](Bonjour le monde)\n[00:15.00]How are you\n[00:15.00](Comment ça va)\n",
        );
    }

    #[test]
    fn skips_lines_without_a_match() {
        let lrc = "[00:12.34]Untranslated line\n";
        let table = HashMap::new();
        let out = merge(lrc, &table);
        assert_eq!(out, lrc);
    }

    #[test]
    fn preserves_lrc_header_tags() {
        let lrc = "[ti:Song Title]\n[ar:The Artist]\n[00:12.34]Hello world\n";
        let mut table = HashMap::new();
        table.insert("Hello world".into(), "Salut".into());
        let out = merge(lrc, &table);
        assert!(out.contains("[ti:Song Title]"));
        assert!(out.contains("[ar:The Artist]"));
        assert!(out.contains("[00:12.34](Salut)"));
    }

    #[test]
    fn handles_multi_stamp_chorus() {
        let lrc = "[00:30.10][01:05.40]Chorus\n";
        let mut table = HashMap::new();
        table.insert("Chorus".into(), "Refrain".into());
        let out = merge(lrc, &table);
        // Translation rides under the SAME compound stamp prefix so
        // an LRC reader that stable-sorts by stamp groups the pair.
        assert_eq!(
            out,
            "[00:30.10][01:05.40]Chorus\n[00:30.10][01:05.40](Refrain)\n",
        );
    }

    #[test]
    fn ignores_empty_text_lines() {
        // A bare-stamp instrumental marker has nothing to translate.
        let lrc = "[01:30.00]\n";
        let mut table = HashMap::new();
        table.insert("".into(), "should not appear".into());
        let out = merge(lrc, &table);
        assert_eq!(out, "[01:30.00]\n");
    }

    #[test]
    fn round_trips_crlf_line_endings() {
        let lrc = "[00:12.34]Hello\r\n[00:15.00]World\r\n";
        let mut table = HashMap::new();
        table.insert("Hello".into(), "Salut".into());
        let out = merge(lrc, &table);
        assert_eq!(
            out,
            "[00:12.34]Hello\r\n[00:12.34](Salut)\r\n[00:15.00]World\r\n",
        );
    }
}
