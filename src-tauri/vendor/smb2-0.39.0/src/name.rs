//! Mapping for filename characters SMB2 does not allow on the wire.
//!
//! SMB2 borrows its name syntax from Windows, so eight ASCII characters are
//! illegal in a file or directory name -- `"`, `*`, `:`, `<`, `>`, `?`, `\`,
//! and `|` are wildcards or separators in the protocol's own grammar -- along
//! with the control characters and a trailing space or period. A server asked
//! to create such a name answers `STATUS_OBJECT_NAME_INVALID` (0xC0000033) and
//! there is nothing the client can retry.
//!
//! Every SMB client that has to carry POSIX names solves this the same way, and
//! has since Services for Macintosh: substitute each illegal character with a
//! code point in the Unicode private-use area at U+F000, and substitute back on
//! the way in. The server stores an ordinary legal name and never knows. **No
//! server-side configuration is involved**, which is why matching the table
//! exactly matters more than choosing a good one: a file written by this crate
//! and the same file written by macOS Finder have to land on the same on-disk
//! name, or the two clients disagree about what a share contains.
//!
//! # The table
//!
//! | Local | Wire | | Local | Wire |
//! |---|---|---|---|---|
//! | `0x01`–`0x1F` | U+F001–U+F01F | | `?` | U+F025 |
//! | `"` | U+F020 | | `\` | U+F026 |
//! | `*` | U+F021 | | `\|` | U+F027 |
//! | `:` | U+F022 | | trailing ` ` | U+F028 |
//! | `<` | U+F023 | | trailing `.` | U+F029 |
//! | `>` | U+F024 | | | |
//!
//! The trailing rule applies to the **last character of each path component**
//! and to that character alone: `"a.. "` maps to `a..` + U+F028, not to three
//! substitutions. Leading and interior spaces and periods are legal and stay
//! as they are.
//!
//! Verified against macOS 15 smbfs writing to a QNAP TS-464 (Samba) share on
//! 2026-08-05, by creating probe files through the `/Volumes` mount and reading
//! the resulting on-disk bytes over SSH on the NAS. The real name that started
//! this, `"how_are_you_feeling?"_emojis.json`, is stored as
//! `ef80a0 how_are_you_feeling ef80a5 ef80a0 _emojis.json`.
//!
//! # Always on
//!
//! [`encode_path`] runs on every path this crate puts on the wire and
//! [`decode_name`] on every name it reads back; there is no switch. For a name
//! with no illegal character -- almost every name -- encoding is the identity
//! and the input string is returned unchanged, so the common case costs a scan.
//! For a name with one, the mapped form is the only form the server will
//! accept, so there is no competing "correct" behavior to offer. The functions
//! are public, so a consumer that wants to see or construct the literal wire
//! name can call them directly.
//!
//! # Where the round trip is not exact
//!
//! Both gaps are inherent to the scheme and both match what macOS does, which
//! is the point:
//!
//! - **A local name that already holds U+F001–U+F029 goes out unchanged**, so
//!   it reads back as the illegal character it stands for: `decode_name` of
//!   `encode_name("a\u{F025}b")` is `a?b`, not the original. Escaping it
//!   instead would put a name on the server that Finder renders differently.
//! - **U+F028 and U+F029 decode wherever they appear**, but only encode at the
//!   end of a component, so a wire name carrying one in the middle (which only
//!   a client that does not implement this scheme can produce) decodes to a
//!   space or period that re-encoding will not restore.
//!
//! Every other name round-trips exactly, in both directions.
//!
//! # Separators
//!
//! In this crate a `/` is the only path separator a caller can write, and a
//! `\` is an ordinary character in a name (it becomes U+F026). [`encode_path`]
//! splits on `/`, maps each component, and joins with the `\` the protocol
//! wants; [`decode_path`] does the reverse. A `.` or `..` component is passed
//! through untouched so relative paths keep working.
//!
//! U+F000 is deliberately **not** in the table, though the scheme nominally
//! pairs it with NUL. Decoding it would hand a caller a name with an embedded
//! NUL, which truncates in every C API it reaches -- macOS does exactly that
//! and produces a cut-off name. Leaving it alone keeps the mapping symmetric
//! and costs nothing, because no filesystem can hold a name containing NUL.
//!
//! This crate does **not** apply Unicode normalization. macOS smbfs converts
//! decomposed names to NFC before sending (verified the same way, same date),
//! because its local filesystem stores NFD; a cross-platform crate has no such
//! local form to convert from, and an NFD name is perfectly legal on the wire.
//! A consumer that wants byte-for-byte Finder parity on macOS should normalize
//! to NFC before calling.

