use super::is_relative;

#[test]
fn only_a_relative_path_is_accepted() {
    assert!(is_relative("/api/v2/canvas-stream/abc"));

    // The ones that matter: each would move the webview off the host the
    // user authenticated against, and a `<video>` element would load them
    // without a murmur.
    assert!(!is_relative("//evil.example/canvas"));
    assert!(!is_relative("https://evil.example/canvas"));
    assert!(!is_relative("http://evil.example/canvas"));
    assert!(!is_relative("javascript:alert(1)"));
    assert!(!is_relative("api/v2/canvas-stream/abc"));
    assert!(!is_relative(""));
}
