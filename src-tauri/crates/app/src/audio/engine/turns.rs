//! Whose turn it is to publish a load, as a trait.
//!
//! An [`AudioEngine`] cannot be built without a running app and an audio device,
//! and the rules that decide who may publish, and what a loser leaves behind, are
//! worth testing on their own. A producer written against this runs on the real
//! engine in the app and on [`TestEngine`] in a test.

use std::future::Future;

use super::{AudioCmd, AudioEngine, LoadIntent};

/// What a producer asks of the engine: the publish lock, the claim on the same
/// mark the decoder reads, and the one command that ends what is playing.
pub(crate) trait Turns {
    fn lock_publish(&self) -> impl Future<Output = tokio::sync::MutexGuard<'_, ()>> + Send;
    fn claim_dispatch(&self, intent: LoadIntent) -> bool;
    fn load_intent_superseded(&self, intent: LoadIntent) -> bool;
    fn stop(&self);
}

impl Turns for AudioEngine {
    fn lock_publish(&self) -> impl Future<Output = tokio::sync::MutexGuard<'_, ()>> + Send {
        AudioEngine::lock_publish(self)
    }

    fn claim_dispatch(&self, intent: LoadIntent) -> bool {
        AudioEngine::claim_dispatch(self, intent)
    }

    fn load_intent_superseded(&self, intent: LoadIntent) -> bool {
        AudioEngine::load_intent_superseded(self, intent)
    }

    fn stop(&self) {
        let _ = self.send(AudioCmd::Stop);
    }
}

/// The engine's arbitration as it really is, on the real high-water mark, with
/// the one command these rules are about counted instead of sent.
#[cfg(test)]
pub(crate) struct TestEngine {
    pub(crate) shared: crate::audio::state::SharedPlayback,
    pub(crate) publish: tokio::sync::Mutex<()>,
    stops: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl TestEngine {
    pub(crate) fn new() -> Self {
        Self {
            shared: crate::audio::state::SharedPlayback::new(),
            publish: tokio::sync::Mutex::new(()),
            stops: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn stops(&self) -> usize {
        self.stops.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// What another producer does before it publishes anything.
    pub(crate) fn claims(&self, intent: LoadIntent) {
        assert!(self.shared.try_claim_load(intent.get()));
    }

    /// The mark as the decoder would read it.
    pub(crate) fn newest_claim(&self) -> u64 {
        self.shared
            .newest_load_intent
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
impl Turns for TestEngine {
    async fn lock_publish(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.publish.lock().await
    }

    fn claim_dispatch(&self, intent: LoadIntent) -> bool {
        self.shared.try_claim_load(intent.get())
    }

    fn load_intent_superseded(&self, intent: LoadIntent) -> bool {
        intent.get() < self.newest_claim()
    }

    fn stop(&self) {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