use std::borrow::Cow;

/// First code point of the private-use block this scheme maps into.
const PUA_BASE: u32 = 0xF000;

/// Highest code point the scheme assigns (trailing period).
const PUA_LAST: u32 = 0xF029;

/// The wire form of a trailing space.
const WIRE_TRAILING_SPACE: char = '\u{F028}';

/// The wire form of a trailing period.
const WIRE_TRAILING_PERIOD: char = '\u{F029}';

/// Map one character that is illegal in *any* position of an SMB2 name.
///
/// `None` means the character is legal and travels as itself. The trailing
/// space and period are not here: they are only illegal at the end of a
/// component, so [`encode_name`] handles them positionally.
fn encode_char(c: char) -> Option<char> {
    match c {
        // Control characters. NUL is excluded on purpose; see the module docs.
        '\u{01}'..='\u{1F}' => char::from_u32(PUA_BASE + c as u32),
        '"' => Some('\u{F020}'),
        '*' => Some('\u{F021}'),
        ':' => Some('\u{F022}'),
        '<' => Some('\u{F023}'),
        '>' => Some('\u{F024}'),
        '?' => Some('\u{F025}'),
        '\\' => Some('\u{F026}'),
        '|' => Some('\u{F027}'),
        _ => None,
    }
}

/// Map one private-use code point back to the character it stands for.
///
/// `None` means the character is not part of the scheme and travels as itself,
/// including private-use code points outside U+F001–U+F029.
fn decode_char(c: char) -> Option<char> {
    match c {
        '\u{F001}'..='\u{F01F}' => char::from_u32(c as u32 - PUA_BASE),
        '\u{F020}' => Some('"'),
        '\u{F021}' => Some('*'),
        '\u{F022}' => Some(':'),
        '\u{F023}' => Some('<'),
        '\u{F024}' => Some('>'),
        '\u{F025}' => Some('?'),
        '\u{F026}' => Some('\\'),
        '\u{F027}' => Some('|'),
        '\u{F028}' => Some(' '),
        '\u{F029}' => Some('.'),
        _ => None,
    }
}

/// Map one path component to the form SMB2 accepts on the wire.
///
/// Takes a single name, never a path: a `/` is left alone here and a `\`
/// becomes U+F026, so handing this a path would turn its separators into name
/// characters. Use [`encode_path`] for anything with separators in it.
///
/// A name with nothing to map is returned borrowed and unchanged.
///
/// ```
/// # use smb2::name::encode_name;
/// assert_eq!(encode_name("report.pdf"), "report.pdf");
/// assert_eq!(encode_name("who?"), "who\u{F025}");
/// assert_eq!(encode_name("trailing "), "trailing\u{F028}");
/// ```
pub fn encode_name(name: &str) -> Cow<'_, str> {
    let ends_illegally = matches!(name.chars().next_back(), Some(' ') | Some('.'));
    if !ends_illegally && !name.chars().any(|c| encode_char(c).is_some()) {
        return Cow::Borrowed(name);
    }

    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        let is_last = chars.peek().is_none();
        let mapped = match c {
            ' ' if is_last => Some(WIRE_TRAILING_SPACE),
            '.' if is_last => Some(WIRE_TRAILING_PERIOD),
            _ => encode_char(c),
        };
        out.push(mapped.unwrap_or(c));
    }
    Cow::Owned(out)
}

