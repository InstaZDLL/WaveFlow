use super::*;

const NOW: Duration = Duration::from_millis(0);

#[test]
fn what_is_pushed_is_read_back_in_order_across_chunks() {
    let buffer = Buffer::new();
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    buffer.push(&data[..1000]);
    buffer.push(&data[1000..]);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while out.len() < data.len() {
        let n = buffer.read(&mut buf, NOW).unwrap();
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out, data);
    // Nothing unread is kept, and what was read is let go.
    assert!(matches!(
        buffer.read(&mut buf, NOW),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
    ));
    assert_eq!(buffer.held_bytes(), 0);
}

#[test]
fn a_reader_too_far_behind_loses_the_oldest_bytes_not_the_newest() {
    let buffer = Buffer::new();
    // 16 KB/s, a 128 kbit/s station: 30 minutes is 28.8 MB.
    buffer.set_byte_rate(16_000.0);
    let second = vec![7u8; 16_000];
    for _ in 0..(31 * 60) {
        buffer.push(&second);
    }
    let lag = buffer.lag();
    assert_eq!(lag.unread, Some(SPAN));
    assert_eq!(lag.dropped_bytes, 60 * 16_000);
    // The memory follows the bound, give or take a chunk.
    let held = buffer.held_bytes();
    assert!(
        (28_800_000..=28_800_000 + 2 * CHUNK).contains(&held),
        "{held}"
    );
}

#[test]
fn an_absurd_rate_or_a_flood_of_titles_is_not_trusted() {
    let buffer = Buffer::new();
    for rate in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e-300, 1e300] {
        buffer.set_byte_rate(rate);
    }
    buffer.push(&[0u8; 100]);
    assert_eq!(buffer.lag().unread, None);
    for i in 0..(MAX_TITLES * 3) {
        buffer.push_title(Some(format!("title {i}")));
    }
    assert_eq!(buffer.lock().titles.len(), MAX_TITLES);
}

#[test]
fn the_bound_follows_the_bitrate_within_its_limits() {
    assert_eq!(capacity_for(Some(40_000.0)), 72_000_000);
    assert_eq!(capacity_for(Some(100.0)), MIN_BYTES);
    assert_eq!(capacity_for(Some(1_000_000.0)), MAX_BYTES);
    assert_eq!(capacity_for(None), MAX_BYTES);
}

#[test]
fn titles_come_back_when_the_reader_reaches_them() {
    let buffer = Buffer::new();
    buffer.push(&[0u8; 100]);
    buffer.push_title(Some("Second song".into()));
    buffer.push(&[0u8; 100]);
    let mut buf = [0u8; 150];
    assert_eq!(buffer.read(&mut buf[..50], NOW).unwrap(), 50);
    assert_eq!(buffer.take_title(), None);
    assert_eq!(buffer.read(&mut buf, NOW).unwrap(), 150);
    assert_eq!(buffer.take_title(), Some(Some("Second song".into())));
    assert_eq!(buffer.take_title(), None);
}

#[test]
fn a_dropped_stretch_hands_over_the_title_it_carried() {
    let buffer = Buffer::new();
    buffer.set_byte_rate(800.0);
    buffer.push(&[1u8; 10]);
    buffer.push_title(Some("Skipped into".into()));
    assert_eq!(buffer.take_title(), None);
    // Full: the first ten bytes go, and with them the place the title
    // changed at.
    buffer.push(&vec![0u8; MIN_BYTES]);
    assert_eq!(buffer.lag().dropped_bytes, 10);
    assert_eq!(buffer.take_title(), Some(Some("Skipped into".into())));
}

#[test]
fn behind_is_the_time_spent_paused_even_in_the_first_seconds() {
    let buffer = Buffer::new();
    let t0 = Instant::now();
    // However much the station sent ahead, never pausing is live.
    buffer.push(&[0u8; 400_000]);
    assert_eq!(buffer.lag_at(t0).behind, Duration::ZERO);
    // Paused a second in, before anything settled, for 20 s.
    buffer.set_paused_at(true, t0 + Duration::from_secs(1));
    assert_eq!(
        buffer.lag_at(t0 + Duration::from_secs(11)).behind,
        Duration::from_secs(10)
    );
    buffer.set_paused_at(false, t0 + Duration::from_secs(21));
    let later = t0 + Duration::from_secs(300);
    assert_eq!(buffer.lag_at(later).behind, Duration::from_secs(20));
    // A second pause adds to it; a repeated call changes nothing.
    buffer.set_paused_at(true, later);
    buffer.set_paused_at(true, later + Duration::from_secs(4));
    buffer.set_paused_at(false, later + Duration::from_secs(5));
    buffer.set_paused_at(false, later + Duration::from_secs(9));
    assert_eq!(
        buffer.lag_at(later + Duration::from_secs(60)).behind,
        Duration::from_secs(25)
    );
}

#[test]
fn behind_stops_at_the_capacity() {
    let buffer = Buffer::new();
    buffer.set_byte_rate(16_000.0);
    let t0 = Instant::now();
    buffer.set_paused_at(true, t0);
    assert_eq!(buffer.lag_at(t0 + Duration::from_secs(3_600)).behind, SPAN);
}

#[test]
fn the_end_is_read_after_what_is_left() {
    let buffer = Buffer::new();
    buffer.push(&[1, 2, 3]);
    buffer.finish(End::Failed("gone".into()));
    let mut buf = [0u8; 8];
    assert_eq!(buffer.read(&mut buf, NOW).unwrap(), 3);
    assert!(buffer.read(&mut buf, NOW).is_err());
    // Nothing more is taken once ended.
    buffer.push(&[4]);
    assert!(buffer.read(&mut buf, NOW).is_err());
    let finished = Buffer::new();
    finished.finish(End::Finished);
    assert_eq!(finished.read(&mut buf, NOW).unwrap(), 0);
}

#[test]
fn a_waiting_reader_wakes_on_new_data() {
    let buffer = std::sync::Arc::new(Buffer::new());
    let writer = std::sync::Arc::clone(&buffer);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        writer.push(&[9u8; 4]);
    });
    let mut buf = [0u8; 8];
    assert_eq!(buffer.read(&mut buf, Duration::from_secs(5)).unwrap(), 4);
    handle.join().unwrap();
}
