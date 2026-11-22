use super::{Admit, DailyBudget};
use chrono::NaiveDate;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
}

#[test]
fn writes_pass_until_the_limit_then_one_notice_then_nothing() {
    let mut budget = DailyBudget::new(day(29), 0, 100);
    assert_eq!(budget.admit(day(29), 60), Admit::Write);
    assert_eq!(budget.admit(day(29), 40), Admit::Write);
    assert_eq!(budget.admit(day(29), 1), Admit::Notice);
    assert_eq!(budget.admit(day(29), 1), Admit::Drop);
}

#[test]
fn a_new_day_starts_a_new_budget() {
    let mut budget = DailyBudget::new(day(29), 0, 100);
    assert_eq!(budget.admit(day(29), 200), Admit::Notice);
    assert_eq!(budget.admit(day(30), 50), Admit::Write);
}

#[test]
fn a_restart_counts_what_today_already_wrote() {
    let mut budget = DailyBudget::new(day(29), 100, 100);
    assert_eq!(budget.admit(day(29), 1), Admit::Drop);
}

fn written<Tz: chrono::TimeZone>(time: &chrono::DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut out = String::new();
    super::write_log_time(
        &mut tracing_subscriber::fmt::format::Writer::new(&mut out),
        time,
    )
    .unwrap();
    out
}

#[test]
fn a_line_time_is_the_clock_with_its_offset() {
    use chrono::{FixedOffset, TimeZone};

    let instant = chrono::Utc
        .with_ymd_and_hms(2026, 11, 15, 0, 0, 0)
        .unwrap()
        .checked_add_signed(chrono::TimeDelta::microseconds(362_923))
        .unwrap();

    let east = instant.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
    assert_eq!(written(&east), "2026-11-15T08:00:00.362923+08:00");

    let west = instant.with_timezone(&FixedOffset::west_opt(5 * 3600 + 30 * 60).unwrap());
    assert_eq!(written(&west), "2026-11-14T18:30:00.362923-05:30");

    // A UTC time says +00:00, never Z: every line has the same shape.
    assert_eq!(written(&instant), "2026-11-15T00:00:00.362923+00:00");
}

#[test]
fn the_sinks_timer_writes_the_local_offset() {
    use tracing_subscriber::fmt::time::FormatTime;

    let mut out = String::new();
    super::LocalTime
        .format_time(&mut tracing_subscriber::fmt::format::Writer::new(&mut out))
        .unwrap();

    let parsed = chrono::DateTime::parse_from_rfc3339(&out).unwrap();
    assert_eq!(
        *parsed.offset(),
        chrono::Local::now().fixed_offset().timezone()
    );
}

/// The default filter and [`super::without_probe_junk`] hold back the two
/// lines symphonia writes for every file it opens, and only those: its other
/// warnings still pass, the probe's own about unreadable trailing metadata
/// included.
#[test]
fn the_default_filter_drops_symphonias_per_file_lines_only() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{EnvFilter, Layer};

    struct Targets(Arc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> Layer<S> for Targets {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            self.0
                .lock()
                .unwrap()
                .push(event.metadata().target().to_owned());
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(super::without_probe_junk())
        .with(EnvFilter::new(super::DEFAULT_FILTER))
        .with(Targets(Arc::clone(&seen)));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "symphonia_format_isomp4::demuxer", "skipping top-level atom");
        tracing::warn!(target: "symphonia_core::formats::probe", "skipped {} bytes of junk at {}", 4096, 0);
        tracing::warn!(
            target: "symphonia_core::formats::probe",
            "skipping trailing '{}' metdata due to error while reading it",
            "APEv2"
        );
        tracing::warn!(target: "symphonia_format_isomp4::atoms::ilst", "unsupported data type");
        tracing::warn!(target: "symphonia_core::codecs", "decode error");
        tracing::info!(target: "waveflow::scan", "scan finished");
        tracing::info!(target: "sqlx::query", "slow statement");
    });

    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        [
            "symphonia_core::formats::probe",
            "symphonia_format_isomp4::atoms::ilst",
            "symphonia_core::codecs",
            "waveflow::scan",
        ]
    );
}

/// symphonia logs through `log`: its probe lines reach the subscriber
/// through `tracing-log`, under the `log` target with their own in a field.
/// The junk line is still held back that way, and the trailing-metadata
/// warning still passes.
#[test]
fn the_probe_junk_line_is_held_back_when_it_comes_through_log() {
    use log::Log;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{EnvFilter, Layer};

    struct Messages(Arc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> Layer<S> for Messages {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            struct Message<'a>(&'a mut Vec<String>);
            impl tracing::field::Visit for Message<'_> {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0.push(format!("{value:?}"));
                    }
                }
            }
            event.record(&mut Message(&mut self.0.lock().unwrap()));
        }
    }

    let seen = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry()
        .with(super::without_probe_junk())
        .with(EnvFilter::new(super::DEFAULT_FILTER))
        .with(Messages(Arc::clone(&seen)));
    let bridge = tracing_log::LogTracer::new();
    let warn = |args: std::fmt::Arguments<'_>| {
        bridge.log(
            &log::Record::builder()
                .target("symphonia_core::formats::probe")
                .level(log::Level::Warn)
                .args(args)
                .build(),
        );
    };
    tracing::subscriber::with_default(subscriber, || {
        warn(format_args!("skipped {} bytes of junk at {}", 4096, 0));
        warn(format_args!(
            "skipping trailing '{}' metdata due to error while reading it",
            "APEv2"
        ));
    });

    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        ["skipping trailing 'APEv2' metdata due to error while reading it"]
    );
}
