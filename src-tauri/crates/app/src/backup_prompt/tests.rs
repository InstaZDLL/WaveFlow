use super::*;
use std::path::PathBuf;

#[test]
fn each_button_is_its_choice() {
    for moment in [Moment::Launch, Moment::Running] {
        assert_eq!(choice_for(Some(RETRY)), Choice::Retry);
        assert_eq!(choice_for(Some(CONTINUE)), Choice::Continue);
        assert_eq!(choice_for(Some(moment.stop_label())), Choice::Quit);
    }
}

#[test]
fn the_child_s_answer_survives_its_exit_status() {
    for choice in [Choice::Retry, Choice::Continue, Choice::Quit] {
        assert_eq!(choice_from_exit(Some(exit_code(choice))), choice);
    }
    // A child that crashed, or was killed by a signal.
    assert_eq!(choice_from_exit(Some(101)), Choice::Quit);
    assert_eq!(choice_from_exit(None), Choice::Quit);
}

#[test]
fn a_closed_window_changes_nothing() {
    assert_eq!(choice_for(None), Choice::Quit);
    assert_eq!(choice_for(Some("Yes")), Choice::Quit);
}

#[test]
fn the_text_names_the_database_and_the_reason() {
    let failure = SnapshotFailure {
        db: PathBuf::from("/data/profiles/3/data.db"),
        reason: "the copy could not be written: disk full".into(),
    };
    let text = body(DbScope::Profile, &failure, Moment::Launch);
    assert!(text.starts_with("This profile's library has to be updated"));
    assert!(text.contains("disk full"));
    assert!(text.contains("/data/profiles/3/data.db"));
    assert!(text.contains("quit and leave it untouched"));
    assert!(body(DbScope::App, &failure, Moment::Running).contains("cancel and leave"));
}
