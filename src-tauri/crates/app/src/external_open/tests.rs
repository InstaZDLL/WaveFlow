use super::{host_value, is_openable_url};

const APPDIR: &str = "/tmp/.mount_WaveFlkLdcgI";

#[test]
fn entries_inside_the_image_are_dropped_and_the_host_ones_kept() {
    let path = "/tmp/.mount_WaveFlkLdcgI/usr/bin/:/usr/local/bin:/usr/bin";
    assert_eq!(
        host_value(path, APPDIR, false).as_deref(),
        Some("/usr/local/bin:/usr/bin")
    );
}

#[test]
fn a_variable_that_only_points_into_the_image_is_removed() {
    // The launcher writes some of them with a doubled slash.
    assert_eq!(
        host_value(
            "/tmp/.mount_WaveFlkLdcgI//usr/lib/gio/modules",
            APPDIR,
            false
        ),
        None
    );
    assert_eq!(
        host_value(
            "/tmp/.mount_WaveFlkLdcgI/usr/lib/:/tmp/.mount_WaveFlkLdcgI/usr/lib32/",
            APPDIR,
            false
        ),
        None
    );
}

#[test]
fn a_path_the_app_aimed_at_the_image_through_a_link_is_removed() {
    // The link lives in the cache directory, so nothing in the value
    // names the mount: only knowing the app set it gives it away.
    let link = "/home/nayeon/.cache/app.waveflow/gstreamer-plugins";
    assert_eq!(host_value(link, APPDIR, true), None);
    assert_eq!(host_value(link, APPDIR, false).as_deref(), Some(link));
}

#[test]
fn values_that_never_mention_the_image_are_left_alone() {
    assert_eq!(host_value("GNOME", APPDIR, false).as_deref(), Some("GNOME"));
    assert_eq!(host_value("", APPDIR, false).as_deref(), Some(""));
}

#[test]
fn only_web_and_mail_links_are_openable() {
    assert!(is_openable_url("https://www.last.fm/api/account/create"));
    assert!(is_openable_url("http://example.com"));
    assert!(is_openable_url("mailto:someone@example.com"));
    assert!(!is_openable_url("file:///etc/passwd"));
    assert!(!is_openable_url("/home/user"));
    assert!(!is_openable_url("javascript:alert(1)"));
}
