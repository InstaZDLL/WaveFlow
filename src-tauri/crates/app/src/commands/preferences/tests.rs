use super::*;

const EVERYTHING: ChromeSupport = ChromeSupport {
    app: true,
    adwaita: true,
};
/// macOS, or Linux under X11: no frame GTK draws.
const NO_ADWAITA: ChromeSupport = ChromeSupport {
    app: true,
    adwaita: false,
};
/// Windows.
const NOTHING: ChromeSupport = ChromeSupport {
    app: false,
    adwaita: false,
};

/// A stored choice this session could not put on the window must not
/// be reported as the one in use: the interface draws what it is told,
/// so `app` after a failed apply is a title bar over the system one
/// the window still has.
#[test]
fn a_chrome_that_could_not_be_applied_reports_as_system() {
    for stored in [
        WindowChrome::App,
        WindowChrome::Adwaita,
        WindowChrome::System,
    ] {
        assert_eq!(
            effective_chrome(stored, true, EVERYTHING),
            WindowChrome::System
        );
    }
}

/// With nothing to correct, the platform rule is the only one left --
/// and it refuses what this platform or launch does not offer.
#[test]
fn an_applied_chrome_is_reported_as_stored_where_offered() {
    for stored in [
        WindowChrome::App,
        WindowChrome::Adwaita,
        WindowChrome::System,
    ] {
        assert_eq!(effective_chrome(stored, false, EVERYTHING), stored);
        assert_eq!(
            effective_chrome(stored, false, NOTHING),
            WindowChrome::System
        );
    }
    // A row written on a Wayland session, read under X11: the
    // desktop's frame, not the frame WaveFlow draws.
    assert_eq!(
        effective_chrome(WindowChrome::Adwaita, false, NO_ADWAITA),
        WindowChrome::System
    );
    assert_eq!(
        effective_chrome(WindowChrome::App, false, NO_ADWAITA),
        WindowChrome::App
    );
}

/// `adwaita` keeps the desktop's frame: the interface must not draw a
/// title bar under it.
#[test]
fn a_restyled_frame_draws_nothing_more() {
    assert_eq!(WindowChrome::Adwaita.draw(), "none");
    assert_eq!(WindowChrome::System.draw(), "none");
}

#[test]
fn the_card_offers_what_is_supported_and_hides_when_alone() {
    assert_eq!(EVERYTHING.options(), ["system", "adwaita", "app"]);
    assert_eq!(NO_ADWAITA.options(), ["system", "app"]);
    assert_eq!(NOTHING.options(), ["system"]);
    assert!(!WindowChromeState::new(WindowChrome::System, NOTHING).supported);
    assert!(WindowChromeState::new(WindowChrome::System, NO_ADWAITA).supported);
}

#[test]
fn every_option_round_trips_through_its_stored_value() {
    for chrome in [
        WindowChrome::System,
        WindowChrome::Adwaita,
        WindowChrome::App,
    ] {
        assert_eq!(WindowChrome::parse(chrome.as_str()), Some(chrome));
    }
    assert_eq!(WindowChrome::parse("gnome"), None);
}
