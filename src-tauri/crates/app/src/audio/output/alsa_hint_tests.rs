use super::devices::{present_alsa_hints, AlsaHintRow};

fn row(id: &str, display: &str) -> AlsaHintRow {
    AlsaHintRow {
        id: id.to_string(),
        display: display.to_string(),
    }
}

fn ids(rows: &[AlsaHintRow]) -> Vec<&str> {
    rows.iter().map(|row| row.id.as_str()).collect()
}

#[test]
fn routing_families_are_not_devices() {
    // The reported list: one card, six lines, and three of them go
    // through the mixer — picking one defeats the exclusive output
    // the user has just turned on.
    let kept = present_alsa_hints(vec![
        row("hw:CARD=PCH,DEV=0", "Built-in Audio"),
        row("plughw:CARD=PCH,DEV=0", "Built-in Audio"),
        row("dmix:CARD=PCH,DEV=0", "Built-in Audio"),
        row("dsnoop:CARD=PCH,DEV=0", "Built-in Audio"),
        row("surround40:CARD=PCH,DEV=0", "Built-in Audio"),
        row("null", "Discard all samples"),
    ]);
    assert_eq!(ids(&kept), ["hw:CARD=PCH,DEV=0"]);
}

#[test]
fn a_wrapper_over_hardware_we_already_show_goes() {
    // `front:` and `sysdefault:` reach the same endpoint as the `hw:`
    // row beside them, and `hw:` is the spelling exclusive output can
    // use. The card token is what pairs them: the names share nothing.
    let kept = present_alsa_hints(vec![
        row("sysdefault:CARD=PCH", "Built-in Audio"),
        row("front:CARD=PCH,DEV=0", "Built-in Audio, Front speakers"),
        row(
            "hw:CARD=PCH,DEV=0",
            "Built-in Audio, Direct hardware device",
        ),
    ]);
    assert_eq!(ids(&kept), ["hw:CARD=PCH,DEV=0"]);
}

#[test]
fn a_wrapper_with_no_hardware_row_behind_it_stays() {
    // Some configurations list no bare `hw:` entry at all. Dropping
    // the only way to reach a card would be worse than showing an
    // alias.
    let kept = present_alsa_hints(vec![
        row("sysdefault:CARD=PCH", "Built-in Audio"),
        row("front:CARD=PCH,DEV=0", "Built-in Audio, Front speakers"),
    ]);
    assert_eq!(ids(&kept), ["sysdefault:CARD=PCH", "front:CARD=PCH,DEV=0"]);
}

#[test]
fn a_different_device_on_the_same_card_is_a_different_output() {
    // S/PDIF and HDMI live on the same card under their own device
    // numbers. Keying on the card alone would hide them behind the
    // analogue output.
    let kept = present_alsa_hints(vec![
        row("hw:CARD=PCH,DEV=0", "Analogue"),
        row("iec958:CARD=PCH,DEV=1", "S/PDIF"),
        row("hdmi:CARD=HDMI,DEV=0", "HDMI 1"),
        row("hdmi:CARD=HDMI,DEV=1", "HDMI 2"),
    ]);
    assert_eq!(
        ids(&kept),
        [
            "hw:CARD=PCH,DEV=0",
            "iec958:CARD=PCH,DEV=1",
            "hdmi:CARD=HDMI,DEV=0",
            "hdmi:CARD=HDMI,DEV=1"
        ]
    );
}

#[test]
fn the_choices_that_are_not_hardware_stay() {
    // `default`, `pulse` and `pipewire` are not devices, and they are
    // the right answer for most people most of the time — on a
    // PipeWire desktop, often the only one that works.
    let kept = present_alsa_hints(vec![
        row("default", "Default Audio Device"),
        row("pulse", "PulseAudio Sound Server"),
        row("pipewire", "PipeWire Sound Server"),
    ]);
    assert_eq!(ids(&kept), ["default", "pulse", "pipewire"]);
}

#[test]
fn two_devices_on_one_card_are_told_apart_by_their_names() {
    // The card token is no help here — both rows carry it — so the
    // suffix falls back to the id, which is unique by construction.
    // Before, the list read "USB Audio (PCH)" twice, which is the
    // duplicate this pass exists to remove.
    let kept = present_alsa_hints(vec![
        row("hw:CARD=PCH,DEV=0", "USB Audio"),
        row("hw:CARD=PCH,DEV=2", "USB Audio"),
    ]);
    assert_eq!(
        kept.iter().map(|r| r.display.as_str()).collect::<Vec<_>>(),
        [
            "USB Audio (hw:CARD=PCH,DEV=0)",
            "USB Audio (hw:CARD=PCH,DEV=2)"
        ]
    );
}

#[test]
fn two_identical_cards_do_not_read_as_one() {
    // Two of the same DAC describe themselves identically. The ids
    // differ, so both picks work — but the list showed one line
    // twice, which reads as a duplicate rather than as two devices.
    let kept = present_alsa_hints(vec![
        row("hw:CARD=D50s,DEV=0", "Topping D50s"),
        row("hw:CARD=D50s_1,DEV=0", "Topping D50s"),
    ]);
    assert_eq!(
        kept.iter().map(|r| r.display.as_str()).collect::<Vec<_>>(),
        ["Topping D50s (D50s)", "Topping D50s (D50s_1)"]
    );
}
