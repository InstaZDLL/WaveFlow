//! Global shortcuts on a Wayland session, through the XDG desktop portal
//! (`org.freedesktop.portal.GlobalShortcuts`).
//!
//! The protocol is a session: `CreateSession`, then one `BindShortcuts`
//! with the shortcuts and the combination preferred for each; the desktop
//! shows its own dialog, may assign something else, and answers with what
//! it assigned. Presses arrive as `Activated` signals naming the shortcut.
//! A session binds once, so a change closes it and opens a new one.
//!
//! A host application (not Flatpak) must first say who it is through
//! `org.freedesktop.host.portal.Registry.Register`, on the same
//! connection and before any other call: xdg-desktop-portal 1.22 refuses
//! `CreateSession` otherwise with "An app id is required" (measured on
//! GNOME 50). Inside Flatpak the sandbox already names the app.
//!
//! Each call answers through a `Request` object whose path is known in
//! advance (`…/request/<sender>/<token>`). The `Response` signal is
//! subscribed to before the call, or a fast desktop could answer before
//! anyone listens.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::StreamExt;
use tauri::AppHandle;
use zbus::names::InterfaceName;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::GlobalAction;

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// The id a host install registers under: the desktop file the deb, rpm
/// and AUR packages install (`WaveFlow.desktop`), through which the
/// desktop names and shows the app in its dialog.
const HOST_APP_ID: &str = "WaveFlow";

/// How long the desktop's dialog may stay open. Settings shows the
/// shortcut as dismissed after that, rather than waiting forever.
const DIALOG_TIMEOUT: Duration = Duration::from_secs(120);

