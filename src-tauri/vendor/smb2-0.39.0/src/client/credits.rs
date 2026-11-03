//! Connection-wide SMB2 credit accounting.
//!
//! A server hands the client a budget ("credits") and every request spends
//! from it: `CreditCharge = ceil(max(SendPayload, ExpectedResponse) / 65536)`,
//! at least 1 (MS-SMB2 § 3.1.5.2). Responses carry a `CreditResponse` grant
//! that puts credits back. A client that sends more than it holds is in
//! violation, and MS-SMB2 § 3.3.1.1 lets the server drop the connection —
//! some servers instead stop answering while the TCP socket stays open, which
//! looks exactly like a hung client.
//!
//! The budget is per *connection*, so it lives here rather than in any one
//! stream: several pipelined transfers over one connection draw on the same
//! pool.
//!
//! **NEGOTIATE is exempt, not funded.** Before the first grant a client may
//! send NEGOTIATE and nothing else (MS-SMB2 § 3.2.5.1.1). Modelling that as a
//! seeded permit in the pool is the trap: a permit is fungible, so a request
//! racing the handshake — a watcher re-arming, a listing retrying, anything
//! that arrives while a revived connection is re-negotiating — can spend it
//! and put a frame on the wire the server funded nothing for. The server
//! discards that frame in silence, so the client learns nothing and waits out
//! a full response deadline for an answer that was never coming. So the pool
//! starts EMPTY and NEGOTIATE carries a [`CreditReservation::exempt`], which
//! is the one shape nothing else can take.
//!
//! **Credits are spent on send, not on receipt.** That is the whole point of
//! this type. Accounting for a request only once its answer arrives leaves
//! everything currently in flight invisible, and concurrent senders each read
//! the same "plenty available" number and pile on.
//!
//! **A charge wider than the window fails at once, not after the deadline.**
//! Waiting only makes sense for a charge the server could still fund. One
//! wider than the whole window never can, and because the pool is fair it
//! doesn't wait alone: it soaks up every credit coming back, and every request
//! queued behind it on the connection waits with it. So the pool tracks the
//! window as well as what's unspent (see [`Window`]), and
//! [`CreditPool::can_never_fund`] answers from it.
//!
//! "The window" is every credit the server has handed out and not yet been
//! answered for: what's unspent plus what rides on requests still in flight.
//! It's what the server bounds (Samba's `smb2 max credits`, Windows'
//! `Smb2CreditsMax`), so it's also the most a single request can ever charge.
//! But a small window isn't a verdict on its own. Every request asks the server
//! to grow it (see [`CreditPool::request_for`]), and servers ramp: Windows
//! Server before 2016 grants 32 more at a time, and Samba declines to grow on
//! every session setup leg but the last, then grows on that one (Samba
//! `source3/smbd/smb2_server.c`, `smb2_set_operation_credit`, read on master
//! 2026-09-23; it also cuts the connection outright on a charge above its
//! maximum). So the window only has a CEILING once a response to a request
//! that asked for more came back granting no more than that request consumed,
//! and growth past that point takes the ceiling away again. Only a charge wider
//! than a known ceiling fails fast; anything else waits as before.
//!
//! Replies are read one at a time, but servers grant for several at once: a
//! compound's whole grant rides on one reply and the rest carry 0, and Samba
//! answers a pipeline at its maximum unevenly (one reply double, the next
//! nothing). So a 0 is no verdict either way, and growth only counts once it
//! exceeds the ceiling by more than what's still in flight
//! ([`Window::answer`]). Without both, any compound forgot a known ceiling.
//!
//! The ceiling also sizes the requests this crate picks the size of (chunks,
//! compound limits): see [`CreditPool::comfortable_charge`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{AcquireError, Semaphore};

use crate::msg::header::Header;
use crate::types::{CreditCharge, MessageId};

