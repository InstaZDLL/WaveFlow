//! Rust-only implementation of the playback host contract.
//!
//! State access is typed and limited to the two playback services. The engine
//! reference is weak to avoid an engine -> host -> engine ownership cycle.
//! Events retain the existing names and payloads; consumers subscribe before
//! starting the engine and must resnapshot after a broadcast lag.
use crate::{
    audio::{AudioEngine, PlayerState},
    state::AppState,
};
use serde::Serialize;
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Event {
    pub name: String,
    pub payload: serde_json::Value,
}

struct Host {
    state: Arc<AppState>,
    engine: OnceLock<Weak<Arc<AudioEngine>>>,
    events: broadcast::Sender<Event>,
    runtime: tokio::runtime::Handle,
}

#[derive(Clone)]
pub struct AppHandle(Arc<Host>);

impl AppHandle {
    pub fn new(state: Arc<AppState>) -> Self {
        let (events, _) = broadcast::channel(128);
        Self(Arc::new(Host {
            state,
            engine: OnceLock::new(),
            events,
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    pub fn attach_engine(&self, engine: &Arc<Arc<AudioEngine>>) {
        self.0
            .engine
            .set(Arc::downgrade(engine))
            .expect("one audio engine per host");
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.0.events.subscribe()
    }
}

pub trait HostService: Send + Sync + Sized + 'static {
    fn lookup(host: &AppHandle) -> Option<Arc<Self>>;
}

impl HostService for AppState {
    fn lookup(host: &AppHandle) -> Option<Arc<Self>> {
        Some(host.0.state.clone())
    }
}

impl HostService for Arc<AudioEngine> {
    fn lookup(host: &AppHandle) -> Option<Arc<Self>> {
        host.0.engine.get().and_then(Weak::upgrade)
    }
}

pub trait Manager {
    fn try_state<T: HostService>(&self) -> Option<Arc<T>>;
    fn state<T: HostService>(&self) -> Arc<T> {
        self.try_state().expect("playback service initialized")
    }
}

impl Manager for AppHandle {
    fn try_state<T: HostService>(&self) -> Option<Arc<T>> {
        T::lookup(self)
    }
}

pub trait Emitter {
    fn emit<T: Serialize + Clone>(&self, name: &str, payload: T) -> Result<(), serde_json::Error>;
}

impl Emitter for AppHandle {
    fn emit<T: Serialize + Clone>(&self, name: &str, payload: T) -> Result<(), serde_json::Error> {
        let event = Event {
            name: name.to_owned(),
            payload: serde_json::to_value(payload)?,
        };
        let _ = self.0.events.send(event);
        Ok(())
    }
}

pub fn spawn<F>(app: AppHandle, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    app.0.runtime.spawn(future)
}

pub fn update_playback(_: &AppHandle, _: PlayerState, _: u64, _: bool) {
    // GTK consumes player:state, emitted immediately before this hook.
    // MPRIS and Discord integrations remain in the Tauri host for now.
}
