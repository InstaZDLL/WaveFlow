use super::*;

/// Runtime smoke probe against the real default output device: proves
/// the CoreAudio FFI actually executes (compile ≠ runs) and reports
/// which DoP rates the device can negotiate in 32-bit. Read-only — it
/// never takes hog mode. Ignored by default (needs macOS + a device);
/// run with:
///   cargo test -p waveflow coreaudio_dop_probe -- --ignored --nocapture
#[test]
#[ignore]
fn coreaudio_dop_probe() {
    let dev = get_default_device_id(false).expect("a default output device");
    let hog = get_hogging_pid(dev).unwrap_or(-99);
    println!("default output device id = {dev}, current hog pid = {hog}");

    // The property read the restore path depends on. Printing it
    // proves the raw HAL call actually executes and returns a
    // plausible description, not just that it compiles.
    match read_physical_stream_format(dev) {
        Ok(asbd) => println!(
            "current physical format: {} Hz, {} bits, {} ch, format id {:#x}",
            asbd.mSampleRate, asbd.mBitsPerChannel, asbd.mChannelsPerFrame, asbd.mFormatID
        ),
        Err(err) => panic!("reading the physical format failed: {err}"),
    }

    // The alive listener the device-loss path depends on.
    let mut alive = AliveListener::new(dev);
    alive.register().expect("registering the alive listener");
    assert!(alive.is_alive(), "a present device reports alive");
    println!("alive listener registered, device reports alive");
    for rate in [176_400u32, 352_800, 705_600] {
        let sf = StreamFormat {
            sample_rate: rate as f64,
            sample_format: SampleFormat::I32,
            flags: LinearPcmFlags::IS_SIGNED_INTEGER | LinearPcmFlags::IS_PACKED,
            channels: 2,
        };
        let ok = find_matching_physical_format(dev, sf).is_some();
        println!(
            "DoP {rate} Hz / S32 int / 2ch: {}",
            if ok { "SUPPORTED" } else { "not available" }
        );
    }
}
