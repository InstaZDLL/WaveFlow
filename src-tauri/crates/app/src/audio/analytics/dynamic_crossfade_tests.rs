use super::dynamic_crossfade_ms;

const BASE: u32 = 8000;

#[test]
fn close_tempos_keep_the_whole_window() {
    assert_eq!(
        dynamic_crossfade_ms(
            BASE,
            Some((Some(120.0), Some(0.8))),
            Some((Some(124.0), Some(0.7)))
        ),
        BASE
    );
}

#[test]
fn a_wide_gap_shortens_the_fade() {
    assert_eq!(
        dynamic_crossfade_ms(
            BASE,
            Some((Some(90.0), Some(0.8))),
            Some((Some(140.0), Some(0.8)))
        ),
        2400
    );
}

/// An untrusted or unknown tempo on either side keeps the static
/// window rather than guessing.
#[test]
fn an_untrusted_tempo_keeps_the_static_window() {
    let sure = Some((Some(120.0), Some(0.8)));
    assert_eq!(
        dynamic_crossfade_ms(BASE, sure, Some((Some(60.0), Some(0.1)))),
        0
    );
    assert_eq!(
        dynamic_crossfade_ms(BASE, sure, Some((Some(60.0), None))),
        0
    );
    assert_eq!(dynamic_crossfade_ms(BASE, sure, Some((None, None))), 0);
    assert_eq!(dynamic_crossfade_ms(BASE, sure, None), 0);
}

#[test]
fn the_floor_never_lengthens_a_short_base() {
    assert_eq!(
        dynamic_crossfade_ms(
            1000,
            Some((Some(90.0), Some(0.8))),
            Some((Some(160.0), Some(0.8)))
        ),
        1000
    );
}
