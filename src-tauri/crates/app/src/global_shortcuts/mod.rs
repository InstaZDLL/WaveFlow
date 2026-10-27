//! System-wide shortcuts: play / pause, next, previous, like and volume
//! from any application.
//!
//! The media keys already reach the player through the OS media controls
//! (`media_controls`); these are for keyboards without them, and for the
//! actions the media keys do not have. None is bound by default: a
//! combination registered system-wide is taken from every other
//! application, so the user picks each one.
//!
//! Three backends, chosen once at launch ([`initial_backend`]):
//!
//! - **native** — `tauri-plugin-global-shortcut`: `RegisterHotKey` on
//!   Windows, Carbon hot keys on macOS (which need no accessibility
//!   permission), an X11 key grab on Linux. A combination another
//!   application holds is refused and reported as taken.
//! - **portal** — a Wayland session has no global grab: an X11 grab there
//!   only sees keys while an X11 window has the focus, and the plugin
//!   "registers" without error and never fires. The XDG GlobalShortcuts
//!   portal ([`portal`]) is the way in; the desktop confirms each change in
//!   a dialog of its own and may assign a different combination, which is
//!   shown as it assigned it.
//! - **unavailable** — Wayland without that portal (or no display at all).
//!   Said so in Settings rather than accepting combinations that would
//!   never fire.
//!
//! Every action goes through [`crate::player_actions`], like the tray and
//! the media keys.
//!
//! The bindings are an `app_setting` row, not a `profile_setting` one: a
//! global shortcut is a claim the process makes on the whole desktop,
//! registered at launch before any window, and it would be wrong for a
//! profile switch to drop combinations the user set for their keyboard —
//! or to take new ones from other applications behind their back.

mod combo;
#[cfg(target_os = "linux")]
mod portal;

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

pub use combo::{Combo, ComboError};

use crate::{error::AppResult, player_actions, state::AppState};

/// `app_setting` key: a JSON object of action id → stored combination.
pub const SETTING_KEY: &str = "shortcuts.global";

/// Tags the log lines of every action a shortcut runs.
const LABEL: &str = "global shortcut";

/// Volume step per press, the in-app arrow keys' `VOLUME_STEP`.
const VOLUME_STEP: i32 = 5;

/// What a global shortcut can do. The ids are the in-app shortcuts' own
/// (`src/lib/shortcuts.ts`), so Settings names them with the same labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GlobalAction {
    TogglePlayback,
    Next,
    Previous,
    ToggleLike,
    VolumeUp,
    VolumeDown,
}

impl GlobalAction {
    pub const ALL: [Self; 6] = [
        Self::TogglePlayback,
        Self::Next,
        Self::Previous,
        Self::ToggleLike,
        Self::VolumeUp,
        Self::VolumeDown,
    ];

    /// The serialized id, also the portal's shortcut id.
    pub fn id(self) -> &'static str {
        match self {
            Self::TogglePlayback => "togglePlayback",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::ToggleLike => "toggleLike",
            Self::VolumeUp => "volumeUp",
            Self::VolumeDown => "volumeDown",
        }
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.id() == id)
    }

    /// What the desktop's own shortcut settings list under WaveFlow on a
    /// Wayland session. English: the portal binds at launch, before the
    /// interface has a language.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn description(self) -> &'static str {
        match self {
            Self::TogglePlayback => "Play / pause",
            Self::Next => "Next track",
            Self::Previous => "Previous track",
            Self::ToggleLike => "Like the current track",
            Self::VolumeUp => "Volume up",
            Self::VolumeDown => "Volume down",
        }
    }
}

