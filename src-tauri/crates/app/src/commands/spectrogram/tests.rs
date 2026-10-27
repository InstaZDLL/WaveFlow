use super::*;

#[test]
fn a_new_decode_stops_the_one_in_flight_and_keeps_the_slot() {
    let jobs = SpectrogramJobs::default();
    let first = jobs.begin();
    let second = jobs.begin();
    assert!(
        first.load(Ordering::Acquire),
        "the older decode is told to stop"
    );
    assert!(!second.load(Ordering::Acquire));
    // The older one finishing late must not free the newer one's slot.
    jobs.end(&first);
    jobs.cancel();
    assert!(second.load(Ordering::Acquire));
    jobs.end(&second);
    assert!(jobs.current.lock().unwrap().is_none());
}

#[test]
fn the_cache_keeps_the_latest_picture_per_file() {
    let jobs = SpectrogramJobs::default();
    let picture = |columns| {
        Arc::new(TrackSpectrogram {
            sample_rate: 44_100,
            columns,
            rows: 256,
            seconds_per_column: 0.1,
            duration_seconds: 1.0,
            range_db: 120.0,
            cells: String::new(),
            cutoff_hz: None,
        })
    };
    let old: CacheKey = (PathBuf::from("a.flac"), 10, None);
    let new: CacheKey = (PathBuf::from("a.flac"), 11, None);
    jobs.remember(old.clone(), picture(1));
    jobs.remember(new.clone(), picture(2));
    // A file replaced in place is a new key, and the stale picture goes.
    assert!(jobs.cached(&old).is_none());
    assert_eq!(jobs.cached(&new).unwrap().columns, 2);
    for i in 0..CACHE_LEN + 2 {
        jobs.remember((PathBuf::from(format!("{i}.flac")), 1, None), picture(i));
    }
    assert_eq!(jobs.cache.lock().unwrap().len(), CACHE_LEN);
}