/// The window the client steers the server toward, in credits.
///
/// Every request asks for its own charge back plus whatever is needed to reach
/// this number, so an idle connection asks for little and a saturated one asks
/// for a lot. Servers clamp the request to their own maximum, so asking high
/// is safe; asking low is not, because a window that shrinks to nothing
/// serializes every transfer.
///
/// 512 covers the deepest pipeline this crate opens (32 requests at 8 credits
/// each for 512 KB chunks) with room for other work on the same connection.
/// It is *not* generous for whole-file compound reads: one CREATE+READ+CLOSE
/// asking for a full 8 MiB `MaxReadSize` charges 130, so only three fit at
/// once. That is a property of the request, not of this number, and the fix is
/// on the caller's side: read with the size you actually want
/// (`Tree::read_file_compound_sized`) and size the batch with
/// [`Connection::credit_capacity_for`](crate::client::Connection::credit_capacity_for).
/// ❌ Don't raise this to buy concurrency for oversized requests: the server
/// clamps it anyway, and the requests stay just as expensive.
const CREDIT_TARGET: u16 = 512;

/// The credits a request costs: `ceil(bytes / 65536)`, at least 1 (MS-SMB2
/// § 3.1.5.2).
///
/// `bytes` is the larger of the send payload and the *expected* response, so a
/// READ pays for the length it asked for whether or not the file fills it.
/// That is why a read should ask for the size it actually wants: a 4 KiB file
/// read with an 8 MiB request costs 128 credits and crowds out every other
/// request on the connection.
pub(crate) fn charge_for_payload(bytes: u64) -> u16 {
    bytes.div_ceil(65536).clamp(1, u16::MAX as u64) as u16
}

/// How many requests charging `charge` each fit in the window the client
/// steers toward, or in the server's `ceiling` when it has shown one below it.
///
/// An estimate of steady state, not a reading of what is unspent right now:
/// other work on the connection draws on the same pool. Never 0, because a
/// caller uses this to size a batch and a batch of nothing makes no progress.
pub(crate) fn capacity_for_charge(charge: u16, ceiling: Option<u16>) -> usize {
    let window = ceiling.map_or(CREDIT_TARGET, |c| c.min(CREDIT_TARGET));
    usize::from((window / charge.max(1)).max(1))
}

/// The server's credit window as the client can reconstruct it, and whether it
/// has stopped growing. See the module docs for why the pool needs this.
#[derive(Default)]
struct Window {
    /// Credits granted and not yet answered for: unspent, reserved, or riding
    /// on a request in flight. Signed so a server that answers for more than it
    /// granted (none should) can't wrap it.
    size: i64,
    /// Where the window stopped growing, or `None` while it may still grow.
    ceiling: Option<u16>,
    /// Requests on the wire (or about to be) whose first response hasn't
    /// arrived, by `MessageId`. The first response settles the request: it's
    /// the one that carries the grant, since a final response after an interim
    /// one grants nothing.
    awaiting: HashMap<u64, Expected>,
    /// The sum of `awaiting`'s charges: the part of `size` riding on requests
    /// whose answer hasn't come back. Kept by [`expect`](Self::expect) and
    /// [`settle`](Self::settle), the only two ways in and out of `awaiting`.
    in_flight: i64,
}

/// What a request in flight holds of the window, and what it asked for.
#[derive(Clone, Copy)]
struct Expected {
    charge: u16,
    requested: u16,
}

impl Window {
    /// Enter a request that is about to go on the wire.
    fn expect(&mut self, msg_id: MessageId, expected: Expected) {
        if let Some(old) = self.awaiting.insert(msg_id.0, expected) {
            self.in_flight -= i64::from(old.charge);
        }
        self.in_flight += i64::from(expected.charge);
    }

    /// Take a request out of the window's in-flight record: answered, or
    /// refunded because it never reached the wire.
    fn settle(&mut self, msg_id: MessageId) -> Option<Expected> {
        let expected = self.awaiting.remove(&msg_id.0)?;
        self.in_flight -= i64::from(expected.charge);
        Some(expected)
    }