/// Map one name that came off the wire back to the characters it stands for.
///
/// Takes a single name, never a path: U+F026 decodes to a `\`, which a path
/// would then be indistinguishable from a separator. Use [`decode_path`] for
/// a wire path.
///
/// A name with nothing to map is returned borrowed and unchanged.
///
/// ```
/// # use smb2::name::decode_name;
/// assert_eq!(decode_name("report.pdf"), "report.pdf");
/// assert_eq!(decode_name("who\u{F025}"), "who?");
/// ```
pub fn decode_name(name: &str) -> Cow<'_, str> {
    if !name.chars().any(is_mapped) {
        return Cow::Borrowed(name);
    }
    Cow::Owned(
        name.chars()
            .map(|c| decode_char(c).unwrap_or(c))
            .collect::<String>(),
    )
}

/// Cheap pre-check for the borrowed fast path: is this code point in the block
/// the scheme uses at all?
fn is_mapped(c: char) -> bool {
    let v = c as u32;
    v > PUA_BASE && v <= PUA_LAST
}

/// Turn a caller's path into the wire path SMB2 wants.
///
/// Splits on `/`, maps each component with [`encode_name`], and joins with
/// `\`. Leading `/` characters are dropped, because SMB2 paths are relative to
/// the share root. A `.` or `..` component passes through untouched.
///
/// ```
/// # use smb2::name::encode_path;
/// assert_eq!(encode_path("/reports/q3.pdf"), "reports\\q3.pdf");
/// assert_eq!(encode_path("notes/who?.txt"), "notes\\who\u{F025}.txt");
/// ```
pub fn encode_path(path: &str) -> String {
    let path = path.trim_start_matches('/');
    let mut out = String::with_capacity(path.len());
    for (i, component) in path.split('/').enumerate() {
        if i > 0 {
            out.push('\\');
        }
        if component == "." || component == ".." {
            out.push_str(component);
        } else {
            out.push_str(&encode_name(component));
        }
    }
    out
}

