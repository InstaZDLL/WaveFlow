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

#[test]
fn a_line_time_is_the_local_clock_with_its_offset() {
    use super::LOG_TIME_FORMAT;
    use chrono::{FixedOffset, TimeZone};

    let instant = chrono::Utc
        .with_ymd_and_hms(2026, 11, 15, 0, 0, 0)
        .unwrap()
        .checked_add_signed(chrono::TimeDelta::microseconds(362_923))
        .unwrap();

    let east = instant.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
    assert_eq!(
        east.format(LOG_TIME_FORMAT).to_string(),
        "2026-11-15T08:00:00.362923+08:00"
    );

    let west = instant.with_timezone(&FixedOffset::west_opt(5 * 3600 + 30 * 60).unwrap());
    assert_eq!(
        west.format(LOG_TIME_FORMAT).to_string(),
        "2026-11-14T18:30:00.362923-05:30"
    );

    let utc = instant.with_timezone(&FixedOffset::east_opt(0).unwrap());
    assert_eq!(
        utc.format(LOG_TIME_FORMAT).to_string(),
        "2026-11-15T00:00:00.362923+00:00"
    );
}
