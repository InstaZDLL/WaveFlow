use super::fill_pcm_period;
use crate::audio::state::SharedPlayback;
use rtrb::RingBuffer;

#[test]
fn a_dry_ring_yields_silence_that_is_not_credited() {
    // The counter drives the progress bar and play crediting: an
    // underrun must leave the track where it was, not advance it.
    let shared = SharedPlayback::new();
    let (_producer, mut consumer) = RingBuffer::<f32>::new(8);
    let mut samples = [1.0_f32; 4];
    let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
    assert_eq!(written, 0);
    assert_eq!(samples, [0.0; 4]);
}

#[test]
fn volume_is_applied_here_because_a_raw_device_has_no_mixer() {
    let shared = SharedPlayback::new();
    shared.set_volume(0.5);
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
    for _ in 0..4 {
        producer.push(1.0).expect("ring has room");
    }
    let mut samples = [0.0_f32; 4];
    let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
    assert_eq!(written, 4);
    assert_eq!(samples, [0.5; 4]);
}

#[test]
fn the_mono_switch_is_not_the_output_s_job() {
    // Folded on the decoder thread, before the balance; folding again
    // here would average a balanced frame back over both speakers.
    let shared = SharedPlayback::new();
    shared
        .mono_enabled
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
    producer.push(0.0).expect("ring has room");
    producer.push(1.0).expect("ring has room");
    let mut samples = [0.5_f32; 2];
    let written = fill_pcm_period(&shared, &mut consumer, &mut samples, 2);
    assert_eq!(written, 2);
    assert_eq!(samples, [0.0, 1.0]);
}