/// Turn a wire path back into a caller's path.
///
/// Splits on `\`, maps each component with [`decode_name`], and joins with
/// `/`, so the result can be handed straight back to any method on
/// [`Tree`](crate::Tree).
///
/// ```
/// # use smb2::name::decode_path;
/// assert_eq!(decode_path("notes\\who\u{F025}.txt"), "notes/who?.txt");
/// ```
pub fn decode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (i, component) in path.split('\\').enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&decode_name(component));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UTF-16LE bytes a name occupies on the wire.
    fn wire_bytes(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    // ── The table, character by character ─────────────────────────────

    #[test]
    fn every_illegal_character_maps_to_its_documented_code_point() {
        let table = [
            ('"', '\u{F020}'),
            ('*', '\u{F021}'),
            (':', '\u{F022}'),
            ('<', '\u{F023}'),
            ('>', '\u{F024}'),
            ('?', '\u{F025}'),
            ('\\', '\u{F026}'),
            ('|', '\u{F027}'),
        ];
        for (local, wire) in table {
            assert_eq!(
                encode_name(&format!("a{local}b")),
                format!("a{wire}b"),
                "encoding {local:?}"
            );
            assert_eq!(
                decode_name(&format!("a{wire}b")),
                format!("a{local}b"),
                "decoding {wire:?}"
            );
        }
    }

    #[test]
    fn control_characters_map_to_the_matching_offset() {
        for code in 0x01u32..=0x1F {
            let local = char::from_u32(code).expect("ASCII control is a char");
            let wire = char::from_u32(PUA_BASE + code).expect("private use is a char");
            assert_eq!(encode_name(&format!("a{local}b")), format!("a{wire}b"));
            assert_eq!(decode_name(&format!("a{wire}b")), format!("a{local}b"));
        }
    }

    #[test]
    fn nul_is_not_part_of_the_table() {
        // Encoding NUL would be dead code (no filesystem holds one) and
        // decoding U+F000 would hand the caller a name that truncates in C.
        assert_eq!(encode_name("a\u{0}b"), "a\u{0}b");
        assert_eq!(decode_name("a\u{F000}b"), "a\u{F000}b");
    }

    #[test]
    fn private_use_code_points_outside_the_table_are_left_alone() {
        for c in ['\u{F02A}', '\u{F0FF}', '\u{F8FF}'] {
            assert_eq!(decode_name(&format!("a{c}b")), format!("a{c}b"));
            assert_eq!(encode_name(&format!("a{c}b")), format!("a{c}b"));
        }
    }

    // ── Position matters for space and period, and only for those ─────

    #[test]
    fn only_the_final_space_or_period_of_a_component_is_mapped() {
        assert_eq!(encode_name("ab "), "ab\u{F028}");
        assert_eq!(encode_name("ab  "), "ab \u{F028}");
        assert_eq!(encode_name("ab."), "ab\u{F029}");
        assert_eq!(encode_name("ab..."), "ab..\u{F029}");
        assert_eq!(encode_name("ab. "), "ab.\u{F028}");
        assert_eq!(encode_name("ab ."), "ab \u{F029}");
        assert_eq!(encode_name(" ab"), " ab");
        assert_eq!(encode_name(".ab"), ".ab");
        assert_eq!(encode_name("a b"), "a b");
        assert_eq!(encode_name("a.b"), "a.b");
        assert_eq!(encode_name(" "), "\u{F028}");
        assert_eq!(encode_name("."), "\u{F029}");
    }

    #[test]
    fn a_trailing_marker_decodes_wherever_it_appears() {
        // macOS does the same, verified on 2026-08-05: a wire name with
        // U+F028 in the middle reads back with a space in the middle.
        assert_eq!(decode_name("a\u{F028}b"), "a b");
        assert_eq!(decode_name("a\u{F029}b"), "a.b");
    }

    // ── Names that must not change ────────────────────────────────────

    #[test]
    fn ordinary_names_are_returned_borrowed_and_unchanged() {
        for name in [
            "report.pdf",
            "",
            "日本語テスト.txt",
            "café",
            "café",
            "📁 folder",
            "a-name_with (punctuation)!#$%&'+,;=[]{}~`^@",
            "документ.txt",
        ] {
            assert!(
                matches!(encode_name(name), Cow::Borrowed(_)),
                "{name:?} should not allocate"
            );
            assert_eq!(encode_name(name), name);
            assert!(matches!(decode_name(name), Cow::Borrowed(_)));
            assert_eq!(decode_name(name), name);
        }
    }

    #[test]
    fn combining_marks_and_astral_planes_survive_a_name_that_is_mapped() {
        // The mapping is per code point, so a name that needs one substitution
        // must not disturb anything else in it.
        let local = "e\u{301}mo\u{1F600}ji?\u{1F469}\u{200D}\u{1F4BB}";
        let wire = "e\u{301}mo\u{1F600}ji\u{F025}\u{1F469}\u{200D}\u{1F4BB}";
        assert_eq!(encode_name(local), wire);
        assert_eq!(decode_name(wire), local);
    }

    // ── Paths ─────────────────────────────────────────────────────────

    #[test]
    fn each_component_is_mapped_on_its_own() {
        assert_eq!(encode_path("a?b/c*d"), "a\u{F025}b\\c\u{F021}d");
        // The trailing rule is per component, not per path.
        assert_eq!(encode_path("dir /file. "), "dir\u{F028}\\file.\u{F028}");
        assert_eq!(encode_path("dir./sub /x"), "dir\u{F029}\\sub\u{F028}\\x");
    }

    #[test]
    fn a_backslash_in_a_name_is_a_name_character_not_a_separator() {
        assert_eq!(encode_path("a\\b"), "a\u{F026}b");
        assert_eq!(encode_path("dir/a\\b"), "dir\\a\u{F026}b");
        assert_eq!(decode_path("dir\\a\u{F026}b"), "dir/a\\b");
    }

    #[test]
    fn leading_slashes_are_dropped_and_other_separators_are_kept() {
        assert_eq!(encode_path("/leading/slash"), "leading\\slash");
        assert_eq!(encode_path("///leading"), "leading");
        assert_eq!(encode_path("foo/bar/baz"), "foo\\bar\\baz");
        assert_eq!(encode_path("no_change"), "no_change");
        assert_eq!(encode_path(""), "");
        // A trailing separator keeps its empty component, as it always has.
        assert_eq!(encode_path("projects/"), "projects\\");
    }

    #[test]
    fn relative_components_pass_through() {
        assert_eq!(encode_path("a/../b"), "a\\..\\b");
        assert_eq!(encode_path("./a"), ".\\a");
        // ...which is the whole reason they are special-cased: the trailing
        // rule would otherwise turn `..` into a name.
        assert_eq!(encode_name(".."), ".\u{F029}");
    }

    #[test]
    fn decode_path_hands_back_something_the_tree_api_accepts() {
        let wire = "sub\u{F025}dir\\lea\u{F021}f. ";
        let caller = decode_path(wire);
        assert_eq!(caller, "sub?dir/lea*f. ");
        assert_eq!(
            encode_path(&caller),
            "sub\u{F025}dir\\lea\u{F021}f.\u{F028}"
        );
    }

    // ── The interop contract: exact bytes ─────────────────────────────

    #[test]
    fn the_real_failing_name_matches_the_bytes_macos_wrote() {
        // Read off a QNAP TS-464 over SSH on 2026-08-05, after macOS smbfs
        // created the file through a /Volumes mount: the on-disk name is
        // ef80a0 how_are_you_feeling ef80a5 ef80a0 _emojis.json (UTF-8).
        let encoded = encode_name("\"how_are_you_feeling?\"_emojis.json");
        let mut expected = Vec::new();
        expected.extend_from_slice(&[0xEF, 0x80, 0xA0]);
        expected.extend_from_slice(b"how_are_you_feeling");
        expected.extend_from_slice(&[0xEF, 0x80, 0xA5]);
        expected.extend_from_slice(&[0xEF, 0x80, 0xA0]);
        expected.extend_from_slice(b"_emojis.json");
        assert_eq!(encoded.as_bytes(), expected.as_slice());
    }

    #[test]
    fn the_wire_bytes_of_every_mapped_character_are_pinned() {
        // UTF-16LE, which is what goes into a CREATE. Written out literally so
        // a change to the table cannot pass by editing a constant.
        assert_eq!(wire_bytes(&encode_name("\"")), [0x20, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("*")), [0x21, 0xF0]);
        assert_eq!(wire_bytes(&encode_name(":")), [0x22, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("<")), [0x23, 0xF0]);
        assert_eq!(wire_bytes(&encode_name(">")), [0x24, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("?")), [0x25, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("\\")), [0x26, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("|")), [0x27, 0xF0]);
        assert_eq!(wire_bytes(&encode_name(" ")), [0x28, 0xF0]);
        assert_eq!(wire_bytes(&encode_name(".")), [0x29, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("\u{01}")), [0x01, 0xF0]);
        assert_eq!(wire_bytes(&encode_name("\u{1F}")), [0x1F, 0xF0]);
    }

    #[test]
    fn the_utf8_bytes_samba_stores_are_pinned() {
        // What `ls` on the server shows, which is the form a second client
        // (Finder, another smb2 consumer) has to agree with.
        let cases: [(&str, &[u8]); 10] = [
            ("\"", &[0xEF, 0x80, 0xA0]),
            ("*", &[0xEF, 0x80, 0xA1]),
            (":", &[0xEF, 0x80, 0xA2]),
            ("<", &[0xEF, 0x80, 0xA3]),
            (">", &[0xEF, 0x80, 0xA4]),
            ("?", &[0xEF, 0x80, 0xA5]),
            ("\\", &[0xEF, 0x80, 0xA6]),
            ("|", &[0xEF, 0x80, 0xA7]),
            (" ", &[0xEF, 0x80, 0xA8]),
            (".", &[0xEF, 0x80, 0xA9]),
        ];
        for (local, utf8) in cases {
            assert_eq!(encode_name(local).as_bytes(), utf8, "for {local:?}");
        }
    }

    // ── Round trips ───────────────────────────────────────────────────

    #[test]
    fn every_name_without_private_use_code_points_round_trips_exactly() {
        for name in [
            "a\"b*c:d<e>f?g\\h|i",
            "trailing ",
            "trailing.",
            "\u{01}\u{1F}leading control",
            "日本語?テスト. ",
            "",
            "?",
            "\u{7F}",
        ] {
            assert_eq!(decode_name(&encode_name(name)), name, "for {name:?}");
        }
    }

    #[test]
    fn a_name_that_already_holds_a_mapped_code_point_is_not_round_trip_stable() {
        // Documented and deliberate: escaping it would put a name on the
        // server that Finder renders as something else.
        assert_eq!(encode_name("a\u{F025}b"), "a\u{F025}b");
        assert_eq!(decode_name(&encode_name("a\u{F025}b")), "a?b");
    }

    #[test]
    fn paths_round_trip_through_both_helpers() {
        for path in [
            "a?b/c*d",
            "dir /file. ",
            "plain/path/file.txt",
            "a\\b/c",
            "one",
        ] {
            assert_eq!(decode_path(&encode_path(path)), path, "for {path:?}");
        }
    }
}

