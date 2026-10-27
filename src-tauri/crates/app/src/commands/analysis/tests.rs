use super::code_is_busy;

/// The slot hands itself out once, records a request made while it
/// is taken (only when asked to), and a run that starts afresh
/// covers what was waiting. The only test touching `SLOT`, so the
/// shared static is not raced by another.
#[test]
fn the_slot_queues_one_request_while_taken() {
    use super::{reserve, slot};
    {
        let mut s = slot();
        s.running = false;
        s.rerun = false;
    }
    assert!(reserve(false), "a free slot is taken");
    assert!(!reserve(false), "a taken slot refuses");
    assert!(!slot().rerun, "a manual refusal queues nothing");
    assert!(!reserve(true), "still taken");
    assert!(slot().rerun, "an automatic refusal is kept");
    // What `RunningGuard::drop` does, minus the call out.
    {
        let mut s = slot();
        s.running = false;
    }
    assert!(reserve(true), "free again");
    assert!(!slot().rerun, "the new run covers the waiting request");
    slot().running = false;
}

#[test]
fn busy_and_locked_primary_codes_match() {
    // Primary BUSY / LOCKED.
    assert!(code_is_busy("5"));
    assert!(code_is_busy("6"));
    // Extended flavours fold to the same primary in the low byte:
    // BUSY_SNAPSHOT = 517 (5 | 2<<8), BUSY_RECOVERY = 261,
    // LOCKED_SHAREDCACHE = 262.
    assert!(code_is_busy("517"));
    assert!(code_is_busy("261"));
    assert!(code_is_busy("262"));
}

#[test]
fn non_contention_codes_do_not_match() {
    // CONSTRAINT = 19, READONLY = 8, CANTOPEN = 14, OK = 0.
    assert!(!code_is_busy("19"));
    assert!(!code_is_busy("8"));
    assert!(!code_is_busy("14"));
    assert!(!code_is_busy("0"));
    // Garbage / non-numeric never counts as retryable.
    assert!(!code_is_busy(""));
    assert!(!code_is_busy("locked"));
}
