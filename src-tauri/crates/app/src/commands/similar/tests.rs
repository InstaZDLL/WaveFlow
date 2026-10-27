use super::*;

#[test]
fn unenriched_fallback_filters_a_cached_deezer_placeholder() {
    // Regression (#406): an offline/cache payload written before the
    // fix still carries Deezer's empty-hash silhouette. The None-branch
    // fallback must drop it, not surface a grey placeholder.
    let placeholder = Some(
        "https://e-cdns-images.dzcdn.net/images/artist//500x500-000000-80-0-0.jpg".to_string(),
    );
    assert_eq!(unenriched_fallback_picture(placeholder), None);
}

#[test]
fn unenriched_fallback_filters_the_lastfm_star() {
    let star = Some(
        "https://lastfm.freetls.fastly.net/i/u/300x300/2a96cbd8b46e442fc41c2b86b821562f.png"
            .to_string(),
    );
    assert_eq!(unenriched_fallback_picture(star), None);
}

#[test]
fn unenriched_fallback_keeps_a_real_url() {
    let real = Some("https://e-cdns-images.dzcdn.net/images/artist/abc123/500x500.jpg".to_string());
    assert_eq!(unenriched_fallback_picture(real.clone()), real);
}