    /// Settle the request `msg_id` with the `granted` credits its response
    /// carried, and read the server's intent off it.
    fn answer(&mut self, msg_id: MessageId, granted: u16) {
        self.size += i64::from(granted);
        let Some(expected) = self.settle(msg_id) else {
            return;
        };
        self.size -= i64::from(expected.charge);
        // A request that didn't ask to grow the window says nothing about
        // whether the server would have.
        if expected.requested <= expected.charge {
            return;
        }
        // Neither is a reply granting nothing: Samba and Windows put a
        // compound's whole grant on one reply and 0 on the others (Samba
        // `smb2_calculate_credits`), so a 0 is usually a grant that rides on a
        // sibling, not the server declining.
        if granted == 0 {
            return;
        }
        if granted <= expected.charge {
            self.ceiling = Some(u16::try_from(self.size.max(0)).unwrap_or(u16::MAX));
            return;
        }
        // Growth counts only past what's still in flight. A grant that covers
        // siblings (a compound's, or Samba answering a pipeline unevenly)
        // lifts `size` above the ceiling until those siblings' charges come
        // off, which is the window staying put, not growing. Reading it as
        // growth forgot the ceiling on every compound, and the next chunked
        // transfer sized itself past what the server funds.
        if self
            .ceiling
            .is_some_and(|ceiling| self.size - self.in_flight > i64::from(ceiling))
        {
            self.ceiling = None;
        }
    }
}

/// Default bound on how long a send waits for the server to grant credits
/// before giving up with [`Error::CreditStarvation`](crate::Error::CreditStarvation).
///
/// Long enough that a merely busy server is never mistaken for a dead one,
/// short enough that a silent one surfaces as an error instead of a hang.
pub(crate) const DEFAULT_CREDIT_WAIT: Duration = Duration::from_secs(30);

/// Server-granted credits that have not been spent yet.
///
/// One `Semaphore` permit per unspent credit. Taking permits is the gate: a
/// request acquires its `CreditCharge` worth *before* its bytes reach the
/// wire, and only a grant on a response puts them back. Waiting is bounded by
/// the caller (see `Inner::reserve_credits`), so a server that stops granting
/// produces an error rather than a wait that never ends.
pub(crate) struct CreditPool {
    /// The live session's budget. Behind a `Mutex<Arc<..>>` rather than owned
    /// outright because a closed `Semaphore` can never reopen (tokio makes
    /// closing terminal), and a connection revived in place needs a budget
    /// again. [`reset`](Self::reset) swaps in a fresh one; whoever still holds
    /// the old `Arc` is parked on a generation that is never coming back and
    /// sees `Err` from the close that killed it.
    budget: Mutex<Arc<Budget>>,
    /// The reserve deadline in milliseconds, tunable per connection.
    wait_ms: AtomicU64,
}

/// One session's credits: the unspent ones, and the whole window.
///
/// Together, because a [`CreditReservation`] refunds into the budget it was
/// taken from, permits and window alike. A reservation that outlives a
/// [`reset`](CreditPool::reset) (a send that waited through a revival, then
/// gave up unsent) would otherwise hand the dead session's credits to the new
/// one, which can then out-spend the server's window (AGENTS.md pitfall 14),
/// and settle its MessageIds in the new window, where the counter started from
/// zero again and the same id is a live request.
struct Budget {
    /// One permit per unspent credit.
    permits: Semaphore,
    /// The whole window, in flight included, and its ceiling.
    window: Mutex<Window>,
}

impl Budget {
    fn empty() -> Arc<Self> {
        Arc::new(Self {
            permits: Semaphore::new(0),
            window: Mutex::new(Window::default()),
        })
    }

    fn available(&self) -> u16 {
        self.permits.available_permits().min(u16::MAX as usize) as u16
    }

    fn put_back(&self, credits: u16) {
        if credits > 0 {
            self.permits.add_permits(credits as usize);
        }
    }

    /// How many credits to request on a request charging `charge`.
    ///
    /// Always at least the charge, so the window can't shrink under a steady
    /// load, plus enough to climb back to [`CREDIT_TARGET`].
    fn request_for(&self, charge: u16) -> u16 {
        charge.saturating_add(CREDIT_TARGET.saturating_sub(self.available()))
    }

    /// Hand back credits reserved for a request whose bytes never reached the
    /// wire (a signing failure, a transport error). Once the bytes are out,
    /// the credits are the server's and only a grant returns them.
    ///
    /// The window doesn't move: those credits never left the client.
    fn refund(&self, charge: u16, expected: &[MessageId]) {
        if !expected.is_empty() {
            let mut window = self.window.lock().unwrap();
            for msg_id in expected {
                window.settle(*msg_id);
            }
        }
        self.put_back(charge);
    }
}

