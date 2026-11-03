use super::Speaker::*;
use super::*;

const SQRT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

fn approx(a: f32, b: f32) {
    assert!((a - b).abs() < 1e-5, "expected {b}, got {a}");
}

/// One frame with a value of its own on each channel, mixed.
fn mix(src: &ChannelLayout, dst: &ChannelLayout, frame: &[f32]) -> Vec<f32> {
    let mut out = Vec::new();
    ChannelMixer::new(src, dst).convert(frame, &mut out);
    assert_eq!(out.len(), dst.channels());
    out
}

fn counts(src: usize, dst: usize, frame: &[f32]) -> Vec<f32> {
    mix(
        &ChannelLayout::from_count(src),
        &ChannelLayout::from_count(dst),
        frame,
    )
}

/// How much of each source channel reaches each output channel, one
/// channel lit at a time. Rows are output channels.
fn gains(src: &ChannelLayout, dst: &ChannelLayout) -> Vec<Vec<f32>> {
    let mixer = ChannelMixer::new(src, dst);
    let mut rows = vec![vec![0.0; src.channels()]; dst.channels()];
    for ch in 0..src.channels() {
        let mut frame = vec![0.0; src.channels()];
        frame[ch] = 1.0;
        let mut out = Vec::new();
        mixer.convert(&frame, &mut out);
        for (row, value) in rows.iter_mut().zip(out) {
            row[ch] = value;
        }
    }
    rows
}

const SURROUND_5_1_SIDE: [Speaker; 6] =
    [FrontLeft, FrontRight, FrontCenter, Lfe, SideLeft, SideRight];

