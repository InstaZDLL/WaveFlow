use super::*;

#[test]
fn colors_parse_only_a_plain_hex_triplet() {
    assert_eq!(
        FrameColors::parse("#1a2B3c"),
        Some(FrameColors {
            background: [0x1a, 0x2b, 0x3c],
            dark: true
        })
    );
    // Everything else is refused, not escaped: the value is pasted
    // into a stylesheet.
    for bad in [
        "1a2b3c",
        "#1a2b3",
        "#1a2b3c4d",
        "#gg0000",
        "red",
        "#000000; } window { background: red",
        "",
    ] {
        assert_eq!(FrameColors::parse(bad), None, "{bad:?}");
    }
}

/// Read from the colour, not the theme: the pages the skins paint.
#[test]
fn dark_or_light_follows_the_background() {
    for (page, dark) in [
        ("#0b0b0e", true),  // Fluid
        ("#000000", true),  // Pulse
        ("#14070c", true),  // Lounge
        ("#121212", true),  // the default dark ambient
        ("#f0ebd8", false), // Editorial's paper
        ("#eceef5", false), // Liquid, light
        ("#e0e7ff", false), // an indigo-100 ambient
        ("#ffffff", false),
    ] {
        assert_eq!(
            FrameColors::parse(page).map(|c| c.dark),
            Some(dark),
            "{page}"
        );
    }
}

/// The promise of the module header: the file pickers, the tray menu
/// and the other windows keep the desktop's look. Every selector has
/// to start with the main window's class, on a client-side frame.
#[cfg(target_os = "linux")]
#[test]
fn every_rule_is_scoped_to_the_main_window() {
    let prefix = format!("window.{WINDOW_CLASS}.csd");
    for dark in [false, true] {
        let css = stylesheet(FrameColors::fallback(dark));
        let mut selectors = 0;
        for block in css.split('}').filter(|b| b.contains('{')) {
            let head = block.split('{').next().unwrap();
            for selector in head.split(',').map(str::trim) {
                assert!(selector.starts_with(&prefix), "{selector:?}");
                selectors += 1;
            }
        }
        assert!(selectors > 5, "the sheet should not be empty");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_bar_takes_the_colours_it_was_given() {
    let css = stylesheet(FrameColors {
        background: [18, 18, 18],
        dark: true,
    });
    assert!(css.contains("background: rgb(18, 18, 18);"));
    assert!(css.contains("color: #ffffff;"));
    let css = stylesheet(FrameColors {
        background: [224, 231, 255],
        dark: false,
    });
    assert!(css.contains("background: rgb(224, 231, 255);"));
    assert!(css.contains("color: rgba(0, 0, 6, 0.8);"));
}