#[cfg(test)]
mod round_trip_props {
    use super::*;
    use proptest::prelude::*;

    /// Any name a filesystem could hold, minus the code points the scheme
    /// itself owns (which are documented as not round-trip stable) and the
    /// separator this crate reserves.
    fn arb_name() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            any::<char>().prop_filter("reserved by the scheme", |c| {
                let v = *c as u32;
                *c != '/' && *c != '\u{0}' && !(PUA_BASE..=PUA_LAST).contains(&v)
            }),
            0..24,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    /// The control range the scheme maps. U+007F and the C1 block are not in
    /// it and are legal on the wire, so they must survive untouched.
    fn is_mapped_control(c: char) -> bool {
        ('\u{01}'..='\u{1F}').contains(&c)
    }

    /// The same, but never empty, so a path built from these has no leading
    /// separator for `encode_path` to strip.
    fn arb_component() -> impl Strategy<Value = String> {
        arb_name().prop_filter("a component has a name", |s| !s.is_empty())
    }

    proptest! {
        #[test]
        fn decode_undoes_encode_for_any_name(name in arb_name()) {
            let encoded = encode_name(&name);
            let decoded = decode_name(&encoded).into_owned();
            prop_assert_eq!(decoded, name);
        }

        #[test]
        fn an_encoded_name_carries_nothing_smb2_rejects(name in arb_name()) {
            let encoded = encode_name(&name);
            let has_illegal = encoded.contains(['"', '*', ':', '<', '>', '?', '\\', '|']);
            let has_control = encoded.chars().any(is_mapped_control);
            let ends_illegally = matches!(encoded.chars().next_back(), Some(' ') | Some('.'));
            prop_assert!(!has_illegal);
            prop_assert!(!has_control);
            prop_assert!(!ends_illegally);
        }

        #[test]
        fn a_path_survives_both_directions(
            components in proptest::collection::vec(arb_component(), 1..4),
        ) {
            let path = components.join("/");
            let encoded = encode_path(&path);
            prop_assert_eq!(decode_path(&encoded), path);
        }
    }
}
