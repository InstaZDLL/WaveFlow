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
