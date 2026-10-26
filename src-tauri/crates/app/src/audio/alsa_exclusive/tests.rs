use super::hw_card_index;
use super::{is_system_route, resolve_hw_device};

#[test]
fn a_pinned_sound_server_route_opens_the_default_card() {
    for route in ["pipewire", "PipeWire", "pulse", "default"] {
        assert!(is_system_route(route), "{route}");
        assert_eq!(
            resolve_hw_device(&Some(route.to_string())).unwrap(),
            "hw:0,0",
            "{route}"
        );
    }
    assert!(!is_system_route("PCH"));
}

use super::{pack_samples, pcm_scale, AlsaSampleFormat};

#[test]
fn an_unopened_engine_asks_for_stereo_not_mono() {
    // `SharedPlayback` starts at zero channels and only fills in
    // when a backend opens. Read literally, the first launch with
    // exclusive already on would ask the card for one channel — and
    // a card that grants it would downmix every track from then on.
    assert_eq!(super::channel_candidates(0), vec![2]);
}

#[test]
fn a_known_layout_is_tried_before_stereo_and_stereo_closes_the_list() {
    assert_eq!(super::channel_candidates(6), vec![6, 2]);
    assert_eq!(super::channel_candidates(2), vec![2]);
    // A genuinely mono device keeps its own layout first.
    assert_eq!(super::channel_candidates(1), vec![1, 2]);
}

/// The trap this whole variant exists to document. ALSA puts the 24
/// bits in the LOW three bytes; WASAPI's 24-in-32 puts them in the
/// HIGH three. Copying that backend's `<< 8` over here would send
/// every sample out 256x too large.
#[test]
fn twenty_four_in_thirty_two_is_right_aligned_not_left() {
    let mut bytes = [0u8; 4];
    pack_samples(
        AlsaSampleFormat::S24In32,
        &[1.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    // 8_388_607 = 0x7F_FF_FF, little-endian, top byte untouched.
    assert_eq!(bytes, [0xFF, 0xFF, 0x7F, 0x00]);
    // The left-aligned layout would have been [0x00, 0xFF, 0xFF, 0x7F].
    assert_ne!(bytes, [0x00, 0xFF, 0xFF, 0x7F]);
}

#[test]
fn a_negative_twenty_four_bit_sample_keeps_its_sign_extension() {
    let mut bytes = [0u8; 4];
    pack_samples(
        AlsaSampleFormat::S24In32,
        &[-1.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    // -8_388_608 as i32 = 0xFF_80_00_00. The driver reads the low
    // three bytes; the 0xFF above them is the sign extension a plain
    // i32 store produces and is ignored.
    assert_eq!(bytes, [0x00, 0x00, 0x80, 0xFF]);
}

#[test]
fn the_packed_form_spends_three_bytes_and_no_container() {
    let mut bytes = [0u8; 3];
    pack_samples(
        AlsaSampleFormat::S24Packed,
        &[1.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    assert_eq!(bytes, [0xFF, 0xFF, 0x7F]);
}

#[test]
fn sixteen_bit_full_scale_lands_on_the_endpoints() {
    let mut bytes = [0u8; 4];
    pack_samples(
        AlsaSampleFormat::S16,
        &[1.0, -1.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 32_767);
    assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -32_768);
}

#[test]
fn a_sample_past_full_scale_saturates_instead_of_wrapping() {
    // Anything above 0 dBFS saturates, so it comes out at the
    // endpoint. Wrapping here would turn a loud passage
    // into a burst of full-scale noise of the opposite sign.
    let mut bytes = [0u8; 4];
    pack_samples(
        AlsaSampleFormat::S32,
        &[9.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    assert_eq!(i32::from_le_bytes(bytes), 2_147_483_647);
    pack_samples(
        AlsaSampleFormat::S32,
        &[-9.0],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    assert_eq!(i32::from_le_bytes(bytes), -2_147_483_648);
}

#[test]
fn float_output_is_a_byte_copy() {
    let mut bytes = [0u8; 4];
    pack_samples(
        AlsaSampleFormat::F32,
        &[0.25],
        &mut bytes,
        &mut pcm_scale::Dither16::new(),
    );
    assert_eq!(f32::from_le_bytes(bytes), 0.25);
}

#[test]
fn a_numeric_hw_name_yields_its_index() {
    assert_eq!(hw_card_index("hw:0,0"), Some(0));
    assert_eq!(hw_card_index("hw:3"), Some(3));
}

#[test]
fn a_name_we_never_resolved_to_hw_has_no_index() {
    // `resolve_hw_device` only ever hands us `hw:` names, but the
    // reservation must not invent a card index from anything else.
    assert_eq!(hw_card_index("default"), None);
    assert_eq!(hw_card_index("plughw:1,0"), None);
}

#[test]
fn a_card_selector_is_looked_up_and_not_parsed() {
    // `hw:CARD=…` carries a name, not an index — parsing it as a
    // number would reserve card 0 and hand the wrong device over.
    assert_eq!(hw_card_index("hw:CARD=NoSuchCardHere,DEV=0"), None);
}