/// Run `action`. Called from the hot-key thread (the main thread on
/// Windows and macOS), so anything that awaits is spawned.
pub fn dispatch(app: &AppHandle, action: GlobalAction) {
    if action == GlobalAction::TogglePlayback {
        player_actions::toggle_play_pause(app, LABEL);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match action {
            GlobalAction::Next => {
                player_actions::next(&app, LABEL).await;
            }
            GlobalAction::Previous => {
                player_actions::previous(&app, LABEL).await;
            }
            GlobalAction::ToggleLike => player_actions::toggle_like_current(&app, LABEL).await,
            GlobalAction::VolumeUp => player_actions::nudge_volume(&app, VOLUME_STEP, LABEL).await,
            GlobalAction::VolumeDown => {
                player_actions::nudge_volume(&app, -VOLUME_STEP, LABEL).await
            }
            GlobalAction::TogglePlayback => unreachable!("handled above"),
        }
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Backend {
    Native,
    /// Linux only; part of the type everywhere so the frontend reads one
    /// shape.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Portal,
    Unavailable,
}

/// Where one binding stands, for its Settings row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum BindingState {
    /// Registered: it fires from any application.
    Active,
    /// Another application holds this combination.
    Taken,
    /// The system refused it for another reason (a combination it
    /// reserves, or a macOS refusal, which does not say which).
    Refused { message: String },
    /// Not a combination this accepts.
    Invalid { reason: ComboError },
    /// The desktop's dialog was dismissed, or never answered (portal).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Dismissed,
    /// Set aside while Settings records a new combination.
    Suspended,
    /// No backend on this session.
    Unavailable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BindingView {
    pub action: GlobalAction,
    pub combo: String,
    pub state: BindingState,
    /// What the desktop assigned through the portal, in its own words —
    /// it may differ from the combination asked for.
    pub trigger: Option<String>,
}

/// A combination that was asked for and not kept, with why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rejected {
    pub action: GlobalAction,
    pub combo: String,
    pub state: BindingState,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub backend: Backend,
    pub bindings: Vec<BindingView>,
    pub rejected: Option<Rejected>,
}

/// The backend this launch can use, decided before the plugin is added:
/// the native one must not start on a Wayland session, where it would
/// accept every combination and deliver none.
pub fn initial_backend() -> Backend {
    #[cfg(target_os = "linux")]
    {
        if crate::display_backend::wayland_session() {
            // Confirmed or ruled out once the portal answers (`init`).
            return Backend::Portal;
        }
        if std::env::var_os("DISPLAY").is_none() {
            return Backend::Unavailable;
        }
    }
    Backend::Native
}

struct Inner {
    backend: Backend,
    /// What is stored: the canonical combination per action, or the raw
    /// value when it no longer parses (shown as invalid, not dropped).
    bindings: BTreeMap<GlobalAction, String>,
    states: HashMap<GlobalAction, BindingState>,
    triggers: HashMap<GlobalAction, String>,
    suspended: bool,
    #[cfg(target_os = "linux")]
    portal: Option<portal::Session>,
}

/// Managed state: the bindings and where each one stands.
pub struct GlobalShortcuts {
    inner: tokio::sync::Mutex<Inner>,
}

impl GlobalShortcuts {
    pub fn new(backend: Backend) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(Inner {
                backend,
                bindings: BTreeMap::new(),
                states: HashMap::new(),
                triggers: HashMap::new(),
                suspended: false,
                #[cfg(target_os = "linux")]
                portal: None,
            }),
        }
    }
}

/// Read the stored bindings and register them. Spawned from `setup`: the
/// native backend runs each registration on the main thread, which is
/// busy until `setup` returns.
pub fn init(app: &AppHandle, backend: Backend) {
    app.manage(GlobalShortcuts::new(backend));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let stored = load(&app.state::<AppState>().app_db).await;
        let shortcuts = app.state::<GlobalShortcuts>();
        let mut inner = shortcuts.inner.lock().await;
        inner.bindings = stored;
        #[cfg(target_os = "linux")]
        if inner.backend == Backend::Portal && !portal::available().await {
            tracing::info!("global shortcuts: Wayland session without the GlobalShortcuts portal");
            inner.backend = Backend::Unavailable;
        }
        if inner.bindings.is_empty() {
            return;
        }
        apply_all(&app, &mut inner).await;
    });
}

