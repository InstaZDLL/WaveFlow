use super::*;
use crate::analysis::key::{MAJOR_NAMES, MINOR_NAMES};

fn key(raw: &str) -> Key {
    parse(raw).unwrap_or_else(|| panic!("{raw} did not parse"))
}

#[test]
fn every_spelling_reads_the_same_key() {
    let a_minor = Key {
        tonic: 9,
        minor: true,
    };
    for raw in ["Am", "A minor", "a min", "8A", "08a", "1m", " A-minor "] {
        assert_eq!(key(raw), a_minor, "{raw}");
    }
    let e_flat = Key {
        tonic: 3,
        minor: false,
    };
    for raw in ["Eb", "E♭ major", "D#", "5B", "10d"] {
        assert_eq!(key(raw), e_flat, "{raw}");
    }
    assert_eq!(
        key("Bbm"),
        Key {
            tonic: 10,
            minor: true
        }
    );
    assert_eq!(
        key("B"),
        Key {
            tonic: 11,
            minor: false
        }
    );
}

#[test]
fn what_is_not_a_key_is_none() {
    for raw in ["", "  ", "H", "13A", "0B", "8C", "C lydian", "Am7"] {
        assert_eq!(parse(raw), None, "{raw}");
    }
}

/// Every name the analysis stores reads back as itself, and lands where
/// the wheel puts it.
#[test]
fn the_stored_names_land_on_the_wheel() {
    for (tonic, name) in MAJOR_NAMES.iter().enumerate() {
        let k = key(name);
        assert_eq!((k.tonic as usize, k.minor), (tonic, false), "{name}");
    }
    for (tonic, name) in MINOR_NAMES.iter().enumerate() {
        let k = key(name);
        assert_eq!((k.tonic as usize, k.minor), (tonic, true), "{name}");
    }
    assert_eq!(key("C").camelot(), (8, false));
    assert_eq!(key("Am").camelot(), (8, true));
    assert_eq!(key("G").camelot(), (9, false));
    assert_eq!(key("F").camelot(), (7, false));
    assert_eq!(key("E").camelot(), (12, false));
    assert_eq!(key("B").camelot(), (1, false));
    for n in 1..=12u8 {
        for ring in ["A", "B"] {
            let raw = format!("{n}{ring}");
            assert_eq!(key(&raw).camelot(), (n, ring == "A"), "{raw}");
        }
    }
}

#[test]
fn neighbours_on_the_wheel_are_compatible() {
    let c = key("C");
    assert!(compatible(c, key("C")));
    assert!(compatible(c, key("G")), "a fifth up");
    assert!(compatible(c, key("F")), "a fifth down");
    assert!(compatible(c, key("Am")), "relative minor");
    assert!(compatible(key("12B"), key("1B")), "the wheel wraps");
    assert!(compatible(key("1A"), key("12A")));

    assert!(!compatible(c, key("D")), "two steps");
    assert!(!compatible(c, key("Em")), "a step and a ring");
    assert!(!compatible(c, key("Cm")), "parallel minor");
    assert!(!compatible(c, key("F#")), "the far side");
}