impl CreditPool {
    /// A fresh pool is empty: the server funds every credit, and it has not
    /// granted one yet.
    ///
    /// The one thing a client may send before a grant is NEGOTIATE (MS-SMB2
    /// § 3.2.5.1.1), and that is an exemption rather than a credit — see
    /// [`CreditReservation::exempt`]. ❌ Don't seed a permit for it here: a
    /// permit is fungible, so anything racing the handshake can spend it
    /// instead and put a frame on the wire the server funded nothing for.
    pub(crate) fn new() -> Self {
        Self {
            budget: Mutex::new(Budget::empty()),
            wait_ms: AtomicU64::new(DEFAULT_CREDIT_WAIT.as_millis() as u64),
        }
    }

    /// The budget as of right now.
    fn current(&self) -> Arc<Budget> {
        Arc::clone(&self.budget.lock().unwrap())
    }

    /// Throw the spent budget away and start again from empty, the way a
    /// brand-new connection starts.
    ///
    /// Called when a connection is revived on a new transport. ❌ Don't reuse
    /// the old budget: its permits were granted by a session that no longer
    /// exists, and the new server may have a much smaller window. Carrying
    /// them over would let the first burst after a reconnect out-spend the
    /// server exactly the way the original wedge did.
    pub(crate) fn reset(&self) {
        *self.budget.lock().unwrap() = Budget::empty();
    }

    /// Credits on hand: granted by the server and not reserved by a request.
    ///
    /// Saturates at `u16::MAX`; no server grants a window that wide.
    pub(crate) fn available(&self) -> u16 {
        self.current().available()
    }

    /// Bank a grant no request settles, the way [`answer`](Self::answer)
    /// treats an unsolicited notification, a final response after its interim
    /// one, or the response to NEGOTIATE.
    #[cfg(test)]
    pub(crate) fn grant(&self, credits: u16) {
        let budget = self.current();
        budget.window.lock().unwrap().size += i64::from(credits);
        budget.put_back(credits);
    }

    /// Bank the `CreditResponse` from a response to `msg_id`, settling that
    /// request's share of the window.
    pub(crate) fn answer(&self, msg_id: MessageId, credits: u16) {
        let budget = self.current();
        budget.window.lock().unwrap().answer(msg_id, credits);
        budget.put_back(credits);
    }

    /// Where the server's window stopped growing, or `None` while it may
    /// still grow. See the module docs.
    pub(crate) fn ceiling(&self) -> Option<u16> {
        self.current().window.lock().unwrap().ceiling
    }

    /// Whether `charge` is wider than a window the server has stopped
    /// growing, so no amount of waiting can fund it.
    pub(crate) fn can_never_fund(&self, charge: u16) -> bool {
        self.ceiling().is_some_and(|ceiling| charge > ceiling)
    }

    /// The most a request whose size this crate picks (a chunk, a compound
    /// limit) should charge, or `None` while the window has no ceiling.
    ///
    /// Half the ceiling, at least one credit. A request charging the whole
    /// ceiling is funded only once everything else on the connection has been
    /// answered, and a watcher's long poll is never answered on cue, so it
    /// would wait out the deadline. Half leaves room for that, and for a second
    /// chunk in flight behind the first.
    pub(crate) fn comfortable_charge(&self) -> Option<u16> {
        self.ceiling().map(|ceiling| (ceiling / 2).max(1))
    }

    /// Take `charge` credits if they are on hand right now.
    pub(crate) fn try_reserve(&self, charge: u16) -> Option<CreditReservation> {
        let budget = self.current();
        budget
            .permits
            .try_acquire_many(u32::from(charge))
            .ok()?
            .forget();
        Some(CreditReservation::new(budget, charge))
    }

    /// Wait for `charge` credits. Resolves once they are reserved, or with
    /// `Err` if the pool was closed by a connection teardown.
    ///
    /// Fair: waiters are served in order, so a large request can't be starved
    /// by a stream of small ones behind it.
    ///
    /// Binds to the budget as it is when the wait starts, so a reset mid-wait
    /// resolves the waiter with `Err` (the pool it was queued on was closed)
    /// rather than silently migrating it onto the new session's budget. The
    /// reservation stays bound to that budget too, refund included.
    pub(crate) async fn reserve(&self, charge: u16) -> Result<CreditReservation, AcquireError> {
        let budget = self.current();
        budget
            .permits
            .acquire_many(u32::from(charge))
            .await?
            .forget();
        Ok(CreditReservation::new(budget, charge))
    }