/// Parse a stored row. Unknown action ids are skipped (a newer build's);
/// a value that no longer parses is kept, to be shown as invalid.
fn parse_stored(raw: &str) -> BTreeMap<GlobalAction, String> {
    let map: HashMap<String, String> = serde_json::from_str(raw).unwrap_or_default();
    map.into_iter()
        .filter_map(|(id, combo)| {
            let action = GlobalAction::ALL.into_iter().find(|a| a.id() == id)?;
            let combo = Combo::parse(&combo).map(|c| c.canonical()).unwrap_or(combo);
            (!combo.is_empty()).then_some((action, combo))
        })
        .collect()
}

async fn load(app_db: &SqlitePool) -> BTreeMap<GlobalAction, String> {
    let raw = sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = ?")
        .bind(SETTING_KEY)
        .fetch_optional(app_db)
        .await
        .ok()
        .flatten();
    raw.as_deref().map(parse_stored).unwrap_or_default()
}

async fn save(app_db: &SqlitePool, bindings: &BTreeMap<GlobalAction, String>) -> AppResult<()> {
    let object: BTreeMap<&str, &str> = bindings
        .iter()
        .map(|(action, combo)| (action.id(), combo.as_str()))
        .collect();
    sqlx::query(
        "INSERT INTO app_setting (key, value, value_type, updated_at)
         VALUES (?, ?, 'json', ?)
         ON CONFLICT(key) DO UPDATE
            SET value = excluded.value, updated_at = excluded.updated_at",
    )
    .bind(SETTING_KEY)
    .bind(serde_json::to_string(&object).unwrap_or_else(|_| "{}".into()))
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(app_db)
    .await?;
    Ok(())
}

/// The plugin flattens the crate's errors to their text; "taken" is the
/// one this tells apart, by the wording `global_hotkey::Error`'s
/// `AlreadyRegistered` displays (pinned by a test below).
fn classify(err: &tauri_plugin_global_shortcut::Error) -> BindingState {
    let message = err.to_string();
    if message.starts_with("HotKey already registered") {
        BindingState::Taken
    } else {
        BindingState::Refused { message }
    }
}

/// Register one combination natively. Blocking: the plugin runs it on the
/// main thread and waits, so this goes to the blocking pool.
async fn native_register(app: &AppHandle, action: GlobalAction, combo: Combo) -> BindingState {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result =
            app.global_shortcut()
                .on_shortcut(combo.shortcut(), move |app, _shortcut, event| {
                    // One press, one action: `RegisterHotKey` is asked not to
                    // repeat, and a release is not a press.
                    if event.state == ShortcutState::Pressed {
                        dispatch(app, action);
                    }
                });
        match result {
            Ok(()) => BindingState::Active,
            Err(err) => classify(&err),
        }
    })
    .await
    .unwrap_or_else(|err| BindingState::Refused {
        message: err.to_string(),
    })
}

async fn native_unregister(app: &AppHandle, combo: Combo) {
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        if let Err(err) = app.global_shortcut().unregister(combo.shortcut()) {
            tracing::debug!(%err, "global shortcut: unregister failed");
        }
    })
    .await;
}

/// Unregister every binding this process holds natively.
async fn native_release_all(app: &AppHandle, inner: &mut Inner) {
    for (action, raw) in &inner.bindings {
        if inner.states.get(action) == Some(&BindingState::Active) {
            if let Ok(combo) = Combo::parse(raw) {
                native_unregister(app, combo).await;
            }
        }
    }
}

