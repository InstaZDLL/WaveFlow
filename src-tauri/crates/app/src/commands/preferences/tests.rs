use super::*;

const EVERYTHING: ChromeSupport = ChromeSupport {
    app: true,
    adwaita: true,
    app_needs_xwayland: false,
};
/// macOS, or Linux under X11: no frame GTK draws.
const NO_ADWAITA: ChromeSupport = ChromeSupport {
    app: true,
    adwaita: false,
    app_needs_xwayland: false,
};
/// KWin on Wayland: the compositor draws the frame and keeps it, so
/// WaveFlow's own bar would be a second one (#1089).
const COMPOSITOR_FRAME: ChromeSupport = ChromeSupport {
    app: false,
    adwaita: false,
    app_needs_xwayland: true,
};
/// Windows.
const NOTHING: ChromeSupport = ChromeSupport {
    app: false,
    adwaita: false,
    app_needs_xwayland: false,
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

/// The real `app.db` schema: `value_type` and `updated_at` are NOT NULL and
/// checked, which a hand-made table would let a wrong write get past.
async fn migrated_app_db() -> sqlx::SqlitePool {
    // One connection: each one to `:memory:` is a database of its own.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("../../migrations/app")
        .run(&pool)
        .await
        .unwrap();
    pool
}

/// No row is the answer the frontend tells an update from a new install
/// with, and a later version replaces the row rather than adding one.
#[tokio::test]
async fn the_whats_new_marker_starts_absent_and_follows_the_version() {
    let pool = migrated_app_db().await;
    assert_eq!(read_whats_new_marker(&pool).await.unwrap(), None);

    write_whats_new_marker(&pool, "2.0.0-beta.1").await.unwrap();
    assert_eq!(
        read_whats_new_marker(&pool).await.unwrap().as_deref(),
        Some("2.0.0-beta.1")
    );

    write_whats_new_marker(&pool, "2.0.0").await.unwrap();
    assert_eq!(
        read_whats_new_marker(&pool).await.unwrap().as_deref(),
        Some("2.0.0")
    );
}

/// #1089: on a compositor that keeps drawing its frame, a stored `app`
/// resolves to the desktop's frame -- not to a title bar drawn under the
/// compositor's -- and the choice is kept, so it comes back by itself
/// once XWayland is on.
#[test]
fn a_compositor_frame_refuses_app_without_losing_it() {
    let stored = WindowChrome::App;
    let resolved = effective_chrome(stored, false, COMPOSITOR_FRAME);
    assert_eq!(resolved, WindowChrome::System);
    assert_eq!(resolved.draw(), "none");
    assert_eq!(effective_chrome(stored, false, NO_ADWAITA), stored);
}

/// The card has something to say there (why `app` is missing) even when
/// the only choice left is the desktop's frame.
#[test]
fn a_compositor_frame_explains_the_missing_option() {
    let state = WindowChromeState::new(WindowChrome::System, COMPOSITOR_FRAME);
    assert!(state.supported);
    assert!(state.app_needs_xwayland);
    assert!(!state.options.contains(&"app"));
    assert!(!state.options.contains(&"adwaita"));
    let plain = WindowChromeState::new(WindowChrome::System, NOTHING);
    assert!(!plain.supported);
    assert!(!plain.app_needs_xwayland);
}
