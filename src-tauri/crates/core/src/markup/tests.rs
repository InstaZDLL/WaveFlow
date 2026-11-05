use super::*;

#[test]
fn inline_tags_keep_a_word_whole_and_block_tags_separate_lines() {
    assert_eq!(html_to_line("a <b>b</b>  c", 100), "a b c");
    assert_eq!(html_to_line("<b>bo</b>ld", 100), "bold");
    assert_eq!(html_to_line("<p>one</p><p>two</p>", 100), "one two");
    assert_eq!(
        html_to_text("<p>one</p><p>two<br/>three</p>")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
}

#[test]
fn what_is_not_a_tag_keeps_its_text() {
    assert_eq!(html_to_line("I <3 you", 100), "I <3 you");
    assert_eq!(html_to_line("a < b and c", 100), "a < b and c");
    assert_eq!(
        html_to_line("x<y and no closing", 100),
        "x<y and no closing"
    );
}

#[test]
fn scripts_styles_and_comments_are_not_text() {
    assert_eq!(
        html_to_line(
            "a<script>alert('x>y')</script>b<STYLE type=x>p{}</sTyLe>c<!-- d > e -->f",
            100
        ),
        "abcf"
    );
    assert_eq!(html_to_line("kept<script>alert(1)", 100), "kept");
    assert_eq!(html_to_line("kept<!-- never closed", 100), "kept");
}

#[test]
fn entities_are_decoded_once() {
    assert_eq!(
        html_to_line("caf&eacute; &amp; th&#xE9;", 100),
        "café & thé"
    );
    assert_eq!(html_to_line("AT&T &unknown; &", 100), "AT&T &unknown; &");
    assert_eq!(html_to_line("&lt;b&gt;bold&lt;/b&gt;", 100), "<b>bold</b>");
    assert_eq!(html_to_line("&amp;lt;", 100), "&lt;");
    assert_eq!(
        html_to_line("&#0;&#xD800;&#x110000;", 100),
        "&#0;&#xD800;&#x110000;",
        "a reference that names no character stays as written"
    );
}

#[test]
fn a_line_is_bounded_and_keeps_no_control_character() {
    assert_eq!(html_to_line("abcdef", 3), "abc");
    assert_eq!(html_to_line("a\u{7}b\n\n\tc  ", 100), "ab c");
    assert_eq!(html_to_line("ééééé", 2), "éé");
    assert_eq!(clip("éé", 3), "é");
}

#[test]
fn a_cue_loses_every_angle_bracket_pair() {
    assert_eq!(
        cue_to_text("<v Ana>Hello <00:00:01.000><c.yellow>there</c>&amp;co"),
        "Hello there&co"
    );
}

/// Measured on the shapes that would make a look-ahead quadratic: five
/// megabytes of each comes back well inside the bound, where a stripper that
/// rescans from every `<` would still be running after the test had failed.
#[test]
fn hostile_markup_is_read_in_linear_time() {
    let started = std::time::Instant::now();
    for shape in ["<a ", "<", "&", "&#", "<script></", "</scrip", "<!-", "<a>"] {
        let input = shape.repeat(5 * 1024 * 1024 / shape.len());
        let _ = html_to_text(&input);
        let _ = cue_to_text(&input);
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}