#[test]
fn equal_layouts_are_copied_bit_for_bit() {
    for count in [1, 2, 6, 8, 12] {
        let frame: Vec<f32> = (0..count).map(|i| 0.1 + i as f32 * 0.037).collect();
        let out = counts(count, count, &frame);
        assert_eq!(
            out.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
            frame.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn mono_keeps_its_level_on_the_front_pair_of_any_output() {
    assert_eq!(counts(1, 2, &[0.5]), [0.5, 0.5]);
    assert_eq!(
        counts(1, 8, &[-0.25]),
        [-0.25, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
    );
}

#[test]
fn stereo_on_a_wide_output_stays_on_the_front_pair_untouched() {
    for dst in [3, 4, 5, 6, 7, 8] {
        let out = counts(2, dst, &[0.3, -0.7]);
        assert_eq!(out[0].to_bits(), 0.3_f32.to_bits());
        assert_eq!(out[1].to_bits(), (-0.7_f32).to_bits());
        assert!(out[2..].iter().all(|s| *s == 0.0), "{dst}: {out:?}");
    }
}

#[test]
fn stereo_on_mono_is_the_average() {
    let out = counts(2, 1, &[1.0, 0.0]);
    approx(out[0], 0.5);
    let out = counts(2, 1, &[0.5, -0.5]);
    approx(out[0], 0.0);
}

/// BS.775's Lo/Ro, scaled by the one factor that keeps a full-scale frame
/// in range: 1 + 0.707 + 0.707.
#[test]
fn five_one_on_stereo_is_bs775_without_the_lfe() {
    let scale = 1.0 / (1.0 + 2.0 * SQRT_HALF);
    // FL FR FC LFE BL BR
    let out = counts(6, 2, &[1.0, 0.0, 0.0, 9.9, 0.0, 0.0]);
    approx(out[0], scale);
    approx(out[1], 0.0);
    let out = counts(6, 2, &[0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
    approx(out[0], SQRT_HALF * scale);
    approx(out[1], SQRT_HALF * scale);
    let out = counts(6, 2, &[0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    approx(out[0], SQRT_HALF * scale);
    approx(out[1], 0.0);
    let out = counts(6, 2, &[0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
    approx(out[0], 0.0);
    approx(out[1], SQRT_HALF * scale);
}

#[test]
fn seven_one_on_stereo_folds_every_surround_to_its_side() {
    let rows = gains(&ChannelLayout::from_count(8), &ChannelLayout::from_count(2));
    // FL FR FC LFE BL BR SL SR
    let scale = 1.0 / (1.0 + 3.0 * SQRT_HALF);
    let left = [1.0, 0.0, SQRT_HALF, 0.0, SQRT_HALF, 0.0, SQRT_HALF, 0.0];
    let right = [0.0, 1.0, SQRT_HALF, 0.0, 0.0, SQRT_HALF, 0.0, SQRT_HALF];
    for ch in 0..8 {
        approx(rows[0][ch], left[ch] * scale);
        approx(rows[1][ch], right[ch] * scale);
    }
}

/// The reported defect: the fallback kept the first six channels and lost
/// the sides.
#[test]
fn seven_one_on_five_one_keeps_the_sides_in_the_surrounds() {
    let rows = gains(&ChannelLayout::from_count(8), &ChannelLayout::from_count(6));
    // 0.707 + 0.707 into each surround is the heaviest sum.
    let scale = 1.0 / (2.0 * SQRT_HALF);
    let expected = [
        // FL   FR   FC   LFE  BL        BR        SL        SR
        [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 0.0, 0.0, SQRT_HALF, 0.0, SQRT_HALF, 0.0],
        [0.0, 0.0, 0.0, 0.0, 0.0, SQRT_HALF, 0.0, SQRT_HALF],
    ];
    for (row, want) in rows.iter().zip(expected) {
        for (got, want) in row.iter().zip(want) {
            approx(*got, want * scale);
        }
    }
}

/// The other reported defect: the fallback sent the centre to a rear
/// speaker and the LFE to the other.
#[test]
fn five_one_on_quad_sends_the_centre_to_the_front_and_drops_the_lfe() {
    let rows = gains(&ChannelLayout::from_count(6), &ChannelLayout::from_count(4));
    let scale = 1.0 / (1.0 + SQRT_HALF);
    // Output FL FR BL BR.
    approx(rows[0][0], scale);
    approx(rows[0][2], SQRT_HALF * scale);
    approx(rows[1][1], scale);
    approx(rows[1][2], SQRT_HALF * scale);
    approx(rows[2][4], scale);
    approx(rows[3][5], scale);
    // The centre reaches no rear speaker, and the LFE nothing at all.
    assert_eq!(rows[2][2], 0.0);
    assert_eq!(rows[3][2], 0.0);
    assert!(rows.iter().all(|row| row[3] == 0.0));
}

#[test]
fn quad_on_five_one_goes_to_the_surrounds_not_the_centre_and_lfe() {
    // FL FR BL BR → FL FR FC LFE BL BR
    assert_eq!(
        counts(4, 6, &[0.1, 0.2, 0.3, 0.4]),
        [0.1, 0.2, 0.0, 0.0, 0.3, 0.4]
    );
}

#[test]
fn five_zero_on_five_one_leaves_the_lfe_silent() {
    // FL FR FC BL BR → FL FR FC LFE BL BR
    assert_eq!(
        counts(5, 6, &[0.1, 0.2, 0.3, 0.4, 0.5]),
        [0.1, 0.2, 0.3, 0.0, 0.4, 0.5]
    );
}

#[test]
fn five_one_on_seven_one_keeps_each_pair_on_its_own_speakers() {
    // Back in the file, back on the output.
    assert_eq!(
        counts(6, 8, &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]),
        [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.0, 0.0]
    );
    // Side in the file, side on the output.
    assert_eq!(
        mix(
            &ChannelLayout::new(SURROUND_5_1_SIDE.to_vec()),
            &ChannelLayout::from_count(8),
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
        ),
        [0.1, 0.2, 0.3, 0.4, 0.0, 0.0, 0.5, 0.6]
    );
}

/// The two 5.1 are the same speakers: side on back, either way round, is a
/// straight copy and takes no level off.
#[test]
fn side_and_back_five_one_map_onto_each_other_at_unity() {
    let side = ChannelLayout::new(SURROUND_5_1_SIDE.to_vec());
    let back = ChannelLayout::from_count(6);
    let frame = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
    assert_eq!(mix(&side, &back, &frame), frame);
    assert_eq!(mix(&back, &side, &frame), frame);
}

/// Seven channels with a rear centre, as FLAC and Vorbis lay them out.
#[test]
fn six_one_on_seven_one_spreads_the_rear_centre_over_the_back_pair() {
    let six_one = ChannelLayout::from_wave_mask(0b0111_0000_1111, 7);
    assert_eq!(
        six_one.speakers(),
        [
            FrontLeft,
            FrontRight,
            FrontCenter,
            Lfe,
            BackCenter,
            SideLeft,
            SideRight
        ]
    );
    let out = mix(
        &six_one,
        &ChannelLayout::from_count(8),
        &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7],
    );
    approx(out[4], 0.5 * SQRT_HALF);
    approx(out[5], 0.5 * SQRT_HALF);
    approx(out[6], 0.6);
    approx(out[7], 0.7);
}

#[test]
fn six_one_on_stereo_folds_the_rear_centre_into_both() {
    // FL FR FC LFE BL BR BC
    let rows = gains(&ChannelLayout::from_count(7), &ChannelLayout::from_count(2));
    let scale = 1.0 / (1.0 + 3.0 * SQRT_HALF);
    approx(rows[0][6], SQRT_HALF * scale);
    approx(rows[1][6], SQRT_HALF * scale);
    assert_eq!(rows[0][3], 0.0);
}

/// BS.775's mono: 0.707 L + 0.707 R + C + 0.5 Ls + 0.5 Rs. The old fallback
/// kept the front left alone.
#[test]
fn surround_on_mono_uses_every_channel_but_the_lfe() {
    let rows = gains(&ChannelLayout::from_count(6), &ChannelLayout::from_count(1));
    let scale = 1.0 / (2.0 * SQRT_HALF + 1.0 + 1.0);
    let want = [SQRT_HALF, SQRT_HALF, 1.0, 0.0, 0.5, 0.5];
    for (got, want) in rows[0].iter().zip(want) {
        approx(*got, want * scale);
    }
    let rows = gains(&ChannelLayout::from_count(8), &ChannelLayout::from_count(1));
    assert!(rows[0][6] > 0.0 && rows[0][7] > 0.0, "{rows:?}");
}

#[test]
fn three_channels_fold_the_centre_into_the_front() {
    let scale = 1.0 / (1.0 + SQRT_HALF);
    let out = counts(3, 2, &[1.0, 2.0, 0.4]);
    approx(out[0], (1.0 + SQRT_HALF * 0.4) * scale);
    approx(out[1], (2.0 + SQRT_HALF * 0.4) * scale);
}

#[test]
fn quad_on_stereo_maps_the_surrounds_by_side() {
    let scale = 1.0 / (1.0 + SQRT_HALF);
    let out = counts(4, 2, &[1.0, 2.0, 0.6, 0.8]);
    approx(out[0], (1.0 + SQRT_HALF * 0.6) * scale);
    approx(out[1], (2.0 + SQRT_HALF * 0.8) * scale);
}

#[test]
fn beyond_seven_one_no_channel_is_dropped() {
    let rows = gains(
        &ChannelLayout::from_count(10),
        &ChannelLayout::from_count(2),
    );
    for ch in [8, 9] {
        assert!(rows[0][ch] + rows[1][ch] > 0.0, "{ch}: {rows:?}");
    }
    assert!(rows[0][8] > 0.0 && rows[1][9] > 0.0, "{rows:?}");
}

/// A driver can accept a mono output as front left (`0x1`), and the fold has
/// to land on it all the same.
#[test]
fn a_single_channel_output_is_mono_whatever_it_is_called() {
    for mono in [
        ChannelLayout::from_wave_mask(0x1, 1),
        ChannelLayout::from_wave_mask(0x4, 1),
        ChannelLayout::new(vec![FrontLeft]),
    ] {
        assert_eq!(mono, ChannelLayout::from_count(1));
        let out = mix(&ChannelLayout::from_count(2), &mono, &[0.5, 0.5]);
        approx(out[0], 0.5);
        let out = mix(
            &ChannelLayout::from_count(6),
            &mono,
            &[0.0, 0.0, 1.0, 0.0, 0.0, 0.0],
        );
        assert!(out[0] > 0.0, "{out:?}");
    }
}

/// Channels past 7.1 have no name to find each other by, so each keeps its
/// place rather than all landing on the first one.
#[test]
fn nameless_channels_keep_their_place_on_a_wider_output() {
    let frame: Vec<f32> = (0..10).map(|i| i as f32 * 0.01 + 0.01).collect();
    let out = counts(10, 12, &frame);
    assert_eq!(out[..10], frame[..]);
    assert_eq!(out[10..], [0.0, 0.0]);
}

/// Every pair of common layouts: a full-scale frame stays in range after
/// the mix, whatever its signs. This is what makes the final clamp a
/// backstop rather than the limiter of every downmix.
#[test]
fn no_mix_of_common_layouts_can_clip() {
    let layouts = [1, 2, 3, 4, 5, 6, 7, 8];
    for src in layouts {
        for dst in layouts {
            let rows = gains(
                &ChannelLayout::from_count(src),
                &ChannelLayout::from_count(dst),
            );
            for row in rows {
                let worst: f32 = row.iter().map(|g| g.abs()).sum();
                assert!(worst <= 1.0 + 1e-5, "{src} → {dst}: {worst}");
            }
        }
    }
}

/// Every channel but the LFE reaches some speaker, whatever the output.
#[test]
fn no_mix_of_common_layouts_loses_a_full_range_channel() {
    for src in 2..=8 {
        let source = ChannelLayout::from_count(src);
        for dst in 1..=8 {
            let rows = gains(&source, &ChannelLayout::from_count(dst));
            for (ch, speaker) in source.speakers().iter().enumerate() {
                let total: f32 = rows.iter().map(|row| row[ch]).sum();
                if *speaker == Lfe {
                    continue;
                }
                assert!(total > 0.0, "{src} → {dst}: {speaker:?} is lost");
            }
        }
    }
}

/// The ALSA order is the same mix on other indices: a 5.1 frame reaches the
/// speakers of its names, not the ones at the same numbers.
#[test]
fn an_output_in_alsa_order_gets_each_channel_on_its_speaker() {
    assert_eq!(
        mix(
            &ChannelLayout::from_count(6),
            &ChannelLayout::alsa_default(6),
            // FL FR FC LFE BL BR
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
        ),
        // FL FR BL BR FC LFE
        [0.1, 0.2, 0.5, 0.6, 0.3, 0.4]
    );
    assert_eq!(
        mix(
            &ChannelLayout::from_count(8),
            &ChannelLayout::alsa_default(8),
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8]
        ),
        [0.1, 0.2, 0.5, 0.6, 0.3, 0.4, 0.7, 0.8]
    );
}

#[test]
fn a_mixer_appends_several_frames_to_what_is_there() {
    let mixer = ChannelMixer::new(&ChannelLayout::from_count(4), &ChannelLayout::from_count(6));
    let mut out = vec![9.0];
    mixer.convert(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8], &mut out);
    assert_eq!(
        out,
        [9.0, 0.1, 0.2, 0.0, 0.0, 0.3, 0.4, 0.5, 0.6, 0.0, 0.0, 0.7, 0.8]
    );
}

#[test]
fn a_wave_mask_names_the_channels_lowest_bit_first() {
    assert_eq!(
        ChannelLayout::from_wave_mask(0x60F, 6).speakers(),
        SURROUND_5_1_SIDE
    );
    assert_eq!(
        ChannelLayout::from_wave_mask(0x63F, 8),
        ChannelLayout::from_count(8)
    );
    // 7.1 wide: the last pair is in front, not at the side.
    assert_eq!(
        ChannelLayout::from_wave_mask(0xFF, 8).speakers()[6..],
        [FrontLeftOfCenter, FrontRightOfCenter]
    );
    // A mask that does not count these channels says nothing about them.
    assert_eq!(
        ChannelLayout::from_wave_mask(0x3, 6),
        ChannelLayout::from_count(6)
    );
    assert_eq!(
        ChannelLayout::from_wave_mask(0, 8),
        ChannelLayout::from_count(8)
    );
}

#[test]
fn a_layout_states_its_wave_mask_when_it_has_one() {
    assert_eq!(ChannelLayout::from_count(2).wave_mask(), Some(0x3));
    assert_eq!(ChannelLayout::from_count(4).wave_mask(), Some(0x33));
    assert_eq!(ChannelLayout::from_count(6).wave_mask(), Some(0x3F));
    assert_eq!(ChannelLayout::from_count(8).wave_mask(), Some(0x63F));
    // Another order, or a channel with no bit: no mask can say that.
    assert_eq!(ChannelLayout::alsa_default(6).wave_mask(), None);
    assert_eq!(ChannelLayout::from_count(10).wave_mask(), None);
}

#[test]
fn what_a_decoder_reports_decides_the_layout() {
    use symphonia::core::audio::layouts::*;
    use symphonia::core::audio::{Channels, Position};
    assert_eq!(
        ChannelLayout::from_symphonia(&CHANNEL_LAYOUT_5P1_SIDE).speakers(),
        SURROUND_5_1_SIDE
    );
    assert_eq!(
        ChannelLayout::from_symphonia(&CHANNEL_LAYOUT_5P1),
        ChannelLayout::from_count(6)
    );
    // Discrete channels have no position: the count speaks for them.
    assert_eq!(
        ChannelLayout::from_symphonia(&Channels::Discrete(6)),
        ChannelLayout::from_count(6)
    );
    // One channel is mono, whichever position the codec gave it.
    assert_eq!(
        ChannelLayout::from_symphonia(&Channels::Positioned(Position::FRONT_LEFT)),
        ChannelLayout::from_count(1)
    );
}

#[test]
fn pairs_reach_every_surround() {
    assert_eq!(ChannelLayout::from_count(1).pairs(), []);
    assert_eq!(ChannelLayout::from_count(2).pairs(), [(0, 1)]);
    assert_eq!(ChannelLayout::from_count(6).pairs(), [(0, 1), (4, 5)]);
    assert_eq!(
        ChannelLayout::from_count(8).pairs(),
        [(0, 1), (4, 5), (6, 7)]
    );
    assert_eq!(
        ChannelLayout::alsa_default(8).pairs(),
        [(0, 1), (2, 3), (6, 7)]
    );
}

#[test]
fn a_packed_layout_comes_back_the_same() {
    for layout in [
        ChannelLayout::from_count(1),
        ChannelLayout::from_count(2),
        ChannelLayout::from_count(8),
        ChannelLayout::alsa_default(6),
        ChannelLayout::new(SURROUND_5_1_SIDE.to_vec()),
        ChannelLayout::from_count(12),
    ] {
        let count = layout.channels();
        assert_eq!(ChannelLayout::unpack(layout.pack(), count), layout);
    }
    // Nothing published yet, or published for another count: the count.
    assert_eq!(ChannelLayout::unpack(0, 6), ChannelLayout::from_count(6));
    assert_eq!(
        ChannelLayout::unpack(ChannelLayout::alsa_default(6).pack(), 8),
        ChannelLayout::from_count(8)
    );
    // Too wide for the nibbles: zero, which is the count again.
    assert_eq!(ChannelLayout::from_count(17).pack(), 0);
}

#[test]
fn a_stereo_track_folds_to_mono_from_the_front_pair_alone() {
    // The old average over eight channels took 12 dB off it and fed the LFE.
    let gains = ChannelLayout::from_count(8).mono_gains(2);
    assert_eq!(gains, [0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    let gains = ChannelLayout::alsa_default(6).mono_gains(1);
    assert_eq!(gains, [0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn a_surround_track_folds_to_mono_by_bs775_and_stays_in_range() {
    let gains = ChannelLayout::from_count(6).mono_gains(6);
    let scale = 1.0 / (2.0 * SQRT_HALF + 1.0 + 1.0);
    for (got, want) in gains.iter().zip([SQRT_HALF, SQRT_HALF, 1.0, 0.0, 0.5, 0.5]) {
        approx(*got, want * scale);
    }
    approx(gains.iter().sum::<f32>(), 1.0);
}