    /// [`Budget::request_for`] on the live budget.
    #[cfg(test)]
    pub(crate) fn request_for(&self, charge: u16) -> u16 {
        self.current().request_for(charge)
    }

    /// How long [`Inner::reserve_credits`](crate::client::connection) waits
    /// before declaring starvation.
    pub(crate) fn wait_timeout(&self) -> Duration {
        Duration::from_millis(self.wait_ms.load(Ordering::Relaxed))
    }

    /// Retune the starvation deadline.
    pub(crate) fn set_wait_timeout(&self, after: Duration) {
        let ms = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
        self.wait_ms.store(ms, Ordering::Relaxed);
    }

    /// Wake every waiter with an error. Called when the connection dies, so a
    /// task parked on credits fails immediately instead of waiting out the
    /// full deadline for a server that will never answer again.
    pub(crate) fn close(&self) {
        self.current().permits.close();
    }

    /// Whether [`close`](Self::close) has been called.
    pub(crate) fn is_closed(&self) -> bool {
        self.current().permits.is_closed()
    }

    /// Force the pool to exactly `credits`, for tests that need to stage a
    /// specific window without a full negotiate exchange.
    #[cfg(test)]
    pub(crate) fn set_ceiling(&self, ceiling: u16) {
        self.current().window.lock().unwrap().ceiling = Some(ceiling);
    }

    #[cfg(test)]
    pub(crate) fn set_available(&self, credits: u16) {
        let budget = self.current();
        budget.window.lock().unwrap().size = i64::from(credits);
        let permits = &budget.permits;
        let have = permits.available_permits();
        let want = usize::from(credits);
        match want.cmp(&have) {
            std::cmp::Ordering::Greater => permits.add_permits(want - have),
            std::cmp::Ordering::Less => {
                if let Ok(permit) = permits.try_acquire_many((have - want) as u32) {
                    permit.forget();
                }
            }
            std::cmp::Ordering::Equal => {}
        }
    }
}

/// Credits taken from the pool for one request that has not been sent yet.
///
/// Dropping without [`commit`](Self::commit) refunds them — that is the path
/// for a request that failed to sign, encrypt, or reach the transport, and for
/// a caller whose future is dropped before the send. Only
/// [`CreditPool::try_reserve`] and [`CreditPool::reserve`] make one, so it
/// always holds the budget it debited, and refunds into that one even after a
/// revival swapped in another (see [`Budget`]).
#[must_use = "dropping the reservation refunds the credits without sending"]
pub(crate) struct CreditReservation {
    budget: Option<Arc<Budget>>,
    charge: u16,
    /// The requests [`stamp`](Self::stamp) entered in the window, forgotten
    /// again if they never reach the wire.
    stamped: Vec<MessageId>,
}

impl CreditReservation {
    fn new(budget: Arc<Budget>, charge: u16) -> Self {
        Self {
            budget: Some(budget),
            charge,
            stamped: Vec::new(),
        }
    }

    /// A reservation for the one request that spends no credits: NEGOTIATE.
    ///
    /// MS-SMB2 § 3.2.5.1.1 lets a client send NEGOTIATE holding nothing,
    /// because every credit it will ever hold arrives on the response. That
    /// makes NEGOTIATE *exempt* from the window rather than funded by it, and
    /// exemption is the only shape that can't be spent by something else:
    /// a seeded permit in the pool is fungible, and a request that grabs it
    /// while the handshake is still in flight sends a frame the server funded
    /// nothing for. Servers answer such a frame by silently discarding it, so
    /// the client waits out a full response deadline for an answer that was
    /// never coming.
    pub(crate) fn exempt() -> Self {
        Self {
            budget: None,
            charge: 0,
            stamped: Vec::new(),
        }
    }