/// (Re)apply every stored binding through the current backend and
/// record where each one stands.
async fn apply_all(app: &AppHandle, inner: &mut Inner) {
    inner.states.clear();
    inner.triggers.clear();
    let parsed: Vec<(GlobalAction, Result<Combo, ComboError>)> = inner
        .bindings
        .iter()
        .map(|(action, raw)| (*action, Combo::parse(raw)))
        .collect();
    for (action, combo) in &parsed {
        if let Err(reason) = combo {
            inner
                .states
                .insert(*action, BindingState::Invalid { reason: *reason });
        }
    }
    let valid: Vec<(GlobalAction, Combo)> = parsed
        .into_iter()
        .filter_map(|(action, combo)| combo.ok().map(|c| (action, c)))
        .collect();
    match inner.backend {
        Backend::Native => {
            for (action, combo) in valid {
                let state = native_register(app, action, combo).await;
                if state != BindingState::Active {
                    tracing::info!(action = action.id(), combo = %combo.canonical(), ?state, "global shortcut not registered");
                }
                inner.states.insert(action, state);
            }
        }
        Backend::Unavailable => {
            for (action, _) in valid {
                inner.states.insert(action, BindingState::Unavailable);
            }
        }
        #[cfg(target_os = "linux")]
        Backend::Portal => {
            if valid.is_empty() {
                if let Some(session) = inner.portal.take() {
                    session.close().await;
                }
                return;
            }
            let requested: Vec<portal::Requested> = valid
                .iter()
                .map(|(action, combo)| portal::Requested {
                    action: *action,
                    description: action.description(),
                    trigger: combo.portal_trigger(),
                })
                .collect();
            match portal::bind(app.clone(), requested).await {
                // The session in place stays open until the new one is
                // bound: a dismissed dialog must not take the shortcuts
                // that already worked down with it (`set` then puts the
                // previous set back).
                Ok(portal::Outcome::Bound(session, triggers)) => {
                    for (action, _) in &valid {
                        inner.states.insert(*action, BindingState::Active);
                        if let Some(trigger) = triggers.get(action) {
                            inner.triggers.insert(*action, trigger.clone());
                        }
                    }
                    if let Some(previous) = inner.portal.replace(session) {
                        previous.close().await;
                    }
                }
                Ok(portal::Outcome::Dismissed) => {
                    for (action, _) in &valid {
                        inner.states.insert(*action, BindingState::Dismissed);
                    }
                }
                Err(message) => {
                    tracing::warn!(%message, "global shortcuts: portal bind failed");
                    for (action, _) in &valid {
                        inner.states.insert(
                            *action,
                            BindingState::Refused {
                                message: message.clone(),
                            },
                        );
                    }
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        Backend::Portal => unreachable!("the portal backend is Linux-only"),
    }
}

fn snapshot(inner: &Inner, rejected: Option<Rejected>) -> Snapshot {
    let bindings = inner
        .bindings
        .iter()
        .map(|(action, combo)| BindingView {
            action: *action,
            combo: combo.clone(),
            state: if inner.suspended && inner.backend == Backend::Native {
                BindingState::Suspended
            } else {
                inner
                    .states
                    .get(action)
                    .cloned()
                    .unwrap_or(BindingState::Unavailable)
            },
            trigger: inner.triggers.get(action).cloned(),
        })
        .collect();
    Snapshot {
        backend: inner.backend,
        bindings,
        rejected,
    }
}

impl GlobalShortcuts {
    pub async fn snapshot(&self) -> Snapshot {
        snapshot(&*self.inner.lock().await, None)
    }

    /// Bind `action` to `combo`, or unbind it with `None`.
    ///
    /// A combination another action holds moves to this one, as in the
    /// in-app shortcuts card. A combination the system refuses — or, on
    /// Wayland, one whose dialog the user dismissed — is not stored: the
    /// previous set stays and the refusal comes back in `rejected`, so the
    /// row can say "taken by another application" next to the
    /// combination that was tried. A recording
    /// in progress ends here — the new set is registered at once, which is
    /// what finds a conflict.
    pub async fn set(
        &self,
        app: &AppHandle,
        app_db: &SqlitePool,
        action: GlobalAction,
        combo: Option<&str>,
    ) -> AppResult<Snapshot> {
        let mut inner = self.inner.lock().await;
        let mut next = inner.bindings.clone();
        let mut rejected = None;
        match combo.map(str::trim).filter(|c| !c.is_empty()) {
            None => {
                next.remove(&action);
            }
            Some(raw) => match Combo::parse(raw) {
                Err(reason) => {
                    rejected = Some(Rejected {
                        action,
                        combo: raw.to_string(),
                        state: BindingState::Invalid { reason },
                    });
                }
                Ok(parsed) => {
                    let canonical = parsed.canonical();
                    next.retain(|other, c| *other == action || *c != canonical);
                    next.insert(action, canonical);
                }
            },
        }
        if rejected.is_none() && inner.backend == Backend::Unavailable {
            if let Some(raw) = next.get(&action).filter(|_| combo.is_some()) {
                rejected = Some(Rejected {
                    action,
                    combo: raw.clone(),
                    state: BindingState::Unavailable,
                });
            }
        }
        if rejected.is_some() {
            // Nothing changes; a recording still ends.
            if inner.suspended {
                inner.suspended = false;
                apply_all(app, &mut inner).await;
            }
            return Ok(snapshot(&inner, rejected));
        }

        if inner.backend == Backend::Native && !inner.suspended {
            native_release_all(app, &mut inner).await;
        }
        let previous = std::mem::replace(&mut inner.bindings, next);
        let previous_states = inner.states.clone();
        let previous_triggers = inner.triggers.clone();
        inner.suspended = false;
        apply_all(app, &mut inner).await;

        // A combination that was not taken up is not kept: put the
        // previous set back and say why. Natively it is registered again;
        // through the portal the previous session is still the one open,
        // so only what is shown goes back.
        let refused = inner
            .states
            .get(&action)
            .filter(|s| {
                matches!(
                    s,
                    BindingState::Taken | BindingState::Refused { .. } | BindingState::Dismissed
                )
            })
            .cloned();
        if let Some(state) = refused {
            let tried = inner.bindings.get(&action).cloned().unwrap_or_default();
            if inner.backend == Backend::Native {
                native_release_all(app, &mut inner).await;
                inner.bindings = previous;
                apply_all(app, &mut inner).await;
            } else {
                inner.bindings = previous;
                inner.states = previous_states;
                inner.triggers = previous_triggers;
            }
            rejected = Some(Rejected {
                action,
                combo: tried,
                state,
            });
            return Ok(snapshot(&inner, rejected));
        }
        if let Err(err) = save(app_db, &inner.bindings).await {
            // Not stored, so not kept either: what fires must be what the
            // next launch registers. Natively the previous set goes back
            // at once; a portal session cannot be rebound without the
            // desktop's dialog, so there the next launch restores it.
            if inner.backend == Backend::Native {
                native_release_all(app, &mut inner).await;
                inner.bindings = previous;
                apply_all(app, &mut inner).await;
            }
            return Err(err);
        }
        Ok(snapshot(&inner, None))
    }

    /// Set every native registration aside while Settings records a
    /// combination, so pressing one WaveFlow already holds reaches the
    /// page instead of running its action. `false` puts them back.
    /// Nothing to do through the portal: those keys belong to the desktop.
    pub async fn suspend(&self, app: &AppHandle, suspended: bool) -> Snapshot {
        let mut inner = self.inner.lock().await;
        if inner.backend == Backend::Native && inner.suspended != suspended {
            if suspended {
                native_release_all(app, &mut inner).await;
                inner.suspended = true;
            } else {
                inner.suspended = false;
                apply_all(app, &mut inner).await;
            }
        }
        snapshot(&inner, None)
    }

    /// Unbind everything (the Settings reset).
    pub async fn clear(&self, app: &AppHandle, app_db: &SqlitePool) -> AppResult<Snapshot> {
        let mut inner = self.inner.lock().await;
        if inner.backend == Backend::Native && !inner.suspended {
            native_release_all(app, &mut inner).await;
        }
        inner.bindings.clear();
        inner.suspended = false;
        apply_all(app, &mut inner).await;
        save(app_db, &inner.bindings).await?;
        Ok(snapshot(&inner, None))
    }
}

#[cfg(test)]
mod tests;