#[zbus::proxy(
    interface = "org.freedesktop.portal.GlobalShortcuts",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait GlobalShortcutsPortal {
    fn create_session(&self, options: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    fn bind_shortcuts(
        &self,
        session_handle: &ObjectPath<'_>,
        shortcuts: Vec<(&str, HashMap<&str, Value<'_>>)>,
        parent_window: &str,
        options: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<OwnedObjectPath>;

    #[zbus(signal)]
    fn activated(
        &self,
        session_handle: OwnedObjectPath,
        shortcut_id: String,
        timestamp: u64,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.host.portal.Registry",
    default_service = "org.freedesktop.portal.Desktop",
    default_path = "/org/freedesktop/portal/desktop"
)]
trait Registry {
    fn register(&self, app_id: &str, options: HashMap<&str, Value<'_>>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.portal.Request",
    default_service = "org.freedesktop.portal.Desktop"
)]
trait Request {
    #[zbus(signal)]
    fn response(&self, response: u32, results: HashMap<String, OwnedValue>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.portal.Session",
    default_service = "org.freedesktop.portal.Desktop"
)]
trait PortalSession {
    fn close(&self) -> zbus::Result<()>;
}

/// One shortcut to bind.
pub struct Requested {
    pub action: GlobalAction,
    pub description: &'static str,
    pub trigger: String,
}

pub enum Outcome {
    /// Bound; with what the desktop assigned to each shortcut, in its own
    /// words.
    Bound(Session, HashMap<GlobalAction, String>),
    /// The user dismissed the desktop's dialog, or left it open too long.
    Dismissed,
}

/// An open portal session and the task that listens for its presses.
pub struct Session {
    conn: zbus::Connection,
    handle: OwnedObjectPath,
    listener: tauri::async_runtime::JoinHandle<()>,
}

impl Session {
    pub async fn close(self) {
        self.listener.abort();
        close_handle(&self.conn, &self.handle).await;
    }
}

/// Whether the desktop offers the interface at all: GNOME before 48 and
/// several compositors do not.
pub async fn available() -> bool {
    async fn version() -> zbus::Result<OwnedValue> {
        let conn = zbus::Connection::session().await?;
        let props = zbus::fdo::PropertiesProxy::builder(&conn)
            .destination(DESTINATION)?
            .path(PORTAL_PATH)?
            .build()
            .await?;
        Ok(props
            .get(
                InterfaceName::from_static_str_unchecked(INTERFACE),
                "version",
            )
            .await?)
    }
    match version().await {
        Ok(v) => {
            tracing::info!(version = ?v, "global shortcuts: portal available");
            true
        }
        Err(err) => {
            tracing::info!(%err, "global shortcuts: portal not available");
            false
        }
    }
}

/// A token unique to this process, valid as an object path element.
fn token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "waveflow_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The caller's unique bus name as the portal writes it in a path:
/// `:1.42` → `1_42`.
fn sender_element(conn: &zbus::Connection) -> zbus::Result<String> {
    let name = conn
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("no unique name on the session bus".into()))?;
    Ok(name.trim_start_matches(':').replace('.', "_"))
}

/// Subscribe to the `Response` of the request `token` will create.
async fn expect_response(
    conn: &zbus::Connection,
    sender: &str,
    token: &str,
) -> zbus::Result<ResponseStream> {
    let path = format!("{PORTAL_PATH}/request/{sender}/{token}");
    let proxy = RequestProxy::builder(conn).path(path)?.build().await?;
    proxy.receive_response().await
}

/// Wait for a response; `None` when it never came.
async fn await_response(
    mut stream: ResponseStream,
) -> zbus::Result<Option<(u32, HashMap<String, OwnedValue>)>> {
    match tokio::time::timeout(DIALOG_TIMEOUT, stream.next()).await {
        Ok(Some(signal)) => {
            let args = signal.args()?;
            Ok(Some((args.response, args.results)))
        }
        Ok(None) | Err(_) => Ok(None),
    }
}

/// Open a session and bind `shortcuts` in it.
pub async fn bind(app: AppHandle, shortcuts: Vec<Requested>) -> Result<Outcome, String> {
    bind_inner(app, shortcuts)
        .await
        .map_err(|err| err.to_string())
}

async fn bind_inner(app: AppHandle, shortcuts: Vec<Requested>) -> zbus::Result<Outcome> {
    let conn = zbus::Connection::session().await?;
    if std::env::var_os("FLATPAK_ID").is_none() {
        // Best effort: a portal older than 1.19 has no registry and then
        // needs no registration either.
        match RegistryProxy::new(&conn).await {
            Ok(registry) => {
                if let Err(err) = registry.register(HOST_APP_ID, HashMap::new()).await {
                    tracing::debug!(%err, "global shortcuts: portal registry refused the app id");
                }
            }
            Err(err) => tracing::debug!(%err, "global shortcuts: no portal registry"),
        }
    }
    let sender = sender_element(&conn)?;
    let portal = GlobalShortcutsPortalProxy::new(&conn).await?;

    // 1. The session.
    let request_token = token();
    let session_token = token();
    let responses = expect_response(&conn, &sender, &request_token).await?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(request_token.as_str()));
    options.insert("session_handle_token", Value::from(session_token.as_str()));
    portal.create_session(options).await?;
    let handle = match await_response(responses).await? {
        Some((0, results)) => results
            .get("session_handle")
            .and_then(|v| {
                String::try_from(v.clone())
                    .ok()
                    .and_then(|s| OwnedObjectPath::try_from(s).ok())
                    .or_else(|| OwnedObjectPath::try_from(v.clone()).ok())
            })
            .map(Ok)
            .unwrap_or_else(|| {
                OwnedObjectPath::try_from(format!("{PORTAL_PATH}/session/{sender}/{session_token}"))
            })?,
        Some((code, _)) => {
            return Err(zbus::Error::Failure(format!(
                "CreateSession answered {code}"
            )))
        }
        None => return Ok(Outcome::Dismissed),
    };

    // 2. The shortcuts. Listening starts before the bind, on this session
    // only: `Activated` is broadcast to every session of the connection.
    let mut presses = portal.receive_activated().await?;
    let request_token = token();
    let responses = expect_response(&conn, &sender, &request_token).await?;
    let list: Vec<(&str, HashMap<&str, Value<'_>>)> = shortcuts
        .iter()
        .map(|s| {
            let mut props: HashMap<&str, Value<'_>> = HashMap::new();
            props.insert("description", Value::from(s.description));
            props.insert("preferred_trigger", Value::from(s.trigger.as_str()));
            (s.action.id(), props)
        })
        .collect();
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(request_token.as_str()));
    portal
        .bind_shortcuts(&handle.as_ref(), list, "", options)
        .await?;
    let results = match await_response(responses).await? {
        Some((0, results)) => results,
        Some((1, _)) | None => {
            close_handle(&conn, &handle).await;
            return Ok(Outcome::Dismissed);
        }
        Some((code, _)) => {
            close_handle(&conn, &handle).await;
            return Err(zbus::Error::Failure(format!(
                "BindShortcuts answered {code}"
            )));
        }
    };
    let assigned = assigned_triggers(results.get("shortcuts"));

    let session_handle = handle.clone();
    let listener = tauri::async_runtime::spawn(async move {
        while let Some(signal) = presses.next().await {
            let Ok(args) = signal.args() else { continue };
            if args.session_handle != session_handle {
                continue;
            }
            if let Some(action) = GlobalAction::from_id(&args.shortcut_id) {
                super::dispatch(&app, action);
            }
        }
    });
    Ok(Outcome::Bound(
        Session {
            conn,
            handle,
            listener,
        },
        assigned,
    ))
}

async fn close_handle(conn: &zbus::Connection, handle: &OwnedObjectPath) {
    if let Ok(builder) = PortalSessionProxy::builder(conn).path(handle.clone()) {
        if let Ok(proxy) = builder.build().await {
            let _ = proxy.close().await;
        }
    }
}

/// Read `shortcuts` (`a(sa{sv})`) from a `BindShortcuts` response: each
/// id with its `trigger_description`.
fn assigned_triggers(value: Option<&OwnedValue>) -> HashMap<GlobalAction, String> {
    let Some(value) = value else {
        return HashMap::new();
    };
    let Ok(list) = <Vec<(String, HashMap<String, OwnedValue>)>>::try_from(value.clone()) else {
        return HashMap::new();
    };
    list.into_iter()
        .filter_map(|(id, props)| {
            let action = GlobalAction::from_id(&id)?;
            let trigger = props
                .get("trigger_description")
                .and_then(|v| String::try_from(v.clone()).ok())
                .filter(|t| !t.is_empty())?;
            Some((action, trigger))
        })
        .collect()
}