    /// Fill in `header`'s `CreditCharge` and `CreditRequest` for a request
    /// charging `charge` out of this reservation, and enter it in the window
    /// so its response can settle it.
    ///
    /// Before the bytes go out, never after: a fast server can answer before
    /// the send returns, and an answer to a request the window has no record
    /// of can't be settled. A compound stamps each of its requests on one
    /// reservation.
    pub(crate) fn stamp(&mut self, header: &mut Header, charge: u16) {
        let Some(budget) = &self.budget else {
            return;
        };
        let requested = budget.request_for(charge);
        header.credit_charge = CreditCharge(charge);
        header.credits = requested;
        budget
            .window
            .lock()
            .unwrap()
            .expect(header.message_id, Expected { charge, requested });
        self.stamped.push(header.message_id);
    }

    /// The bytes are on the wire: the credits belong to the server now.
    pub(crate) fn commit(mut self) {
        self.budget = None;
    }
}

impl Drop for CreditReservation {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.refund(self.charge, &self.stamped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reservation_holds_credits_out_of_the_pool_until_it_is_refunded() {
        let pool = CreditPool::new();
        pool.set_available(10);

        let reservation = pool.try_reserve(4).unwrap();
        assert_eq!(pool.available(), 6);

        drop(reservation);
        assert_eq!(
            pool.available(),
            10,
            "an unsent request gives its credits back"
        );
    }

    #[test]
    fn a_committed_reservation_leaves_the_credits_with_the_server() {
        let pool = CreditPool::new();
        pool.set_available(10);

        pool.try_reserve(4).unwrap().commit();

        assert_eq!(
            pool.available(),
            6,
            "credits spent on the wire only come back as a grant"
        );
    }

    #[test]
    fn a_charge_larger_than_the_window_is_not_partially_reserved() {
        let pool = CreditPool::new();
        pool.set_available(3);

        assert!(pool.try_reserve(4).is_none());
        assert_eq!(pool.available(), 3, "a failed reserve takes nothing");
    }

    #[test]
    fn the_credit_request_always_covers_the_charge_and_climbs_to_the_target() {
        let pool = CreditPool::new();

        pool.set_available(0);
        assert_eq!(pool.request_for(8), 8 + CREDIT_TARGET);

        pool.set_available(CREDIT_TARGET);
        assert_eq!(
            pool.request_for(8),
            8,
            "at target, ask only for the charge back"
        );

        pool.set_available(u16::MAX);
        assert_eq!(pool.request_for(8), 8, "never ask for less than the charge");
    }

    #[tokio::test]
    async fn a_grant_wakes_a_waiter() {
        let pool = CreditPool::new();
        pool.set_available(0);

        let waiting = pool.reserve(4);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err(),
            "nothing to reserve yet"
        );

        pool.grant(4);
        waiting
            .await
            .expect("the grant satisfies the waiter")
            .commit();
        assert_eq!(pool.available(), 0);
    }

    #[tokio::test]
    async fn closing_the_pool_fails_waiters_instead_of_parking_them() {
        let pool = CreditPool::new();
        pool.set_available(0);

        let waiting = pool.reserve(1);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );

        pool.close();
        assert!(waiting.await.is_err());
        assert!(pool.is_closed());
    }

    #[test]
    fn a_fresh_pool_funds_nothing_until_the_server_grants() {
        let pool = CreditPool::new();

        assert_eq!(pool.available(), 0);
        assert!(
            pool.try_reserve(1).is_none(),
            "a credit the server never granted must not be spendable: \
             NEGOTIATE is exempt from the window, and an exemption is not a \
             permit anything else can take"
        );
    }

    #[tokio::test]
    async fn a_reset_pool_holds_no_ungranted_credit_and_is_no_longer_closed() {
        let pool = CreditPool::new();
        pool.set_available(400);
        pool.close();
        assert!(pool.is_closed());

        pool.reset();

        assert!(
            !pool.is_closed(),
            "a revived connection needs a live budget"
        );
        assert_eq!(
            pool.available(),
            0,
            "credits granted by a dead session must not carry over -- the new \
             server's window may be far smaller, and until it says so the \
             client holds nothing"
        );
        assert!(
            pool.try_reserve(1).is_none(),
            "a request racing the revival must wait for a real grant rather \
             than spend the handshake's exemption"
        );
    }

    #[tokio::test]
    async fn a_waiter_on_the_old_budget_is_failed_by_the_reset_rather_than_migrated() {
        let pool = CreditPool::new();
        pool.set_available(0);

        let waiting = pool.reserve(4);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err(),
            "nothing to reserve yet"
        );

        // Teardown, then revival. The waiter belongs to the dead generation.
        pool.close();
        pool.reset();
        pool.grant(64);

        assert!(
            tokio::time::timeout(Duration::from_millis(200), waiting)
                .await
                .expect("the waiter must resolve, not hang")
                .is_err(),
            "a send queued against the old session must fail rather than \
             silently continue on the new one"
        );
    }

    /// Send one request charging `charge` out of `pool`, entered in the window
    /// under `msg_id` with whatever it asks the server for.
    fn send(pool: &CreditPool, msg_id: u64, charge: u16) {
        let mut reservation = pool.try_reserve(charge).unwrap();
        let mut header = Header::new_request(crate::types::Command::Echo);
        header.message_id = MessageId(msg_id);
        reservation.stamp(&mut header, charge);
        reservation.commit();
    }

    #[test]
    fn a_response_that_declines_to_grow_the_window_marks_its_ceiling() {
        let pool = CreditPool::new();
        pool.set_available(64);

        send(&pool, 1, 1);
        assert_eq!(pool.ceiling(), None, "in flight, nothing is known yet");
        pool.answer(MessageId(1), 1);

        assert_eq!(pool.ceiling(), Some(64));
        assert!(pool.can_never_fund(65));
        assert!(!pool.can_never_fund(64), "the whole window is fundable");
    }

    #[test]
    fn the_window_counts_credits_in_flight_not_only_the_unspent_ones() {
        let pool = CreditPool::new();
        pool.set_available(64);

        // A long poll holds 16 of the 64 while the server declines to grow.
        send(&pool, 1, 16);
        send(&pool, 2, 1);
        pool.answer(MessageId(2), 1);

        assert_eq!(pool.available(), 48);
        assert_eq!(
            pool.ceiling(),
            Some(64),
            "the long poll's credits come back when it's answered, so they're window"
        );
    }

    #[test]
    fn growth_past_the_ceiling_means_the_window_is_still_ramping() {
        let pool = CreditPool::new();
        pool.set_available(1);

        // Samba's shape: the first session setup leg grants the charge only.
        send(&pool, 1, 1);
        pool.answer(MessageId(1), 1);
        assert_eq!(pool.ceiling(), Some(1));

        send(&pool, 2, 1);
        pool.answer(MessageId(2), 33);
        assert_eq!(pool.ceiling(), None);
        assert!(!pool.can_never_fund(500));
    }

    #[test]
    fn a_compound_at_the_ceiling_keeps_it() {
        let pool = CreditPool::new();
        pool.set_available(64);
        send(&pool, 1, 1);
        pool.answer(MessageId(1), 1);
        assert_eq!(pool.ceiling(), Some(64));

        // Samba and Windows put a compound's whole grant on its LAST reply
        // and 0 on the others (Samba `smb2_calculate_credits`: "To match
        // Windows"). Read reply by reply, the 0 looks like the window
        // shrinking and the 2 like it growing again, and together they
        // forgot a ceiling the server never moved.
        send(&pool, 2, 1);
        send(&pool, 3, 1);
        pool.answer(MessageId(2), 0);
        pool.answer(MessageId(3), 2);

        assert_eq!(pool.ceiling(), Some(64));
        assert!(pool.can_never_fund(81));

        // The replies are routed in either order. Settling the grant first
        // puts the window one above the ceiling until the sibling's charge
        // comes off, which is not the server growing it.
        send(&pool, 4, 1);
        send(&pool, 5, 1);
        pool.answer(MessageId(4), 2);
        pool.answer(MessageId(5), 0);

        assert_eq!(pool.ceiling(), Some(64));
    }

    #[test]
    fn a_pipeline_keeps_the_ceiling_while_the_server_grants_unevenly() {
        // Samba answering pipelined WRITEs at its 64-credit maximum: one reply
        // grants double its charge while the next is still in flight, and the
        // next grants nothing (seen against smb-smallcredits, 2026-09-23).
        let pool = CreditPool::new();
        pool.set_available(64);
        send(&pool, 1, 32);
        pool.answer(MessageId(1), 32);
        assert_eq!(pool.ceiling(), Some(64));

        send(&pool, 2, 32);
        send(&pool, 3, 32);
        pool.answer(MessageId(2), 64);
        pool.answer(MessageId(3), 0);

        assert_eq!(pool.ceiling(), Some(64));
    }

    #[test]
    fn a_request_that_asked_for_no_growth_says_nothing_about_the_ceiling() {
        let pool = CreditPool::new();
        pool.set_available(CREDIT_TARGET + 1);

        // Still at the target once its charge is out, a request asks for
        // that charge back and no more.
        send(&pool, 1, 1);
        pool.answer(MessageId(1), 1);

        assert_eq!(pool.ceiling(), None);
    }

    #[test]
    fn only_the_first_response_settles_a_request() {
        let pool = CreditPool::new();
        pool.set_available(64);

        // An interim STATUS_PENDING carries the grant, the final answer none.
        send(&pool, 1, 1);
        pool.answer(MessageId(1), 1);
        pool.answer(MessageId(1), 0);

        assert_eq!(
            pool.ceiling(),
            Some(64),
            "the final answer must not take the charge out of the window twice"
        );
    }

    #[test]
    fn a_request_that_never_reached_the_wire_leaves_the_window_as_it_was() {
        let pool = CreditPool::new();
        pool.set_available(64);

        let mut reservation = pool.try_reserve(8).unwrap();
        let mut header = Header::new_request(crate::types::Command::Echo);
        header.message_id = MessageId(1);
        reservation.stamp(&mut header, 8);
        drop(reservation);
        send(&pool, 2, 1);
        pool.answer(MessageId(2), 1);

        assert_eq!(pool.available(), 64);
        assert_eq!(
            pool.ceiling(),
            Some(64),
            "a refund puts credits back in the pool, not new ones in the window"
        );
        assert!(
            pool.current().window.lock().unwrap().awaiting.is_empty(),
            "an unsent request has no answer coming to settle it"
        );
    }

    /// A request that waited for credits through a revival, then gave up
    /// unsent (a handle refused because its session is gone, a caller
    /// dropped), drops its reservation after `reset`. Its credits were the
    /// dead session's, and its MessageId was counted from that session's
    /// zero, so the same id can be a live request in the new window.
    #[test]
    fn a_reservation_from_before_a_reset_refunds_nothing_into_the_new_session() {
        let pool = CreditPool::new();
        pool.set_available(10);
        let mut stale = pool.try_reserve(4).unwrap();
        let mut header = Header::new_request(crate::types::Command::Read);
        header.message_id = MessageId(0);
        stale.stamp(&mut header, 4);

        pool.close();
        pool.reset();
        pool.grant(2);
        // The new session's first request: MessageId 0 again.
        send(&pool, 0, 1);
        assert_eq!(pool.available(), 1);

        drop(stale);

        assert_eq!(
            pool.available(),
            1,
            "the dead session's credits must stay in its budget, or the new \
             connection can out-spend the server's window"
        );
        assert!(
            pool.current()
                .window
                .lock()
                .unwrap()
                .awaiting
                .contains_key(&0),
            "the new session's request 0 is still in flight; the stale refund \
             must not settle it"
        );
    }

    #[test]
    fn a_reset_forgets_the_old_servers_ceiling() {
        let pool = CreditPool::new();
        pool.set_available(64);
        send(&pool, 1, 1);
        pool.answer(MessageId(1), 1);
        assert_eq!(pool.ceiling(), Some(64));

        pool.reset();

        assert_eq!(pool.ceiling(), None);
    }

    #[test]
    fn a_comfortable_charge_is_half_the_ceiling_and_never_nothing() {
        let pool = CreditPool::new();
        assert_eq!(pool.comfortable_charge(), None);

        pool.current().window.lock().unwrap().ceiling = Some(64);
        assert_eq!(pool.comfortable_charge(), Some(32));

        pool.current().window.lock().unwrap().ceiling = Some(1);
        assert_eq!(pool.comfortable_charge(), Some(1));
    }

    #[test]
    fn capacity_counts_the_ceiling_when_it_is_below_the_target() {
        assert_eq!(capacity_for_charge(10, None), 51);
        assert_eq!(capacity_for_charge(10, Some(64)), 6);
        assert_eq!(capacity_for_charge(10, Some(4096)), 51);
        assert_eq!(capacity_for_charge(130, Some(64)), 1, "never 0");
    }
}
